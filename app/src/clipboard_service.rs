//! ClipboardService — склейка клипборд-стека (Фаза 3, шаги 3/6; план §1 D4–D7):
//! - **запись истории:** поток-воркер читает события listener'а (iskra-sys:
//!   message-only окно, D1), читает форматы (D2), фильтрует исключения (D7),
//!   пишет через ClipboardStore (дедуп-подъём D5, лимиты 1000/200 МБ) и
//!   эмитит `clipboard://updated` (payload — затронутый ClipboardEntry);
//! - **paste(id):** цепочка D6 целиком — GetForegroundWindow → скрыть лончер →
//!   контент в клипборд (iskra-sys set_*) → SetForegroundWindow(prev) → пауза
//!   ~80 мс → повторный restore при несовпадении (риск 4) → SendInput Ctrl+V;
//! - **автопастер сниппетов (D9):** CopyText внутри run_item кладёт тело в
//!   клипборд, затем `paste_just_copied` возвращает фокус и шлёт Ctrl+V;
//! - **настройки на лету (D7):** `clipboard_enabled`/`clipboard_excluded_apps`
//!   применяются без рестарта — listener жив, события игнорируются;
//! - **--clipboard-seed N (шаг 6):** синтетическое наполнение истории для
//!   приёмки лимита 1000 записей (тексты + 3 PNG-заглушки), ранний exit.
//!
//! Изображения (D4): истина — PNG на диске; раскладку делает этот сервис —
//! файлы кладутся как `clipboard/{content_hash}.png` + thumbnail 128px
//! `{content_hash}_thumb.png` рядом. Хэш-имя вместо `{id}.png`: контент
//! известен ДО add() (id — только после), застейдженный файл при дедуп-подъёме
//! совпадает с путём существующей записи и не требует удаления; нет гонки
//! next-id с paste-бампом из потока команд (отклонение от комментария схемы
//! V3 зафиксировано в отчёте шага 3; репозиторию формат пути безразличен).
//!
//! Изображение > MAX_IMAGE_BYTES — skip+лог ДО записи на диск (риск 6).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

use iskra_core::clipboard::{
    AddOutcome, ClipboardEntry, ClipboardError, ClipboardKind, ClipboardNew, ClipboardStore,
    MAX_IMAGE_BYTES, content_hash64,
};
use iskra_core::logging;
use iskra_core::search::db::Db;
use iskra_core::snippets::SnippetStore;
use iskra_core::{Settings, Snippet, EVENT_CLIPBOARD_UPDATED};
use iskra_sys::clipboard::{ClipboardContent, ClipboardEvent, ClipboardListener};

/// Сколько записей отдаёт `clipboard_list` (MVP: топ-100 без пагинации, D8/риск 8).
pub const LIST_LIMIT: usize = 100;
/// Размер thumbnail'а изображений в списке (D4).
const THUMB_PX: u32 = 128;
/// Окно подавления событий после set_* при вставке (мс): собственный set не
/// должен повторно попадать в историю (для изображений рэнкод PNG дал бы дубль).
const PASTE_SUPPRESS_MS: u64 = 700;
/// Пауза restore → SendInput (D6/риск 4).
const PASTE_SETTLE: Duration = Duration::from_millis(80);
/// Пауза после повторного restore (риск 4).
const PASTE_RESTORE_RETRY: Duration = Duration::from_millis(40);

pub type Result<T> = std::result::Result<T, ClipboardError>;

/// Флаги мониторинга, применяемые на лету (D7). Общие для воркера и команд.
struct MonitorConfig {
    enabled: AtomicBool,
    excluded: Mutex<Vec<String>>,
    /// unix-мс: до этого момента события клипборда пропускаются (окно вставки).
    suppress_until_ms: AtomicU64,
}

impl MonitorConfig {
    fn new() -> Self {
        MonitorConfig {
            enabled: AtomicBool::new(true),
            excluded: Mutex::new(Vec::new()),
            suppress_until_ms: AtomicU64::new(0),
        }
    }

    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Владелец в списке исключений? (вход уже lowercase от iskra-sys). Записи
    /// хранятся нормализованными (trim + lowercase); совпадение — точное или без
    /// суффикса «.exe» (пользователь может написать и «notepad», и «notepad.exe»).
    fn is_excluded(&self, owner: &str) -> bool {
        let owner = owner.to_lowercase();
        let bare = owner.strip_suffix(".exe").unwrap_or(&owner);
        self.excluded
            .lock()
            .expect("monitor excluded poisoned")
            .iter()
            .any(|a| {
                let a = a.as_str();
                a == owner || a.strip_suffix(".exe").unwrap_or(a) == bare
            })
    }

