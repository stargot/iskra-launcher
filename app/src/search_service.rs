//! SearchService — склейка ядра поиска (iskra-core) и системы (iskra-sys)
//! для Tauri-команд (план Ф2, шаг 4, D7/D8/D9).
//!
//! Потоки:
//! - вызывающий (Tauri-команда `search`): быстрые провайдеры (apps/calc/settings/
//!   system/web) — синхронно через Aggregator (D8, < 20 мс);
//! - `search-slow` (поток на запрос): «медленный» file-провайдер (FTS5), merge
//!   с быстрыми, результат — событие `search://updated` с тем же queryId (D8).
//!   ЕДИНСТВЕННЫЙ источник эм-итов списков — search-путь с валидным qid:
//!   отдельный re-emit от icon-воркера УБРАН (полировка фазы 2: он гонлся
//!   с командой пользователя за qid и возвращал в UI неотфильтрованный список);
//! - `iskra-icons`: очередь извлечения иконок (COM STA внутри iskra-sys/icons),
//!   best-effort, ошибка → icon_path = None + лог (D7). После успешной пачки —
//!   лёгкое событие `icons://updated` БЕЗ списка: UI сам перезапрашивает search()
//!   с текущим вводом, иконки подставляются обычным путём;
//! - `iskra-index`: фоновый индексатор — первый полный скан, далее mtime-
//!   инкремент раз в 30 мин (D9), прогресс — событие `index://progress`.
//!
//! БД: индексатор/usage — одно соединение; file-провайдер — ВТОРОЕ соединение
//! к той же БД: WAL допускает параллельных читателей, поэтому долгая транзакция
//! первого полного скана не блокирует поиск (осознанное отклонение от «ровно
//! одного соединения», зафиксировано в отчёте шага 4).
//!
//! Recents на пустой запрос: usage-таблица хранит только (id, count, time) без
//! заголовков, поэтому элементы recents — из реестра виденных элементов сессии
//! + снимок `recents.json` (переживает рестарт); порядок — по usage из БД.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter};

use iskra_core::ipc::{
    IndexPhase, IndexStatus, SearchError, SearchResponse, EVENT_ICONS_UPDATED,
    EVENT_INDEX_PROGRESS, EVENT_SEARCH_UPDATED,
};
use iskra_core::logging;
use iskra_core::search::aggregator::RESULT_LIMIT;
use iskra_core::search::indexer;
use iskra_core::search::{
    default_app_dirs, scan_apps_report, Aggregator, AppEntry, AppsProvider, CalcProvider, Db,
    FilesProvider, ItemAction, SearchItem, SearchProvider, SettingsProvider, SystemCommand,
    SystemProvider, UsageStore, WebProvider,
};
use iskra_sys::{icons, power, shell};

use crate::clipboard;

/// Размер извлекаемых иконок (квадрат, px). 32 — с запасом под 20×20 и HiDPI.
const ICON_PX: i32 = 32;
/// Сколько элементов хранить в снимке recents.json.
const RECENTS_SAVE_LIMIT: usize = 200;
/// Интервал инкрементальной переиндексации (D9).
const INDEX_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// Тишина в очереди иконок, после которой пачка считается завершённой.
const ICON_BATCH_IDLE: Duration = Duration::from_millis(100);

/// Доставка событий в UI. Отделена от сервиса: прод — Tauri `AppHandle::emit`,
/// тесты — коллекция в памяти (без AppHandle).
pub trait EventSink: Send + Sync {
    fn search_updated(&self, response: &SearchResponse);
    fn index_progress(&self, status: &IndexStatus);
    /// Новые иконки в кэше (без списка!) — UI сам перезапрашивает search().
    fn icons_updated(&self);
}

/// Прод-приёмник: tauri emit (каналы — часть контракта ipc.rs).
pub struct TauriSink(pub AppHandle);

impl EventSink for TauriSink {
    fn search_updated(&self, response: &SearchResponse) {
        if let Err(e) = self.0.emit(EVENT_SEARCH_UPDATED, response) {
            logging::warn(&format!("search: emit {EVENT_SEARCH_UPDATED} FAILED: {e}"));
        }
    }