    fn suppress_until(&self, until_ms: u64) {
        self.suppress_until_ms.store(until_ms, Ordering::Release);
    }

    fn is_suppressed(&self, now_ms: u64) -> bool {
        now_ms < self.suppress_until_ms.load(Ordering::Acquire)
    }

    /// Применить настройки на лету (listener не перезапускается — D7).
    fn apply(&self, settings: &Settings) {
        self.enabled.store(settings.clipboard_enabled, Ordering::Relaxed);
        let normalized: Vec<String> = settings
            .clipboard_excluded_apps
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        *self.excluded.lock().expect("monitor excluded poisoned") = normalized;
    }
}

/// Сервис: хранилище истории + сниппеты + флаги мониторинга. Методы `&self`
/// (внутри мьютексы БД) — безопасно дергать из воркера и команд параллельно.
/// Жизнью БД управляют репозитории (их Arc<Db>).
pub struct ClipboardService {
    store: ClipboardStore,
    snippets: SnippetStore,
    config: Arc<MonitorConfig>,
    images_dir: PathBuf,
}

impl ClipboardService {
    /// Полный конструктор (прод и тесты): БД + каталог изображений.
    pub fn open(db: Arc<Db>, images_dir: PathBuf) -> Self {
        ClipboardService {
            store: ClipboardStore::new(db.clone()),
            snippets: SnippetStore::new(db),
            config: Arc::new(MonitorConfig::new()),
            images_dir,
        }
    }

    /// Прод-старт: БД в %APPDATA%\iskra, поток listener'а (D1) + поток-воркер.
    /// Воркер переживает apply_settings на лету; сбой listener'а не роняет
    /// приложение — вставка/сниппеты работают, запись истории деградирует (риск 9).
    pub fn spawn(app: AppHandle, settings: &Settings) -> Arc<ClipboardService> {
        let data_dir = logging::base_dir();
        let db = Arc::new(
            Db::open(&data_dir.join("index.db")).expect("clipboard: не открыть index.db"),
        );
        let svc = Arc::new(ClipboardService::open(db, data_dir.join("clipboard")));
        svc.config.apply(settings);
        logging::info(&format!(
            "clipboard: старт (enabled={}, исключения={:?})",
            settings.clipboard_enabled, settings.clipboard_excluded_apps
        ));

        match iskra_sys::clipboard::start_listener() {
            Ok(listener) => {
                let worker_svc = svc.clone();
                let worker_app = app.clone();
                let spawned = std::thread::Builder::new()
                    .name("clipboard-svc".into())
                    .spawn(move || worker_loop(listener, worker_svc, worker_app));
                match spawned {
                    Ok(_) => {}
                    Err(e) => logging::warn(&format!("clipboard: воркер не запущен: {e}")),
                }
            }
            Err(e) => logging::warn(&format!(
                "clipboard: listener НЕ запущен: {e} — история не пишется (вставка доступна)"
            )),
        }
        svc
    }

    /// Настройки мониторинга на лету (D7): вызывается из update_settings.
    pub fn apply_settings(&self, settings: &Settings) {
        self.config.apply(settings);
    }

    // --- команды: история ---

    /// `clipboard_list(query)`: pinned сверху, далее used_at DESC, топ-100.
    pub fn list(&self, query: Option<&str>) -> Result<Vec<ClipboardEntry>> {
        self.store.list(query, LIST_LIMIT)
    }

    pub fn get(&self, id: i64) -> Result<Option<ClipboardEntry>> {
        self.store.get(id)
    }

    /// `clipboard_delete(id)`: строка + PNG/thumbnail с диска (D4).
    pub fn delete(&self, id: i64) -> Result<bool> {
        self.store.delete(id)
    }

    /// `clipboard_pinned(id, pinned)` (D8). used_at не трогаем.
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<bool> {
        self.store.set_pinned(id, pinned)
    }

    // --- команды: сниппеты (D9) ---

    pub fn snippets_list(&self) -> std::result::Result<Vec<Snippet>, iskra_core::snippets::SnippetError> {
        self.snippets.list()
    }

    pub fn snippet_create(
        &self,
        name: &str,
        body: &str,
        keywords: &str,
    ) -> std::result::Result<Snippet, iskra_core::snippets::SnippetError> {
        self.snippets.create(name, body, keywords, now_ms())
    }

    pub fn snippet_update(
        &self,
        id: i64,
        name: &str,
        body: &str,
        keywords: &str,
    ) -> std::result::Result<bool, iskra_core::snippets::SnippetError> {
        self.snippets.update(id, name, body, keywords)
    }

    pub fn snippet_delete(&self, id: i64) -> std::result::Result<bool, iskra_core::snippets::SnippetError> {
        self.snippets.delete(id)
    }

    // --- конвейер записи (воркер) ---

    /// Обработка одного события listener'а: read → исключения (D7) → add → emit.
    /// ВАЖНО (урок прогона 1): owner_process_name — СРАЗУ после read (микрогонка:
    /// владелец клипборда может смениться в любой момент).
    fn process_event(&self, app: &AppHandle) {
        if !self.config.enabled() {
            return; // тумблер на лету: listener жив, события игнорируются
        }
        let content = match iskra_sys::clipboard::read() {
            Ok(c) => c,
            Err(e) => {
                logging::warn(&format!("clipboard: read FAILED (риск 1/9): {e}"));
                return;
            }
        };
        let owner = iskra_sys::clipboard::owner_process_name();
        if let Some(o) = &owner {
            if self.config.is_excluded(o) {
                logging::info(&format!("clipboard: источник «{o}» в исключениях — пропущено"));
                return;
            }
        }
        // Повторная проверка окна подавления: read мог захватить уже наш контент
        // (set_* при вставке) — такое событие в истории не нужно.
        if self.config.is_suppressed(now_ms() as u64) {
            logging::info("clipboard: событие в окне подавления вставки — пропущено");
            return;
        }
        let source = owner.as_deref();
        let outcome = match content {
            ClipboardContent::Text(text) => {
                if text.trim().is_empty() {
                    return; // пустой текст — не история
                }
                self.store
                    .add(&ClipboardNew::Text { text }, source, now_ms())
            }
            ClipboardContent::Image { png, width, height } => {
                // Риск 6: слишком большое — skip+лог ДО записи на диск.
                if png.len() as i64 > MAX_IMAGE_BYTES {
                    logging::warn(&format!(
                        "clipboard: изображение {width}×{height} ({}) > лимита {} байт — пропущено",
                        png.len(),
                        MAX_IMAGE_BYTES
                    ));
                    return;
                }
                self.add_image_bytes(&png, source, now_ms())
            }
            ClipboardContent::Files(paths) => {
                if paths.is_empty() {
                    return;
                }
                let owned: Vec<String> = paths
                    .into_iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                self.store.add(&ClipboardNew::Files { paths: owned }, source, now_ms())
            }
            ClipboardContent::Skipped(reason) => {
                logging::info(&format!("clipboard: формат пропущен (риск 3): {reason}"));
                return;
            }
        };
        match outcome {
            Ok(out) => {
                logging::info(&format!(
                    "clipboard: запись в истории (dedup={}, evicted={})",
                    out.deduplicated, out.evicted
                ));
                if let Some(entry) = out.entry {
                    self.emit_updated(app, &entry);
                }
            }
            Err(e) => logging::warn(&format!("clipboard: add FAILED: {e}")),
        }
    }

    /// Опубликовать `clipboard://updated` (payload — затронутая запись; UI
    /// перезапрашивает список). Публично: команды delete/pin тоже эмитят.
    pub fn emit_updated(&self, app: &AppHandle, entry: &ClipboardEntry) {
        if let Err(e) = app.emit(EVENT_CLIPBOARD_UPDATED, entry) {
            logging::warn(&format!("clipboard: emit {EVENT_CLIPBOARD_UPDATED} FAILED: {e}"));
        }
    }