    fn index_progress(&self, status: &IndexStatus) {
        if let Err(e) = self.0.emit(EVENT_INDEX_PROGRESS, status) {
            logging::warn(&format!("search: emit {EVENT_INDEX_PROGRESS} FAILED: {e}"));
        }
    }

    fn icons_updated(&self) {
        if let Err(e) = self.0.emit(EVENT_ICONS_UPDATED, ()) {
            logging::warn(&format!("search: emit {EVENT_ICONS_UPDATED} FAILED: {e}"));
        }
    }
}

/// Пути и входные данные сервиса (прод — из окружения, тесты — temp).
pub struct ServicePaths {
    /// Каталог данных: index.db, icons/, recents.json.
    pub data_dir: PathBuf,
    /// Корни индексации (прод: папки пользователя; тесты: пусто/temp).
    pub index_roots: Vec<PathBuf>,
    /// Список приложений (один скан на провайдер и карту иконок).
    pub apps: Vec<AppEntry>,
}

/// UsageStore поверх таблицы usage (Db) и реестра виденных элементов сессии.
struct ServiceUsage {
    db: Arc<Db>,
    registry: Arc<Mutex<HashMap<String, SearchItem>>>,
}

impl UsageStore for ServiceUsage {
    fn used_count(&self, id: &str) -> u32 {
        self.db.used_count(id).unwrap_or_else(|e| {
            logging::warn(&format!("usage: used_count({id}): {e}"));
            0
        })
    }

    fn last_used_at(&self, id: &str) -> Option<SystemTime> {
        let ms = self.db.last_used_ms(id).ok().flatten()?;
        UNIX_EPOCH.checked_add(Duration::from_millis(ms as u64))
    }

    /// Кандидаты recents: только элементы, которые сервис уже видел
    /// (usage-таблица не хранит заголовки); порядок — usage из БД.
    fn recents(&self, limit: usize) -> Vec<SearchItem> {
        let reg = self.registry.lock().expect("registry poisoned");
        let mut scored: Vec<(u32, i64, &SearchItem)> = reg
            .values()
            .map(|it| {
                (
                    self.used_count(&it.id),
                    self.db.last_used_ms(&it.id).unwrap_or(None).unwrap_or(0),
                    it,
                )
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        scored.into_iter().take(limit).map(|(_, _, it)| it.clone()).collect()
    }
}

/// Команды индекс-воркеру.
enum IndexCommand {
    /// Полный рескан сейчас (команда reindex из UI).
    ReindexFull,
}

/// Сервис поиска: агрегатор быстрых провайдеров + file-провайдер + иконки +
/// фоновый индексатор. Управляется через `Arc`, потоки держат клоны.
pub struct SearchService {
    /// Запись: usage, исполнение run_item, индексатор.
    db: Arc<Db>,
    /// «Медленный» file-провайдер (отдельное read-соединение, см. шапку).
    files: Arc<FilesProvider>,
    /// Быстрые провайдеры: apps, calc, settings, system, web.
    agg: Arc<Aggregator>,
    usage: Arc<ServiceUsage>,
    /// Реестр всех выданных элементов (быстрые + файловые) для run_item.
    registry: Arc<Mutex<HashMap<String, SearchItem>>>,
    /// Генератор queryId (монотонный; свежее — больше).
    query_id: AtomicU64,
    /// Отсечение устаревших медленных ответов (D8); Arc — его читает search-slow.
    latest_qid: Arc<AtomicU64>,
    /// Последний статус индексатора (для get_index_status).
    status: Mutex<IndexStatus>,
    /// id apps-элемента → исходник иконки (icon_location из .lnk или target).
    icon_sources: HashMap<String, String>,
    /// Каталог кэша иконок: %APPDATA%\iskra\icons (D7).
    icon_dir: String,
    /// Источники, стоящие в очереди извлечения (дедупликация).
    icon_queued: Mutex<HashSet<String>>,
    /// Источники, чьё извлечение не удалось (не ретраим в этой сессии).
    icon_failed: Mutex<HashSet<String>>,
    icon_tx: Sender<String>,
    index_tx: Sender<IndexCommand>,
    sink: Arc<dyn EventSink>,
    recents_path: PathBuf,
    index_roots: Vec<PathBuf>,
}

impl SearchService {
    /// Прод-конструктор: данные в %APPDATA%\iskra, индексация — папки
    /// пользователя, приложения — Start Menu + Desktop (один скан).
    pub fn new(sink: Arc<dyn EventSink>) -> Arc<SearchService> {
        let report = scan_apps_report(&default_app_dirs());
        logging::info(&format!(
            "search: apps scanned={} broken_lnk={}",
            report.apps.len(),
            report.broken
        ));
        Self::open(
            ServicePaths {
                data_dir: logging::base_dir(),
                index_roots: indexer::default_roots(),
                apps: report.apps,
            },
            sink,
        )
    }

    /// Полный конструктор (тесты подменяют пути/приложения).
    pub fn open(paths: ServicePaths, sink: Arc<dyn EventSink>) -> Arc<SearchService> {
        if let Err(e) = std::fs::create_dir_all(&paths.data_dir) {
            logging::warn(&format!("search: create_dir_all: {e}"));
        }
        let db = Arc::new(
            Db::open(&paths.data_dir.join("index.db")).expect("search: не открыть index.db"),
        );
        // Второе соединение — только чтение для file-провайдера (WAL:
        // читатели не блокируются писателем) — см. шапку модуля.
        let db_read = Arc::new(
            Db::open(&paths.data_dir.join("index.db"))
                .expect("search: не открыть index.db (read)"),
        );

        let icon_sources: HashMap<String, String> = paths
            .apps
            .iter()
            .map(|a| {
                (
                    format!("apps:{}", a.target.to_lowercase()),
                    a.icon_source.clone().unwrap_or_else(|| a.target.clone()),
                )
            })
            .collect();
        let agg = Arc::new(Aggregator::new(vec![
            Arc::new(AppsProvider::new(paths.apps)),
            Arc::new(CalcProvider),
            Arc::new(SettingsProvider),
            Arc::new(SystemProvider),
            Arc::new(WebProvider::default()),
        ]));
        let files = Arc::new(FilesProvider::new(db_read));
        let registry = Arc::new(Mutex::new(HashMap::new()));
        let usage = Arc::new(ServiceUsage { db: db.clone(), registry: registry.clone() });

        let (icon_tx, icon_rx) = channel::<String>();
        let (index_tx, index_rx) = channel::<IndexCommand>();
        let svc = Arc::new(SearchService {
            db,
            files,
            agg,
            usage,
            registry,
            query_id: AtomicU64::new(0),
            latest_qid: Arc::new(AtomicU64::new(0)),
            status: Mutex::new(IndexStatus { phase: IndexPhase::Scanning, done: 0, total: 0 }),
            icon_sources,
            icon_dir: paths.data_dir.join("icons").to_string_lossy().into_owned(),
            icon_queued: Mutex::new(HashSet::new()),
            icon_failed: Mutex::new(HashSet::new()),
            icon_tx,
            index_tx,
            sink,
            recents_path: paths.data_dir.join("recents.json"),
            index_roots: paths.index_roots,
        });
        svc.load_recents();
        spawn_icon_worker(svc.clone(), icon_rx);
        spawn_index_worker(svc.clone(), index_rx);
        svc
    }

    // --- поиск (D8) ---

    /// Команда `search(q)`: быстрые провайдеры синхронно; «медленные» (файлы)
    /// уйдут событием `search://updated` с тем же queryId. Пустой запрос →
    /// recents (usage из БД + реестр сессии).
    pub fn search(&self, q: &str) -> SearchResponse {
        self.search_internal(q)
    }

    fn search_internal(&self, q: &str) -> SearchResponse {
        // qid монотонен и назначается ТОЛЬКО здесь — единственная точка входа
        // для эм-итов списков (D8): и ответ команды, и событие search-slow
        // несут qid реального запроса пользователя.
        let id = self.query_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.latest_qid.store(id, Ordering::Relaxed);
        self.search_with_id(q, id)
    }

    /// Конвейер для уже назначенного qid: быстрые провайдеры, иконки, реестр;
    /// файлы — фоновым потоком с событием (D8).
    fn search_with_id(&self, q: &str, id: u64) -> SearchResponse {
        // 1. Быстрые провайдеры — синхронно (контракт: < 20 мс).
        let mut items = self.agg.query(q, self.usage.as_ref());
        self.decorate_icons(&mut items);
        self.remember(&items);

        // 2. Файлы — в фоне; результат придёт событием (D8). Пустой запрос —
        //    recents, файловый провайдер по контракту молчит.
        if !q.trim().is_empty() {
            self.spawn_slow(q.to_string(), id, items.clone());
        }
        SearchResponse { query_id: id, items }
    }

    /// «Медленный» путь: FTS5-поиск файлов, итоговый скор (ранжирование),
    /// merge с быстрыми, лимит 50, отсечение устаревших, событие.
    fn spawn_slow(&self, q: String, qid: u64, fast: Vec<SearchItem>) {
        let files = self.files.clone();
        let usage = self.usage.clone();
        let svc_sink = self.sink.clone();
        let latest = self.latest_qid.clone();
        let registry = self.registry.clone();
        let _ = std::thread::Builder::new().name("search-slow".into()).spawn(move || {
            let t0 = Instant::now();
            let mut file_items = files.query(&q);
            let now = SystemTime::now();
            for it in file_items.iter_mut() {
                it.score = ranking_total(
                    it.score,
                    usage.used_count(&it.id),
                    now,
                    usage.last_used_at(&it.id),
                );
            }
            let mut merged = fast;
            merged.extend(file_items);
            let merged = dedupe_sort_limit(merged);
            // Ответ на старый запрос никому не нужен — UI отбросил бы его (D8).
            if qid < latest.load(Ordering::Relaxed) {
                return;
            }
            {
                let mut reg = registry.lock().expect("registry poisoned");
                for it in &merged {
                    reg.insert(it.id.clone(), it.clone());
                }
            }
            logging::info(&format!(
                "search slow: q={q:?} qid={qid} items={} in {:?}",
                merged.len(),
                t0.elapsed()
            ));
            svc_sink.search_updated(&SearchResponse { query_id: qid, items: merged });
        });
    }

    /// D7: подстановка иконок apps-элементам. Кэш есть → путь сразу; нет →
    /// постановка в очередь извлечения (best-effort: ошибка → None + лог).
    fn decorate_icons(&self, items: &mut [SearchItem]) {
        for it in items.iter_mut() {
            let ItemAction::LaunchApp { path, .. } = &it.action else { continue };
            let Some(source) = self.icon_source_for(&it.id, path) else { continue };
            let cached = self.icon_cache_path(&source);
            if cached.exists() {
                it.icon_path = cached.to_str().map(str::to_owned);
            } else {
                self.queue_icon(source);
            }
        }
    }

    /// Исходник иконки: icon_location из .lnk (карта по id), иначе цель запуска.
    /// None — извлечение уже не удалось в этой сессии (не ретраим).
    fn icon_source_for(&self, id: &str, fallback: &str) -> Option<String> {
        let source = self
            .icon_sources
            .get(id)
            .cloned()
            .unwrap_or_else(|| fallback.to_string());
        let failed = self.icon_failed.lock().expect("icon_failed poisoned");
        if failed.contains(&source) {
            None
        } else {
            Some(source)
        }
    }

    fn queue_icon(&self, source: String) {
        let mut queued = self.icon_queued.lock().expect("icon_queued poisoned");
        if queued.insert(source.clone()) {
            let _ = self.icon_tx.send(source); // приёмник живёт столько же, сколько сервис
        }
    }

    fn icon_cache_path(&self, source: &str) -> PathBuf {
        PathBuf::from(&self.icon_dir).join(icons::cache_file_name(source))
    }

    fn remember(&self, items: &[SearchItem]) {
        let mut reg = self.registry.lock().expect("registry poisoned");
        for it in items {
            reg.insert(it.id.clone(), it.clone());
        }
    }

    // --- run_item ---

    /// Команда `run_item(id)`: исполнение ItemAction через iskra-sys/буфер,
    /// затем usage++ (БД) и снимок recents.
    pub fn run_item(&self, id: &str) -> Result<(), SearchError> {
        let item = {
            let reg = self.registry.lock().expect("registry poisoned");
            reg.get(id).cloned()
        }
        .or_else(|| self.agg.item(id))
        .ok_or(SearchError::NotFound)?;

        self.execute(&item.action)
            .map_err(|e| SearchError::Action { message: e })?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if let Err(e) = self.db.record_use(id, now_ms) {
            logging::warn(&format!("run_item: record_use({id}): {e}"));
        }
        self.save_recents();
        Ok(())
    }

    /// Исполнение действия элемента (iskra-sys — весь unsafe там; app — склейка).
    fn execute(&self, action: &ItemAction) -> Result<(), String> {
        match action {
            ItemAction::LaunchApp { path, args } => {
                shell::launch_params(path, args.as_deref().unwrap_or("")).map_err(|e| e.to_string())
            }
            ItemAction::OpenPath { path } => shell::launch(path).map_err(|e| e.to_string()),
            ItemAction::OpenUri { uri } => shell::launch(uri).map_err(|e| e.to_string()),
            ItemAction::WebSearch { url } => shell::launch(url).map_err(|e| e.to_string()),
            ItemAction::CopyText { text } => clipboard::copy_text(text).map_err(|e| e.to_string()),
            ItemAction::System { command } => {
                let result = match command {
                    SystemCommand::Lock => power::lock(),
                    SystemCommand::MonitorOff => power::monitor_off(),
                    SystemCommand::Sleep => power::sleep(),
                    SystemCommand::Restart => power::restart(),
                    SystemCommand::Shutdown => power::shutdown(),
                    SystemCommand::EmptyRecycleBin => power::empty_recycle_bin(false),
                };
                result.map_err(|e| e.to_string())
            }
        }
    }

    // --- индексация (D9) ---

    /// Последний статус индексатора (команда get_index_status).
    pub fn index_status(&self) -> IndexStatus {
        *self.status.lock().expect("status poisoned")
    }

    /// Полный рескан по запросу из UI (команда reindex).
    pub fn request_reindex(&self) {
        logging::info("search: reindex requested");
        if self.index_tx.send(IndexCommand::ReindexFull).is_err() {
            logging::warn("search: index worker недоступен");
        }
    }

    /// Один прогон индексатора (полный или инкрементальный) с пересылкой
    /// прогресса в sink и статус. Фазы в runtime.log — для smoke/диагностики.
    fn run_index(&self, full: bool) {
        let started = Instant::now();
        logging::info(&format!("index: {} scan started", if full { "full" } else { "incremental" }));
        let (tx, rx) = channel::<indexer::IndexProgress>();
        let idx = indexer::Indexer::with_roots(self.db.clone(), self.index_roots.clone());
        let scan = std::thread::Builder::new()
            .name("index-scan".into())
            .spawn(move || {
                let result = if full {
                    idx.run_full(Some(&tx))
                } else {
                    idx.run_incremental(Some(&tx))
                };
                result
                // tx отпадает здесь — цикл приёма ниже завершается
            });
        let mut last_phase = None;
        while let Ok(p) = rx.recv() {
            if last_phase != Some(p.phase) {
                logging::info(&format!("index: phase={:?} done={} total={}", p.phase, p.done, p.total));
                last_phase = Some(p.phase);
            }
            let status =
                IndexStatus { phase: IndexPhase::from(p.phase), done: p.done, total: p.total };
            *self.status.lock().expect("status poisoned") = status;
            self.sink.index_progress(&status);
        }
        if let Ok(handle) = scan {
            match handle.join() {
                Ok(Ok(stats)) => logging::info(&format!(
                    "index: done in {:?} discovered={} indexed={} unchanged={} removed={} truncated={}",
                    started.elapsed(),
                    stats.discovered,
                    stats.indexed,
                    stats.unchanged,
                    stats.removed,
                    stats.truncated
                )),
                Ok(Err(e)) => logging::warn(&format!("index: FAILED: {e}")),
                Err(_) => logging::warn("index: scan thread panicked"),
            }
        }
    }

    // --- recents: снимок на диск ---

    fn save_recents(&self) {
        let items: Vec<SearchItem> = {
            let reg = self.registry.lock().expect("registry poisoned");
            reg.values().cloned().collect()
        };
        let mut scored: Vec<(u32, i64, SearchItem)> = items
            .into_iter()
            .map(|it| {
                (
                    self.db.used_count(&it.id).unwrap_or(0),
                    self.db.last_used_ms(&it.id).unwrap_or(None).unwrap_or(0),
                    it,
                )
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let top: Vec<SearchItem> =
            scored.into_iter().take(RECENTS_SAVE_LIMIT).map(|(_, _, it)| it).collect();
        match serde_json::to_string(&top) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.recents_path, json) {
                    logging::warn(&format!("search: save recents: {e}"));
                }
            }
            Err(e) => logging::warn(&format!("search: serialize recents: {e}")),
        }
    }

    fn load_recents(&self) {
        let json = match std::fs::read_to_string(&self.recents_path) {
            Ok(json) => json,
            Err(_) => return, // нет файла — пустой реестр, не ошибка
        };
        match serde_json::from_str::<Vec<SearchItem>>(&json) {
            Ok(items) => {
                let n = items.len();
                let mut reg = self.registry.lock().expect("registry poisoned");
                for it in items {
                    reg.insert(it.id.clone(), it);
                }
                logging::info(&format!("search: recents restored: {n}"));
            }
            Err(e) => logging::warn(&format!("search: recents.json битый, игнорирую: {e}")),
        }
    }
}

// --- вспомогательное ---

/// Итоговый скор файлового элемента (формула — ranking::total_score, веса там).
fn ranking_total(fuzzy_score: i64, used: u32, now: SystemTime, last: Option<SystemTime>) -> i64 {
    iskra_core::search::ranking::total_score(
        fuzzy_score,
        iskra_core::search::ranking::PRIORITY_FILES,
        used,
        now,
        last,
    )
}

/// Merge двух источников: dedupe по (provider, title) — максимум скора,
/// сортировка по убыванию, лимит 50 (та же семантика, что в агрегаторе).
fn dedupe_sort_limit(items: Vec<SearchItem>) -> Vec<SearchItem> {
    let mut best: HashMap<(String, String), SearchItem> = HashMap::new();
    for it in items {
        let key = (it.provider.clone(), it.title.clone());
        match best.get_mut(&key) {
            Some(existing) if existing.score >= it.score => {}
            _ => {
                best.insert(key, it);
            }
        }
    }
    let mut out: Vec<SearchItem> = best.into_values().collect();
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.title.cmp(&b.title)));
    out.truncate(RESULT_LIMIT);
    out
}