    /// Записать изображение из байтов PNG: stage `clipboard/{hash}.png` → add →
    /// thumbnail рядом (D4). При дедуп-подъёме путь в БД прежний, а застейдженный
    /// файл (тот же контент → то же хэш-имя) совпадает с ним — удалять нечего;
    /// док-блок `add` требует удаления только если пути разошлись.
    fn add_image_bytes(&self, png: &[u8], source: Option<&str>, now_ms: i64) -> Result<AddOutcome> {
        if png.len() as i64 > MAX_IMAGE_BYTES {
            return Err(ClipboardError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("PNG {} байт > MAX_IMAGE_BYTES", png.len()),
            )));
        }
        let hash = content_hash64(ClipboardKind::Image, png);
        if let Err(e) = std::fs::create_dir_all(&self.images_dir) {
            return Err(ClipboardError::Io(e));
        }
        let staged = self.images_dir.join(format!("{hash}.png"));
        std::fs::write(&staged, png).map_err(ClipboardError::Io)?;
        let staged_path = staged.to_string_lossy().into_owned();

        let outcome =
            self.store
                .add(&ClipboardNew::Image { png_path: staged_path.clone() }, source, now_ms)?;
        match &outcome.entry {
            // Патологический кейс (все остальные pinned): запись вытеснила сама
            // себя, файлы уже удалил репозиторий — добираем thumbnail, если он
            // успел остаться с прошлого раза.
            None => {
                if let Some(thumb) = thumb_for(&staged) {
                    let _ = std::fs::remove_file(thumb);
                }
            }
            Some(entry) if outcome.deduplicated => {
                // Дедуп (док-блок add): застейженный дубликат не нужен, если
                // путь существующей записи другой (при хэш-именах он совпадает).
                if entry.image_path.as_deref() != Some(staged_path.as_str()) {
                    let _ = std::fs::remove_file(&staged);
                    if let Some(thumb) = thumb_for(&staged) {
                        let _ = std::fs::remove_file(thumb);
                    }
                }
            }
            Some(_) => {
                // Новая запись — thumbnail 128px рядом (D4), best-effort.
                if let Some(thumb) = thumb_for(&staged) {
                    write_thumbnail(png, &thumb);
                }
            }
        }
        Ok(outcome)
    }

    // --- вставка (D6) ---

    /// `clipboard_paste(id)`: цепочка D6. Подъём used_at/used_count — повторным
    /// `add` того же контента (гарантированный дедуп, контракт ipc.rs);
    /// собственный set_* подавляется окном suppress (иначе изображение
    /// рэнкодилось бы в дублирующую запись).
    pub fn paste(&self, app: &AppHandle, id: i64) -> std::result::Result<(), String> {
        let entry = self
            .store
            .get(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("запись {id} не найдена"))?;

        let prev = unsafe { GetForegroundWindow() };
        self.config.suppress_until(now_ms() as u64 + PASTE_SUPPRESS_MS);
        hide_main_window(app);

        let set_result: io::Result<()> = match entry.kind {
            ClipboardKind::Text => {
                let text = entry.content.clone().unwrap_or_default();
                iskra_sys::clipboard::set_text(&text)
            }
            ClipboardKind::Image => {
                let path = entry
                    .image_path
                    .clone()
                    .ok_or_else(|| "у image-записи нет image_path".to_string())?;
                let png = std::fs::read(&path).map_err(|e| format!("чтение PNG {path}: {e}"))?;
                iskra_sys::clipboard::set_image(&png)
            }
            ClipboardKind::Files => {
                let paths: Vec<std::ffi::OsString> =
                    entry.files().into_iter().map(std::ffi::OsString::from).collect();
                iskra_sys::clipboard::set_files(&paths)
            }
        };
        set_result.map_err(|e| format!("контент не положен в клипборд: {e}"))?;

        // Подъём записи (контракт clipboard_paste): тот же контент → дедуп.
        if let Some(item) = bump_item(&entry) {
            match self.store.add(&item, entry.source_app.as_deref(), now_ms()) {
                Ok(out) => {
                    if let Some(updated) = out.entry {
                        self.emit_updated(app, &updated);
                    }
                }
                Err(e) => logging::warn(&format!("clipboard: bump после paste FAILED: {e}")),
            }
        }

        restore_and_send(prev);
        Ok(())
    }
}

/// Цикл воркера: владеет listener'ом (его Drop шлёт WM_QUIT при выходе).
fn worker_loop(listener: ClipboardListener, svc: Arc<ClipboardService>, app: AppHandle) {
    let rx = listener.events();
    while let Ok(event) = rx.recv() {
        match event {
            ClipboardEvent::Updated => svc.process_event(&app),
        }
    }
    logging::info("clipboard: воркер завершён");
}

/// Контент записи для подъёма через `add` (дедуп-подъём D5).
fn bump_item(entry: &ClipboardEntry) -> Option<ClipboardNew> {
    match entry.kind {
        ClipboardKind::Text => Some(ClipboardNew::Text { text: entry.content.clone()? }),
        ClipboardKind::Image => Some(ClipboardNew::Image { png_path: entry.image_path.clone()? }),
        ClipboardKind::Files => Some(ClipboardNew::Files { paths: entry.files() }),
    }
}

/// `{stem}_thumb.{ext}` рядом с изображением (раскладка thumbnail'ов, D4).
fn thumb_for(image: &Path) -> Option<PathBuf> {
    let stem = image.file_stem()?;
    let ext = image.extension()?;
    Some(image.with_file_name(format!(
        "{}_thumb.{}",
        stem.to_string_lossy(),
        ext.to_string_lossy()
    )))
}

/// PNG → thumbnail THUMB_PX рядом (best-effort: сбой — лог, не ошибка записи).
fn write_thumbnail(png: &[u8], thumb_path: &Path) {
    let result = (|| -> io::Result<()> {
        let img = image::load_from_memory(png)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("PNG decode: {e}")))?;
        img.thumbnail(THUMB_PX, THUMB_PX)
            .save_with_format(thumb_path, image::ImageFormat::Png)
            .map_err(|e| io::Error::other(format!("save thumb: {e}")))?;
        Ok(())
    })();
    if let Err(e) = result {
        logging::warn(&format!("clipboard: thumbnail FAILED: {e}"));
    }
}

/// Скрыть лончер (переиспользование логики hide_window; D6, шаг 2 цепочки).
fn hide_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

/// Активное окно ДО старта цепочки вставки (для команд run_item/clipboard_paste).
pub fn foreground_hwnd() -> HWND {
    unsafe { GetForegroundWindow() }
}

/// Хвост цепочки D6: restore → пауза → проверка foreground (риск 4) → Ctrl+V.
/// UIPI (риск 2): в elevated-окна ввод не дойдёт — задокументированное
/// ограничение, ошибка SendInput логируется, но не роняет сервис.
fn restore_and_send(prev: HWND) {
    if !prev.0.is_null() {
        unsafe {
            let _ = SetForegroundWindow(prev);
        }
    }
    std::thread::sleep(PASTE_SETTLE);
    let current = unsafe { GetForegroundWindow() };
    if !prev.0.is_null() && current.0 != prev.0 {
        logging::info("clipboard: foreground ушёл от prev — повторный restore (риск 4)");
        unsafe {
            let _ = SetForegroundWindow(prev);
        }
        std::thread::sleep(PASTE_RESTORE_RETRY);
    }
    if let Err(e) = iskra_sys::sendinput::send_ctrl_v() {
        logging::warn(&format!(
            "clipboard: SendInput Ctrl+V FAILED: {e} (UIPI: elevated-окна — риск 2)"
        ));
    }
}

/// D9: тело сниппета только что скопировано (CopyText внутри run_item) —
/// скрыть лончер и вставить в прежнее активное окно. Без suppress: событие
/// от копирования ДОЛЖНО попасть в историю (воркер обработает его сам).
pub fn paste_just_copied(app: &AppHandle, prev: HWND) {
    hide_main_window(app);
    restore_and_send(prev);
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Шаг 6: --clipboard-seed N — синтетическое наполнение для приёмки
// ---------------------------------------------------------------------------

/// Разобрать `--clipboard-seed N` из аргументов (ранний exit как bench-search).
pub fn parse_clipboard_seed_arg() -> Option<usize> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--clipboard-seed")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<usize>().ok())
}

/// Сколько из N seeded-записей — изображения (минимум с текстами, если N < 3).
fn seed_image_count(n: usize) -> usize {
    n.min(3)
}

/// Синтетические тексты с вариациями (детерминированные, уникальные — дедуп
/// не должен схлопывать корпус). Маркер «(seed)» — для точечной очистки.
fn seed_texts(n: usize) -> Vec<String> {
    let ru = [
        "черновик", "отчет", "заметка", "инструкция", "пароль", "адрес", "цитата", "список",
        "план", "итог",
    ];
    let en = ["draft", "report", "notes", "manual", "link", "todo", "meeting", "invoice"];
    (0..n)
        .map(|i| {
            format!(
                "заметка {}: {} / {} — строка для приёмки №{i} (seed)",
                i + 1,
                ru[i % ru.len()],
                en[i % en.len()]
            )
        })
        .collect()
}

/// PNG-заглушка k (0..3): детерминированный градиент 320×200 — изображение с
/// уникальным контентом (иначе дедуп схлопнет все три).
fn seed_image_png(k: usize) -> io::Result<Vec<u8>> {
    let (w, h) = (320u32, 200u32);
    let mut img = image::RgbImage::new(w, h);
    let base = [match k {
        0 => 220u8,
        1 => 60,
        _ => 130,
    }];
    for (x, y, px) in img.enumerate_pixels_mut() {
        let t = (x + y) as f32 / (w + h) as f32; // 0..1 по диагонали
        *px = image::Rgb([
            base[0].saturating_sub((t * 120.0) as u8),
            (30.0 + t * 160.0) as u8,
            (200.0 - t * 90.0) as u8,
        ]);
    }
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| io::Error::other(format!("seed PNG encode: {e}")))?;
    Ok(buf)
}