/// Иконки: извлечение в фоне (COM STA — внутри iskra-sys/icons, свой цикл).
/// После пачки успешных извлечений — refresh последнего запроса (иконки
/// появляются без повторного ввода; рекурсии нет: всё извлечённое кэшировано).
fn spawn_icon_worker(svc: Arc<SearchService>, rx: Receiver<String>) {
    let _ = std::thread::Builder::new().name("iskra-icons".into()).spawn(move || {
        loop {
            let Ok(first) = rx.recv() else { break }; // отправитель отпал — выход
            let mut extracted = false;
            extract_one(&svc, &first, &mut extracted);
            while let Ok(next) = rx.recv_timeout(ICON_BATCH_IDLE) {
                extract_one(&svc, &next, &mut extracted);
            }
            if extracted {
                // Без списка! UI сам перезапросит search() с текущим вводом
                // (полировка фазы 2: re-emit списка из воркера убран — он
                // гонлся с командой пользователя за qid).
                svc.sink.icons_updated();
            }
        }
    });
}

fn extract_one(svc: &SearchService, source: &str, extracted: &mut bool) {
    svc.icon_queued.lock().expect("icon_queued poisoned").remove(source);
    let path = svc.icon_cache_path(source);
    if path.exists() {
        return; // уже в кэше (например, из прошлой сессии)
    }
    match icons::extract_to_cache(source, ICON_PX, &svc.icon_dir) {
        Ok(path) => {
            *extracted = true;
            logging::info(&format!("icons: extracted {source} -> {path}"));
        }
        Err(e) => {
            svc.icon_failed.lock().expect("icon_failed poisoned").insert(source.to_string());
            logging::warn(&format!("icons: extract {source}: {e} (icon_path=None)"));
        }
    }
}