/// Наполнить историю N записями (тексты + 3 изображения) в штатном
/// %APPDATA%\iskra и завершиться. Вызывается из main() ДО старта Tauri.
/// Все записи получают source_app = "iskra-seed" — точечная очистка приёма.
pub fn run_seed(n: usize) {
    if n == 0 {
        println!("clipboard-seed: нечего вставлять (n=0)");
        return;
    }
    let data_dir = logging::base_dir();
    let db = Arc::new(Db::open(&data_dir.join("index.db")).expect("clipboard-seed: не открыть index.db"));
    let svc = ClipboardService::open(db, data_dir.join("clipboard"));
    let images = seed_image_count(n);
    let mut added = 0usize;
    for (i, text) in seed_texts(n - images).into_iter().enumerate() {
        match svc
            .store
            .add(&ClipboardNew::Text { text }, Some("iskra-seed"), now_ms())
        {
            Ok(out) if out.entry.is_some() => added += 1,
            Ok(out) if out.deduplicated => added += 1, // повтор прогона — подъём
            Ok(_) => {}
            Err(e) => logging::warn(&format!("clipboard-seed: text {i}: {e}")),
        }
    }
    for k in 0..images {
        match seed_image_png(k).and_then(|png| {
            svc.add_image_bytes(&png, Some("iskra-seed"), now_ms())
                .map_err(|e: ClipboardError| io::Error::other(e.to_string()))
        }) {
            Ok(_) => added += 1,
            Err(e) => logging::warn(&format!("clipboard-seed: image {k}: {e}")),
        }
    }
    let total = svc.store.top(usize::MAX).map(|v| v.len()).unwrap_or(0);
    let summary = format!(
        "clipboard-seed: добавлено {added} из {n}, всего записей в истории: {total}"
    );
    println!("{summary}");
    logging::info(&summary);
}

#[cfg(test)]
mod tests {
    use super::*;
    use iskra_core::clipboard::PREVIEW_MAX_CHARS;