/// Индекс-воркер: первый прогон (полный, если индекс пуст; иначе mtime-
/// инкремент), далее ReindexFull по команде или инкремент раз в 30 мин (D9).
fn spawn_index_worker(svc: Arc<SearchService>, rx: Receiver<IndexCommand>) {
    let _ = std::thread::Builder::new().name("iskra-index".into()).spawn(move || {
        let full = svc.db.count_files().unwrap_or(0) == 0;
        svc.run_index(full);
        loop {
            match rx.recv_timeout(INDEX_INTERVAL) {
                Ok(IndexCommand::ReindexFull) => svc.run_index(true),
                Err(RecvTimeoutError::Timeout) => svc.run_index(false),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}

// --- тесты: сервис целиком, без Tauri (VecSink + temp-каталог) ---

#[cfg(test)]
mod tests {
    use super::*;
    use iskra_core::search::FileRecord;

    #[derive(Default)]
    struct VecSink {
        updated: Mutex<Vec<SearchResponse>>,
        progress: Mutex<Vec<IndexStatus>>,
        icons_updated: Mutex<Vec<()>>,
    }

    impl EventSink for VecSink {
        fn search_updated(&self, response: &SearchResponse) {
            self.updated.lock().unwrap().push(response.clone());
        }
        fn index_progress(&self, status: &IndexStatus) {
            self.progress.lock().unwrap().push(*status);
        }
        fn icons_updated(&self) {
            self.icons_updated.lock().unwrap().push(());
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "iskra-svc-test-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Сервис в temp-каталоге: без приложений, индексация — пустой корень
    /// (воркер отработает «0 файлов», реальных папок не трогаем).
    fn test_service(label: &str) -> (Arc<SearchService>, Arc<VecSink>, PathBuf) {
        let dir = temp_dir(label);
        let sink = Arc::new(VecSink::default());
        let svc = SearchService::open(
            ServicePaths { data_dir: dir.clone(), index_roots: vec![], apps: vec![] },
            sink.clone(),
        );
        (svc, sink, dir)
    }

    /// Дождаться завершения стартового прогона индекс-воркера (иначе его фаза
    /// Cleaning может удалить тестовый файл из индекса — гонка теста с воркером).
    fn wait_index_done(svc: &SearchService) {
        for _ in 0..200 {
            if svc.index_status().phase == IndexPhase::Done {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("индекс-воркер не завершил стартовый прогон за 2 с");
    }

    /// Дождаться события search://updated для qid (поток search-slow асинхронен).
    fn wait_updated(sink: &VecSink, qid: u64) -> Option<SearchResponse> {
        for _ in 0..200 {
            {
                let evs = sink.updated.lock().unwrap();
                if let Some(r) = evs.iter().rev().find(|r| r.query_id == qid) {
                    return Some(r.clone());
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Полный конвейер D8: поиск → быстрые сразу; файловые — событием с тем же
    /// queryId; элемент файла попадает в реестр и исполняется run_item (usage++).
    #[test]
    fn search_fast_sync_slow_via_event_and_run_item() {
        let (svc, sink, dir) = test_service("conveyor");
        wait_index_done(&svc);
        // Файл в индексе — пишем напрямую через Db (провайдер читает его же БД).
        svc.db
            .apply_files(
                &[FileRecord {
                    path: r"C:\t\Доклад о бюджете.pdf".to_string(),
                    name: "доклад о бюджете.pdf".to_string(),
                    ext: Some("pdf".to_string()),
                    mtime: 1,
                    size: 1,
                }],
                iskra_core::search::ApplyMode::Full,
                &|_, _| {},
            )
            .unwrap();

        let resp = svc.search("доклад");
        assert!(resp.items.iter().any(|i| i.provider == "web"), "web-fallback в быстром ответе");
        assert!(resp.items.iter().all(|i| i.provider != "files"), "файлы НЕ в синхронном ответе");

        let updated = wait_updated(&sink, resp.query_id).expect("событие search://updated");
        let file_item = updated
            .items
            .iter()
            .find(|i| i.provider == "files")
            .expect("файловый элемент в событии");
        assert_eq!(file_item.title, r"Доклад о бюджете.pdf");
        assert!(updated.query_id >= resp.query_id);

        // run_item файла → OpenPath (ShellExecute: файла не существует → ошибка
        // действия, НО usage уже записан только после успеха... проверяем оба исхода):
        let r = svc.run_item(&file_item.id);
        if r.is_ok() {
            assert_eq!(svc.db.used_count(&file_item.id).unwrap(), 1, "usage++ после успеха");
        } else {
            assert!(matches!(r, Err(SearchError::Action { .. })), "ошибка исполнения, не NotFound");
            assert_eq!(svc.db.used_count(&file_item.id).unwrap(), 0);
        }

        // recents.json записан (снимок)
        assert!(dir.join("recents.json").exists() || r.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Пустой запрос → recents (после usage++ элемент появляется в выдаче "").
    #[test]
    fn empty_query_returns_recents_after_use() {
        let (svc, _sink, dir) = test_service("recents");
        // Кладём элемент в реестр напрямую (эквивалент «виденного» в сессии)
        // и отмечаем использование в БД — как после run_item.
        let item = SearchItem::new(
            "apps:fake",
            "apps",
            "Фейк Приложение",
            ItemAction::OpenPath { path: r"C:\нет\такого.exe".to_string() },
        );
        svc.remember(std::slice::from_ref(&item));
        svc.db.record_use("apps:fake", 123_456).unwrap();

        let resp = svc.search("");
        assert!(
            resp.items.iter().any(|i| i.id == "apps:fake"),
            "recents на пустой запрос: элемент с usage должен быть в выдаче"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// run_item с неизвестным id → NotFound (действие не выполняется).
    #[test]
    fn run_item_unknown_id_is_not_found() {
        let (svc, _sink, dir) = test_service("notfound");
        assert_eq!(svc.run_item("apps:missing"), Err(SearchError::NotFound));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Recents переживают рестарт: снимок сохраняется и восстанавливается.
    #[test]
    fn recents_snapshot_survives_restart() {
        let (svc, _sink, dir) = test_service("snapshot");
        let item = SearchItem::new(
            "apps:keepme",
            "apps",
            "Долгожитель",
            ItemAction::OpenPath { path: r"C:\x.exe".to_string() },
        );
        svc.remember(std::slice::from_ref(&item));
        svc.db.record_use("apps:keepme", 42).unwrap();
        svc.save_recents();
        drop(svc);

        // «Рестарт»: новый сервис на том же каталоге.
        let sink2 = Arc::new(VecSink::default());
        let svc2 = SearchService::open(
            ServicePaths { data_dir: dir.clone(), index_roots: vec![], apps: vec![] },
            sink2,
        );
        let resp = svc2.search("");
        assert!(
            resp.items.iter().any(|i| i.id == "apps:keepme"),
            "после рестарта recents восстановлены из recents.json"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Индекс-воркер: прогон по пустому корню завершается фазой Done,
    /// статусы уходят в sink и читаются get_index_status().
    #[test]
    fn index_worker_reports_done_phase() {
        let (svc, sink, dir) = test_service("indexdone");
        svc.run_index(true); // явный полный прогон по пустым корням
        let st = svc.index_status();
        assert_eq!(st.phase, IndexPhase::Done, "последний статус — Done: {st:?}");
        assert!(sink.progress.lock().unwrap().iter().any(|p| p.phase == IndexPhase::Done));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D7: decorate_icons ставит путь из кэша, когда PNG уже извлечён.
    #[test]
    fn icons_from_cache_when_present() {
        let (svc, _sink, dir) = test_service("icons");
        let target = r"C:\fake\target.exe";
        let item = SearchItem::new(
            format!("apps:{}", target.to_lowercase()),
            "apps",
            "Фейк",
            ItemAction::LaunchApp { path: target.to_string(), args: None },
        );
        // Готовим кэш вручную: {hash}.png от источника (icon_source map пуста → target).
        std::fs::create_dir_all(&svc.icon_dir).unwrap();
        let png = dir.join("icons").join(icons::cache_file_name(target));
        std::fs::write(&png, b"png").unwrap();

        let mut items = vec![item];
        svc.decorate_icons(&mut items);
        assert_eq!(items[0].icon_path.as_deref(), Some(png.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