    fn svc_in(dir: &Path) -> (Arc<Db>, ClipboardService) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        (db.clone(), ClipboardService::open(db, dir.join("images")))
    }

    fn tiny_png(color: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(3, 3, image::Rgb(color));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    /// Исключения (D7): совпадение по lowercase, пустой список пропускает всех.
    #[test]
    fn excluded_apps_matching() {
        let config = MonitorConfig::new();
        config.apply(&Settings {
            clipboard_enabled: true,
            clipboard_excluded_apps: vec!["Notepad.exe".into(), " code ".into()],
            ..Settings::default()
        });
        assert!(config.is_excluded("notepad.exe"));
        assert!(config.is_excluded("NOTEPAD.EXE"));
        assert!(config.is_excluded("notepad"), "без суффикса .exe тоже матчится");
        assert!(config.is_excluded("code.exe"), "« code » → code матчит code.exe");
        assert!(!config.is_excluded("chrome.exe"));
        config.apply(&Settings::default());
        assert!(!config.is_excluded("notepad.exe"), "пустой список никого не исключает");
    }

    /// Изображения через сервис: новая запись → PNG на диске по хэш-имени +
    /// thumbnail рядом; повторный add того же контента → дедуп-подъём, файл НЕ
    /// удаляется (застейдженный путь совпадает с путём записи), дублей нет.
    #[test]
    fn image_add_stages_file_and_thumb_and_dedups() {
        let dir = std::env::temp_dir().join("iskra-test-clip");
        let _ = std::fs::remove_dir_all(&dir);
        let (_db, svc) = svc_in(&dir);

        let png = tiny_png([200, 10, 10]);
        let out1 = svc.add_image_bytes(&png, Some("snipaste.exe"), 1000).unwrap();
        assert!(!out1.deduplicated);
        let e1 = out1.entry.expect("новая запись на месте");
        assert_eq!(e1.kind, ClipboardKind::Image);
        let path = Path::new(e1.image_path.as_deref().expect("image_path есть"));
        assert!(path.exists(), "PNG застейджен по хэш-имени");
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("{}.png", e1.content_hash),
            "раскладка {{hash}}.png"
        );
        let thumb = thumb_for(path).unwrap();
        assert!(thumb.exists(), "thumbnail 128px записан");

        let out2 = svc.add_image_bytes(&png, None, 2000).unwrap();
        assert!(out2.deduplicated, "тот же контент — дедуп-подъём");
        assert_eq!(out2.entry.as_ref().unwrap().id, e1.id);
        assert_eq!(out2.entry.as_ref().unwrap().used_count, 2);
        assert!(path.exists(), "файл существующей записи не тронут");
        assert!(thumb.exists());
        assert_eq!(svc.store.top(10).unwrap().len(), 1, "дублей нет");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// > MAX_IMAGE_BYTES отклоняется ДО записи на диск (риск 6): файл не
    /// появляется ни в каталоге изображений, ни в истории.
    #[test]
    fn oversized_image_rejected_before_disk() {
        let dir = std::env::temp_dir().join("iskra-test-clip-oversize");
        let _ = std::fs::remove_dir_all(&dir);
        let (_db, svc) = svc_in(&dir);
        let big = vec![0u8; (MAX_IMAGE_BYTES + 1) as usize];
        let err = svc.add_image_bytes(&big, None, 1000).unwrap_err();
        assert!(matches!(err, ClipboardError::Io(_)));
        assert_eq!(svc.store.top(10).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Тексты через сервис доходят до хранилища; превью обрезано (D8).
    #[test]
    fn text_add_via_store_and_preview_cap() {
        let dir = std::env::temp_dir().join("iskra-test-clip-text");
        let _ = std::fs::remove_dir_all(&dir);
        let (_db, svc) = svc_in(&dir);
        let long = "х".repeat(PREVIEW_MAX_CHARS + 30);
        svc.store
            .add(&ClipboardNew::Text { text: long.clone() }, Some("notepad.exe"), 1000)
            .unwrap();
        let top = svc.store.top(10).unwrap();
        assert_eq!(top[0].preview.chars().count(), PREVIEW_MAX_CHARS);
        assert_eq!(top[0].source_app.as_deref(), Some("notepad.exe"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Подъём после paste: bump_item восстанавливает ClipboardNew всех видов;
    /// повторный add того же контента — дедуп (used_count растёт, дублей нет).
    #[test]
    fn paste_bump_dedups_all_kinds() {
        let dir = std::env::temp_dir().join("iskra-test-clip-bump");
        let _ = std::fs::remove_dir_all(&dir);
        let (_db, svc) = svc_in(&dir);

        let out = svc
            .store
            .add(&ClipboardNew::Text { text: "вставь меня".into() }, None, 1000)
            .unwrap();
        let text_entry = out.entry.unwrap();
        let item = bump_item(&text_entry).expect("text bump");
        let again = svc.store.add(&item, None, 2000).unwrap();
        assert!(again.deduplicated);
        assert_eq!(again.entry.unwrap().used_count, 2);

        let png = tiny_png([1, 2, 3]);
        let out = svc.add_image_bytes(&png, None, 3000).unwrap();
        let img_entry = out.entry.unwrap();
        let item = bump_item(&img_entry).expect("image bump");
        let again = svc.store.add(&item, None, 4000).unwrap();
        assert!(again.deduplicated, "image bump по тому же файлу — дедуп");
        assert_eq!(again.entry.unwrap().used_count, 2);

        let out = svc
            .store
            .add(
                &ClipboardNew::Files { paths: vec![r"C:\a.txt".into()] },
                None,
                5000,
            )
            .unwrap();
        let files_entry = out.entry.unwrap();
        let item = bump_item(&files_entry).expect("files bump");
        let again = svc.store.add(&item, None, 6000).unwrap();
        assert!(again.deduplicated);
        assert_eq!(svc.store.top(10).unwrap().len(), 3, "ни одного дубля");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// seed-корпус: уникальные тексты, ровно n, все с маркером очистки.
    #[test]
    fn seed_corpus_unique_and_marked() {
        let texts = seed_texts(120);
        assert_eq!(texts.len(), 120);
        let mut sorted = texts.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 120, "тексты уникальны (дедуп не схлопнет корпус)");
        assert!(texts.iter().all(|t| t.contains("(seed)")));

        assert_eq!(seed_image_count(1), 1);
        assert_eq!(seed_image_count(3), 3);
        assert_eq!(seed_image_count(1000), 3);

        // три заглушки генерируются, валидны как PNG и попарно различны
        let a = seed_image_png(0).unwrap();
        let b = seed_image_png(1).unwrap();
        let c = seed_image_png(2).unwrap();
        for buf in [&a, &b, &c] {
            assert_eq!(&buf[..8], b"\x89PNG\r\n\x1a\n");
            image::load_from_memory(buf).expect("PNG декодируется");
        }
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    /// Thumbnail: имя `{stem}_thumb.{ext}` рядом с изображением.
    #[test]
    fn thumb_naming() {
        let p = Path::new(r"C:\data\clip\abcd1234.png");
        assert_eq!(
            thumb_for(p).unwrap(),
            PathBuf::from(r"C:\data\clip\abcd1234_thumb.png")
        );
    }

    /// Окно подавления: до момента — молчим, после — событие обрабатывается.
    #[test]
    fn suppress_window_semantics() {
        let config = MonitorConfig::new();
        assert!(!config.is_suppressed(1000));
        config.suppress_until(5000);
        assert!(config.is_suppressed(4999));
        assert!(!config.is_suppressed(5000));
    }

    // --- дымовые проверки на живой БД (%APPDATA%\iskra) — запуск вручную во
    // время смоука шага 3, когда iskra.exe запущен и слушает клипборд:
    // cargo test -p iskra smoke -- --ignored --nocapture ---

    /// Смоук «clipboard_list возвращает»: ровно код команды — ClipboardStore
    /// поверх живой БД, топ-100. Печатает первые записи.
    #[test]
    #[ignore]
    fn smoke_clipboard_list_live() {
        let data_dir = logging::base_dir();
        let db = Arc::new(Db::open(&data_dir.join("index.db")).expect("live db"));
        let svc = ClipboardService::open(db, data_dir.join("clipboard"));
        let all = svc.list(None).expect("clipboard_list");
        println!("clipboard_list: {} записей (лимит {LIST_LIMIT})", all.len());
        for e in all.iter().take(5) {
            println!(
                "  id={} kind={:?} used_count={} source={:?} preview={:?}",
                e.id,
                e.kind,
                e.used_count,
                e.source_app,
                e.preview.chars().take(40).collect::<String>()
            );
        }
        let smoke = svc.list(Some("iskra-smoke")).expect("clipboard_list(query)");
        println!("поиск «iskra-smoke»: {} записей", smoke.len());
        assert_eq!(smoke.len(), 1, "повторные копирования НЕ плодят записей (дедуп D5)");
        let top = smoke.first().unwrap();
        // Ровно 3 даёт кор-тест dedup_bump_existing_entry; в живом смоуке
        // Set-Clipboard PowerShell кладёт контент несколько раз за вызов
        // (в т.ч. «неудачные») — поэтому проверяем монотонность (>= 3).
        assert!(top.used_count >= 3, "каждое копирование поднимает счётчик, было 3+");
    }

    /// Смоук «рестарт — история на месте»: записи смоука и seeded пережили
    /// перезапуск iskra.exe (проверка выполняется ПОСЛЕ рестарта).
    #[test]
    #[ignore]
    fn smoke_history_survives_restart() {
        let data_dir = logging::base_dir();
        let db = Arc::new(Db::open(&data_dir.join("index.db")).expect("live db"));
        let st = ClipboardStore::new(db);
        let smoke = st.list(Some("iskra-smoke"), 10).expect("list");
        assert_eq!(smoke.len(), 1, "запись смоука пережила рестарт");
        let seeded_texts = st.list(Some("(seed)"), 100).expect("list");
        println!(
            "после рестарта: smoke=1, seeded текстов={}",
            seeded_texts.len()
        );
        assert!(!seeded_texts.is_empty(), "seed-записи пережили рестарт");
    }

    /// Очистка после смоука: удалить все тестовые записи (smoke по превью,
    /// seed по source_app='iskra-seed'); PNG/thumbnail удаляет репозиторий.
    #[test]
    #[ignore]
    fn smoke_cleanup_test_entries() {
        let data_dir = logging::base_dir();
        let db = Arc::new(Db::open(&data_dir.join("index.db")).expect("live db"));
        let st = ClipboardStore::new(db.clone());
        let ids: Vec<i64> = {
            let conn = db.conn();
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM clipboard_entries
                     WHERE source_app = 'iskra-seed'
                        OR preview LIKE '%iskra-smoke%'
                        OR preview LIKE '%изображение 96×64%'
                     ORDER BY id",
                )
                .expect("select test ids");
            let rows = stmt.query_map([], |r| r.get(0)).expect("query_map");
            rows.map(|r| r.expect("row")).collect()
        };
        let mut removed = 0usize;
        for id in ids {
            if st.delete(id).unwrap() {
                removed += 1;
            }
        }
        println!("очищено тестовых записей: {removed}");
    }
}
