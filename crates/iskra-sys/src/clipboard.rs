//! Клипборд (шаг 2 Фазы 3): слушатель изменений, чтение форматов, запись, владелец.
//!
//! - **D1 (слушатель):** отдельный поток с message-only окном (`CreateWindowExW`
//!   с родителем `HWND_MESSAGE`) + `AddClipboardFormatListener`; `WM_CLIPBOARDUPDATE`
//!   → mpsc-канал. Окно Tauri НЕ подклассируется — нет квирков с их wndproc
//!   (паттерн «поток + окно» доказан спайком spikes/tauri-app).
//! - **D2 (форматы):** `CF_UNICODETEXT` → String, `CF_DIB`/`CF_DIBV5` → PNG
//!   (image 0.25; 32/24bpp, top-down/bottom-up; остальное — `Skipped` с причиной),
//!   `CF_HDROP` → Vec<OsString>. Порядок приоритета при чтении: текст → картинка → файлы.
//! - **D6 (запись для вставки):** `set_text`/`set_image`/`set_files` — то, что
//!   app-слой кладёт в клипборд перед SendInput Ctrl+V.
//! - **D7 (исключения):** `owner_process_name()` — GetClipboardOwner → pid →
//!   имя процесса (QueryFullProcessImageNameW, lowercase basename).
//!
//! Риск 1: `OpenClipboard` конкуренция — retry 5×20 мс, дальше Err (вызывающий
//! пропускает событие с логом, потеря одной записи не критична).
//! Риск 3: CF_DIB-кворки — поддержаны только BI_RGB 32/24bpp; остальное — skip+причина.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, WPARAM,
};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, CountClipboardFormats, EmptyClipboard,
    EnumClipboardFormats, GetClipboardData, GetClipboardOwner, IsClipboardFormatAvailable,
    OpenClipboard, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowThreadProcessId, PostThreadMessageW, RegisterClassW, TranslateMessage,
    UnregisterClassW, HCURSOR, HICON, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CLIPBOARDUPDATE, WM_QUIT, WNDCLASSW, WNDCLASS_STYLES,
};

/// Сколько раз повторяем `OpenClipboard`, пока другой процесс его держит (риск 1).
pub const OPEN_RETRIES: u32 = 5;
/// Пауза между попытками `OpenClipboard`, мс (риск 1: 5×20 мс).
pub const OPEN_RETRY_DELAY_MS: u64 = 20;

// ---------------------------------------------------------------------------
// Слушатель (D1): поток + message-only окно + mpsc-канал
// ---------------------------------------------------------------------------

/// Событие от слушателя клипборда. Содержимое не несёт — чтение делается
/// отдельным вызовом [`read`] на потоке-владельце (не блокируя окно сообщений).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardEvent {
    /// Содержимое клипборда изменилось (WM_CLIPBOARDUPDATE).
    Updated,
}

/// Живой слушатель клипборда. Создаётся [`start_listener`], владеет потоком.
///
/// `stop()` — для владельца потока (app-сервис): шлёт WM_QUIT в поток и ждёт
/// завершения (окно уничтожено, listener снят, класс разрегистрирован).
/// Без `stop()` (Drop) — best-effort WM_QUIT без join (не блокируем дроп).
#[derive(Debug)]
pub struct ClipboardListener {
    events_rx: Receiver<ClipboardEvent>,
    thread: Option<JoinHandle<()>>,
    /// None после успешного stop() — чтобы Drop не слал WM_QUIT в переиспользованный
    /// системой thread id.
    thread_id: Option<u32>,
}

// Sender события кладём в thread-local потока слушателя: wndproc вызывается
// только в его потоке, поэтому отдельная статика/GWLP_USERDATA не нужны.
thread_local! {
    static LISTENER_TX: std::cell::RefCell<Option<Sender<ClipboardEvent>>> =
        const { std::cell::RefCell::new(None) };
}

static CLASS_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Запустить поток слушателя: message-only окно + `AddClipboardFormatListener`.
/// Блокируется до готовности (окно создано и слушатель повешен) или до ошибки.
pub fn start_listener() -> io::Result<ClipboardListener> {
    let (events_tx, events_rx) = mpsc::channel::<ClipboardEvent>();
    // handshake инициализации: Result<thread_id> из потока (sync_channel на 1).
    let (init_tx, init_rx) = mpsc::sync_channel::<io::Result<u32>>(1);
    let thread = std::thread::Builder::new()
        .name("iskra-clipboard".into())
        .spawn(move || listener_thread(events_tx, init_tx))
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("spawn listener: {e}")))?;

    let thread_id = init_rx.recv().map_err(|_| {
        io::Error::new(io::ErrorKind::Other, "поток слушателя умер до инициализации")
    })??;

    Ok(ClipboardListener { events_rx, thread: Some(thread), thread_id: Some(thread_id) })
}

impl ClipboardListener {
    /// Приёмник событий. Блокирующий recv / recv_timeout — на усмотрение владельца.
    pub fn events(&self) -> &Receiver<ClipboardEvent> {
        &self.events_rx
    }

    /// Остановить: WM_QUIT в поток → поток снимает слушатель, уничтожает окно,
    /// разрегистрирует класс и завершается; join гарантирует завершение очистки.
    /// Повторный drop/stop после этого WM_QUIT уже не шлют.
    pub fn stop(&mut self) -> io::Result<()> {
        if let Some(thread_id) = self.thread_id.take() {
            // Поток мог уже умереть (тогда post вернёт ошибку — не важно, join всё равно ждём).
            let _ = unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| {
                io::Error::new(io::ErrorKind::Other, "поток слушателя паниковал")
            })?;
        }
        Ok(())
    }
}

impl Drop for ClipboardListener {
    fn drop(&mut self) {
        // Владелец не позвал stop() — просим завершение, join не делаем (не блокируем).
        if let Some(thread_id) = self.thread_id.take() {
            let _ = unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
    }
}

fn next_class_name() -> String {
    let n = CLASS_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("iskra_clipboard_{}_{}", std::process::id(), n)
}

fn listener_thread(events_tx: Sender<ClipboardEvent>, init: SyncSender<io::Result<u32>>) {
    // Уникальное имя класса: несколько старт/стоп подряд (тесты, restart фичи)
    // не должны сталкиваться с уже зарегистрированным классом.
    let class_name = HSTRING::from(next_class_name());

    LISTENER_TX.with(|slot| *slot.borrow_mut() = Some(events_tx));

    let init_err = |op: &str, e: String| {
        let _ = init.send(Err(io::Error::new(
            io::ErrorKind::Other,
            format!("listener: {op}: {e}"),
        )));
    };

    let hinstance = match unsafe { GetModuleHandleW(PCWSTR::null()) } {
        Ok(m) => windows::Win32::Foundation::HINSTANCE(m.0),
        Err(e) => {
            init_err("GetModuleHandleW", e.to_string());
            return;
        }
    };

    let wc = WNDCLASSW {
        style: WNDCLASS_STYLES(0),
        lpfnWndProc: Some(clipboard_wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: HICON(std::ptr::null_mut()),
        hCursor: HCURSOR(std::ptr::null_mut()),
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        let e = io::Error::last_os_error();
        init_err("RegisterClassW", e.to_string());
        return;
    }

    // Сообщение-only окно: родитель HWND_MESSAGE, его не видно и оно не в Z-порядке.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )
    };
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(e) => {
            let _ = unsafe { UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance)) };
            init_err("CreateWindowExW", e.to_string());
            return;
        }
    };

    if let Err(e) = unsafe { AddClipboardFormatListener(hwnd) } {
        unsafe {
            let _ = DestroyWindow(hwnd);
            let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance));
        }
        init_err("AddClipboardFormatListener", e.to_string());
        return;
    }

    let thread_id = unsafe { GetCurrentThreadId() };
    if init.send(Ok(thread_id)).is_err() {
        // Владелец умер, не дождавшись — прибраться и уйти.
        unsafe {
            let _ = RemoveClipboardFormatListener(hwnd);
            let _ = DestroyWindow(hwnd);
            let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance));
        }
        return;
    }

    // Цикл сообщений: 0 = WM_QUIT, -1 = ошибка — в обоих случаях выходим.
    let mut msg = MSG::default();
    loop {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Очистка при любом завершении (stop(), Drop владельца, ошибка цикла).
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
        let _ = DestroyWindow(hwnd);
        let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance));
    }
}

unsafe extern "system" fn clipboard_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        LISTENER_TX.with(|slot| {
            if let Some(tx) = slot.borrow().as_ref() {
                let _ = tx.send(ClipboardEvent::Updated);
            }
        });
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

// ---------------------------------------------------------------------------
// Чтение (D2)
// ---------------------------------------------------------------------------

/// Содержимое клипборда в формате MVP (D2). `Skipped` — данные есть, но формат
/// не поддержан (причина в строке — риск 3: «остальное — skip + причина»).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    /// CF_UNICODETEXT.
    Text(String),
    /// CF_DIB/CF_DIBV5, перекодированные в PNG (+ размеры в пикселях).
    Image { png: Vec<u8>, width: u32, height: u32 },
    /// CF_HDROP — список путей.
    Files(Vec<OsString>),
    /// Формат не из D2 (CF_HTML, палитра, 16bpp, BI_BITFIELDS и пр.).
    Skipped(String),
}

enum ReadRaw {
    Text(String),
    Dib(Vec<u8>),
    Files(Vec<OsString>),
    Skipped(String),
}

/// Прочитать содержимое клипборда (приоритет D2: текст → картинка → файлы).
/// `Ok(Skipped(reason))` — поддержанных форматов нет (это не ошибка).
/// `Err` — клипборд не открылся после retry или данные нечитаемы (риск 1).
pub fn read() -> io::Result<ClipboardContent> {
    open_retry(None)?;
    let raw = unsafe { read_opened() };
    let _ = unsafe { CloseClipboard() };
    match raw? {
        ReadRaw::Text(s) => Ok(ClipboardContent::Text(s)),
        ReadRaw::Files(v) => Ok(ClipboardContent::Files(v)),
        ReadRaw::Dib(bytes) => match parse_dib(&bytes) {
            Ok((png, width, height)) => Ok(ClipboardContent::Image { png, width, height }),
            Err(e) => Ok(ClipboardContent::Skipped(format!("CF_DIB пропущен: {e}"))),
        },
        ReadRaw::Skipped(reason) => Ok(ClipboardContent::Skipped(reason)),
    }
}

/// Тело чтения — вызывать только между OpenClipboard/CloseClipboard.
///
/// # Safety
/// Требует открытого клипборда; работает с сырыми GlobalLock-указателями.
unsafe fn read_opened() -> io::Result<ReadRaw> {
    if CountClipboardFormats() == 0 {
        return Ok(ReadRaw::Skipped("клипборд пуст".into()));
    }
    if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() {
        return Ok(ReadRaw::Text(read_text_locked()?));
    }
    let dib_format = if IsClipboardFormatAvailable(CF_DIB.0 as u32).is_ok() {
        Some(CF_DIB)
    } else if IsClipboardFormatAvailable(CF_DIBV5.0 as u32).is_ok() {
        Some(CF_DIBV5)
    } else {
        None
    };
    if let Some(f) = dib_format {
        return Ok(ReadRaw::Dib(read_binary_locked(f.0 as u32)?));
    }
    if IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() {
        return Ok(ReadRaw::Files(read_files_locked()?));
    }
    let formats = formats_locked();
    Ok(ReadRaw::Skipped(format!("формат(ы) не из D2: {formats:?}")))
}

/// Текст CF_UNICODETEXT: GlobalSize → u16-срез до NUL → lossy-конверсия
/// (квирка спайка: размер включает NUL, но битые записи встречаются — scans bounded).
///
/// # Safety
/// Только при открытом клипборде после успешного IsClipboardFormatAvailable.
unsafe fn read_text_locked() -> io::Result<String> {
    let h = GetClipboardData(CF_UNICODETEXT.0 as u32)
        .map_err(|e| err("GetClipboardData(CF_UNICODETEXT)", e))?;
    let hg = HGLOBAL(h.0);
    let size = GlobalSize(hg);
    if size == 0 {
        return Ok(String::new());
    }
    let ptr = GlobalLock(hg);
    if ptr.is_null() {
        return Err(io::Error::new(io::ErrorKind::Other, "GlobalLock(CF_UNICODETEXT) failed"));
    }
    let units = std::slice::from_raw_parts(ptr as *const u16, size / 2);
    let mut len = 0;
    while len < units.len() && units[len] != 0 {
        len += 1;
    }
    let text = String::from_utf16_lossy(&units[..len]);
    let _ = GlobalUnlock(hg);
    Ok(text)
}

/// Сырые байты формат-блока (копия — память клипборда умирает на CloseClipboard).
///
/// # Safety
/// Только при открытом клипборде после успешного IsClipboardFormatAvailable.
unsafe fn read_binary_locked(format: u32) -> io::Result<Vec<u8>> {
    let h = GetClipboardData(format).map_err(|e| err("GetClipboardData(DIB)", e))?;
    let hg = HGLOBAL(h.0);
    let size = GlobalSize(hg);
    if size == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "DIB-блок нулевой длины"));
    }
    let ptr = GlobalLock(hg);
    if ptr.is_null() {
        return Err(io::Error::new(io::ErrorKind::Other, "GlobalLock(DIB) failed"));
    }
    let bytes = std::slice::from_raw_parts(ptr as *const u8, size).to_vec();
    let _ = GlobalUnlock(hg);
    Ok(bytes)
}

/// CF_HDROP → список путей (DragQueryFileW, double-NUL wide-список).
///
/// # Safety
/// Только при открытом клипборде после успешного IsClipboardFormatAvailable.
unsafe fn read_files_locked() -> io::Result<Vec<OsString>> {
    let h = GetClipboardData(CF_HDROP.0 as u32).map_err(|e| err("GetClipboardData(CF_HDROP)", e))?;
    let hdrop = HDROP(h.0);
    let count = DragQueryFileW(hdrop, u32::MAX, None);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let len = DragQueryFileW(hdrop, i, None) as usize;
        if len == 0 {
            out.push(OsString::new());
            continue;
        }
        let mut buf = vec![0u16; len + 1];
        let n = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
        out.push(OsString::from_wide(&buf[..n.min(len)]));
    }
    Ok(out)
}

/// Список идентификаторов форматов (для диагностики в Skipped).
///
/// # Safety
/// Только при открытом клипборде.
unsafe fn formats_locked() -> Vec<u32> {
    let mut out = Vec::new();
    let mut f = 0u32;
    loop {
        f = EnumClipboardFormats(f);
        if f == 0 {
            break;
        }
        out.push(f);
    }
    out
}

// ---------------------------------------------------------------------------
// Запись (D6): то, что app-слой кладёт перед SendInput Ctrl+V
// ---------------------------------------------------------------------------

/// Положить текст (CF_UNICODETEXT) в клипборд (заменяя прежнее содержимое).
pub fn set_text(text: &str) -> io::Result<()> {
    let mut units: Vec<u8> = Vec::with_capacity((text.len() + 1) * 2);
    for u in text.encode_utf16() {
        units.extend_from_slice(&u.to_ne_bytes());
    }
    units.extend_from_slice(&0u16.to_ne_bytes());
    write_payload(CF_UNICODETEXT.0 as u32, &units)
}

/// Положить изображение: PNG-байты → декодирование → CF_DIB (32bpp BGRA,
/// top-down BI_RGB). Истина хранения — PNG (D4); в клипборд идёт DIB, потому
/// что CF_PNG понимают далеко не все приёмники вставки.
pub fn set_image(png: &[u8]) -> io::Result<()> {
    let img = image::load_from_memory(png)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("PNG decode: {e}")))?
        .to_rgba8();
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "PNG нулевого размера"));
    }

    let header = dib_header_32bpp_top_down(w, h);
    let mut payload = Vec::with_capacity(header.len() + img.as_raw().len());
    payload.extend_from_slice(&header);
    // RGBA → BGRA (альфа — прямая: BI_RGB DIB не premultiplied).
    for px in img.as_raw().chunks_exact(4) {
        payload.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    write_payload(CF_DIB.0 as u32, &payload)
}

/// Положить список файлов (CF_HDROP) — для вставки путей из истории.
pub fn set_files(paths: &[OsString]) -> io::Result<()> {
    if paths.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "пустой список файлов"));
    }
    // DROPFILES: pFiles=смещение списка (20), pt, fNC=0, fWide=1 (UTF-16).
    let mut payload = vec![0u8; 20];
    payload[0..4].copy_from_slice(&20u32.to_ne_bytes());
    payload[16..20].copy_from_slice(&1u32.to_ne_bytes());
    for p in paths {
        for u in p.encode_wide() {
            payload.extend_from_slice(&u.to_ne_bytes());
        }
        payload.extend_from_slice(&0u16.to_ne_bytes());
    }
    payload.extend_from_slice(&0u16.to_ne_bytes());
    write_payload(CF_HDROP.0 as u32, &payload)
}

/// 40-байтный BITMAPINFOHEADER: 32bpp BI_RGB, отрицательная высота (top-down,
/// первый байт данных — верхняя строка, как ждёт image-буфер).
fn dib_header_32bpp_top_down(w: u32, h: u32) -> [u8; 40] {
    let mut v = [0u8; 40];
    v[0..4].copy_from_slice(&40u32.to_le_bytes()); // biSize
    v[4..8].copy_from_slice(&(w as i32).to_le_bytes()); // biWidth
    v[8..12].copy_from_slice(&(-(h as i32)).to_le_bytes()); // biHeight < 0 → top-down
    v[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes
    v[14..16].copy_from_slice(&32u16.to_le_bytes()); // biBitCount
    v[16..20].copy_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    v[20..24].copy_from_slice(&0u32.to_le_bytes()); // biSizeImage = 0 (допустимо для BI_RGB)
    v
}

/// Открыть клипборд → EmptyClipboard → SetClipboardData → CloseClipboard.
/// Payload готовится ДО открытия: сбой аллокации/подготовки не трогает прежнее
/// содержимое клипборда (EmptyClipboard — последняя необратимая операция).
/// После успешного SetClipboardData память владеет система (GlobalFree не зовём);
/// при ошибке освобождаем сами. Владелец остаётся NULL (данные — immediate,
/// не delayed rendering — этого хватает для вставки; окно-владелец не нужно).
fn write_payload(format: u32, payload: &[u8]) -> io::Result<()> {
    let hg = unsafe { GlobalAlloc(GMEM_MOVEABLE, payload.len()) }
        .map_err(|e| err("GlobalAlloc", e))?;
    if let Err(prepare) = fill_global(hg, payload) {
        let _ = unsafe { GlobalFree(Some(hg)) };
        return Err(prepare);
    }

    open_retry(None)?;
    let res: io::Result<()> = (|| unsafe {
        EmptyClipboard().map_err(|e| err("EmptyClipboard", e))?;
        if let Err(e) = SetClipboardData(format, Some(HANDLE(hg.0))) {
            return Err(err("SetClipboardData", e));
        }
        Ok(())
    })();
    let _ = unsafe { CloseClipboard() };
    if res.is_err() {
        let _ = unsafe { GlobalFree(Some(hg)) };
    }
    res
}

/// Скопировать payload в свежевыделенный глобальный блок.
fn fill_global(hg: HGLOBAL, payload: &[u8]) -> io::Result<()> {
    unsafe {
        let ptr = GlobalLock(hg);
        if ptr.is_null() {
            return Err(io::Error::new(io::ErrorKind::Other, "GlobalLock(write) failed"));
        }
        std::ptr::copy_nonoverlapping(payload.as_ptr(), ptr.cast::<u8>(), payload.len());
        let _ = GlobalUnlock(hg);
    }
    Ok(())
}

/// `OpenClipboard` с retry 5×20 мс (риск 1: другой процесс может держать его).
fn open_retry(owner: Option<HWND>) -> io::Result<()> {
    let mut last = io::Error::new(io::ErrorKind::Other, "OpenClipboard: попыток не было");
    for _ in 0..OPEN_RETRIES {
        match unsafe { OpenClipboard(owner) } {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = err("OpenClipboard", e);
                std::thread::sleep(Duration::from_millis(OPEN_RETRY_DELAY_MS));
            }
        }
    }
    Err(last)
}

// ---------------------------------------------------------------------------
// Владелец (D7): процесс-источник последней записи
// ---------------------------------------------------------------------------

/// Имя процесса-владельца клипборда (lowercase basename, напр. `"notepad.exe"`)
/// для исключений D7. `None` — владельца нет (данные положены без окна, как
/// делают наши set_*), процесс недоступен или имя нечитаемо.
pub fn owner_process_name() -> Option<String> {
    let hwnd = unsafe { GetClipboardOwner() }.ok()?;
    if hwnd.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if tid == 0 || pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024]; // хватает для обычных путей; сверхдлинные → None
    let mut len = buf.len() as u32;
    let full = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()
    .map(|()| String::from_utf16_lossy(&buf[..len as usize]));
    let _ = unsafe { CloseHandle(handle) };
    base_process_name(&full?)
}

/// Lowercase basename полного пути образа процесса.
fn base_process_name(full_path: &str) -> Option<String> {
    Path::new(full_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_lowercase)
}

// ---------------------------------------------------------------------------
// CF_DIB → PNG (риск 3: 32/24bpp, top-down/bottom-up; остальное — Err с причиной)
// ---------------------------------------------------------------------------

/// Разобрать сырой DIB (BITMAPINFOHEADER [+ палитра] + пиксели) и собрать PNG.
/// Поддержка: BI_RGB, 32/24bpp, положительная (bottom-up) и отрицательная
/// (top-down) высота. Всё остальное — Err с причиной (читатель обернёт в Skipped).
///
/// Квирка альфы (та же, что в icons.rs): многие приложения кладут a=0 во всех
/// пикселях при непрозрачной картинке — считаем такие пиксели непрозрачными.
fn parse_dib(dib: &[u8]) -> io::Result<(Vec<u8>, u32, u32)> {
    let rd_u32 = |off: usize| {
        u32::from_le_bytes(dib[off..off + 4].try_into().expect("срез проверен длиной"))
    };
    let rd_i32 =
        |off: usize| i32::from_le_bytes(dib[off..off + 4].try_into().expect("срез проверен длиной"));
    let rd_u16 = |off: usize| {
        u16::from_le_bytes(dib[off..off + 2].try_into().expect("срез проверен длиной"))
    };

    if dib.len() < 40 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "DIB короче 40 байт"));
    }
    let header_size = rd_u32(0) as usize;
    if !(40..=124).contains(&header_size) || dib.len() < header_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("неожиданный размер заголовка DIB: {header_size}"),
        ));
    }
    let width = rd_i32(4);
    let height_raw = rd_i32(8);
    let bit_count = rd_u16(14) as u32;
    let compression = rd_u32(16);
    let clr_used = rd_u32(32) as usize;

    if width <= 0 || height_raw == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("некорректные размеры DIB: {width}×{height_raw}"),
        ));
    }
    let top_down = height_raw < 0;
    let height = height_raw.unsigned_abs();
    let width = width as u32;

    if compression != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("сжатие {compression} (BI_BITFIELDS/…) не поддерживается"),
        ));
    }
    if bit_count != 32 && bit_count != 24 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("глубина {bit_count}bpp не поддерживается (только 32/24)"),
        ));
    }
    if clr_used != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "палитра не ожидается у 24/32bpp BI_RGB",
        ));
    }

    let bytes_pp = (bit_count / 8) as usize;
    let row_len = width as usize * bytes_pp;
    let stride = row_len.div_ceil(4) * 4; // строки DIB выровнены на 4 байта
    let pixel_off = header_size; // палитра нулевая (clr_used == 0)
    let needed = pixel_off
        .checked_add(stride.checked_mul(height as usize).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "переполнение размеров DIB")
        })?)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "переполнение размеров DIB"))?;
    if dib.len() < needed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("DIB обрезан: есть {} байт, нужно {needed}", dib.len()),
        ));
    }

    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        let src_y = if top_down { y } else { height as usize - 1 - y };
        let src = &dib[pixel_off + src_y * stride..][..row_len];
        let dst = &mut rgba[y * width as usize * 4..][..width as usize * 4];
        match bit_count {
            32 => {
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = s[3];
                }
            }
            _ => {
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(3)) {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = 255;
                }
            }
        }
    }

    // Квирка альфы: все нули → картинка на самом деле непрозрачная.
    let has_alpha = rgba.chunks_exact(4).any(|px| px[3] != 0);
    if !has_alpha {
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }

    let img = image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "буфер не совпал с размерами DIB")
    })?;
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("PNG encode: {e}")))?;
    Ok((png.into_inner(), width, height))
}

fn err(op: &str, e: windows::core::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{op} failed: {e}"))
}

#[cfg(test)]
pub(crate) static CLIPBOARD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// Тесты трогают СИСТЕМНЫЙ клипборд — сериализуем их внутри процесса и
    /// восстанавливаем прежний текст на выходе (если прежнее содержимое было
    /// не текстом — кладём пустую строку; это задокументировано в плане).
    struct ClipGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }

    impl ClipGuard {
        fn new() -> Self {
            let lock = CLIPBOARD_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = match read() {
                Ok(ClipboardContent::Text(s)) => Some(s),
                _ => None,
            };
            Self { _lock: lock, prev }
        }
    }

    impl Drop for ClipGuard {
        fn drop(&mut self) {
            let restore = self.prev.clone().unwrap_or_default();
            let _ = set_text(&restore);
        }
    }

    fn png_bytes(img: &image::RgbaImage) -> Vec<u8> {
        let mut cur = std::io::Cursor::new(Vec::new());
        img.write_to(&mut cur, image::ImageFormat::Png).unwrap();
        cur.into_inner()
    }

    fn dib_header(w: i32, h: i32, bpp: u16, compression: u32, clr_used: u32) -> Vec<u8> {
        let mut v = vec![0u8; 40];
        v[0..4].copy_from_slice(&40u32.to_le_bytes());
        v[4..8].copy_from_slice(&w.to_le_bytes());
        v[8..12].copy_from_slice(&h.to_le_bytes());
        v[12..14].copy_from_slice(&1u16.to_le_bytes());
        v[14..16].copy_from_slice(&bpp.to_le_bytes());
        v[16..20].copy_from_slice(&compression.to_le_bytes());
        v[32..36].copy_from_slice(&clr_used.to_le_bytes());
        v
    }

    /// Roundtrip шага 2: set_text → read. Пустая строка тоже валидна.
    #[test]
    fn text_set_read_roundtrip() {
        let _g = ClipGuard::new();
        let text = "привет, Iskra! 🚀 clipboard";
        set_text(text).expect("set_text");
        match read().expect("read") {
            ClipboardContent::Text(s) => assert_eq!(s, text),
            other => panic!("ожидался Text, получено {other:?}"),
        }
        set_text("").expect("set_text пустой");
        match read().expect("read") {
            ClipboardContent::Text(s) => assert_eq!(s, ""),
            other => panic!("ожидался пустой Text, получено {other:?}"),
        }
    }

    /// Listener (D1): set_text из другого потока → WM_CLIPBOARDUPDATE → канал.
    /// После stop() поток завершён и sender закрыт (Disconnected) — канал чист.
    #[test]
    fn listener_receives_update_and_stops() {
        let _g = ClipGuard::new();
        let mut listener = start_listener().expect("слушатель должен стартовать");

        set_text("iskra listener test").expect("set_text");
        let ev = listener
            .events()
            .recv_timeout(Duration::from_secs(2))
            .expect("событие должно прийти в течение 2 c (план)");
        assert_eq!(ev, ClipboardEvent::Updated);

        listener.stop().expect("stop должен cleanly завершить поток");
        assert!(
            matches!(
                listener.events().recv_timeout(Duration::from_millis(300)),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
            ),
            "после stop sender должен быть закрыт"
        );
    }

    /// CF_HDROP roundtrip: set_files → read → те же пути (tmp-файлы).
    #[test]
    fn files_set_read_roundtrip() {
        let _g = ClipGuard::new();
        let dir = std::env::temp_dir().join(format!("iskra-clip-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp-каталог");
        let f1 = dir.join("alpha.txt");
        let f2 = dir.join("бета.md");
        std::fs::write(&f1, "a").unwrap();
        std::fs::write(&f2, "b").unwrap();

        let paths = vec![f1.clone().into_os_string(), f2.clone().into_os_string()];
        set_files(&paths).expect("set_files");
        match read().expect("read") {
            ClipboardContent::Files(got) => assert_eq!(got, paths, "пути должны совпасть по порядку"),
            other => panic!("ожидались Files, получено {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Запись картинки (PNG → CF_DIB) → чтение (CF_DIB → PNG): сигнатура PNG,
    /// размеры и содержимое совпадают (roundtrip через реальный клипборд).
    #[test]
    fn image_set_read_png_roundtrip() {
        let _g = ClipGuard::new();
        let mut img = image::RgbaImage::new(3, 2);
        img.put_pixel(0, 0, image::Rgba([200, 100, 50, 255]));
        img.put_pixel(2, 1, image::Rgba([10, 20, 30, 255]));
        let src = png_bytes(&img);

        set_image(&src).expect("set_image");
        match read().expect("read") {
            ClipboardContent::Image { png, width, height } => {
                assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG-сигнатура");
                assert_eq!((width, height), (3, 2));
                let decoded = image::load_from_memory(&png).expect("PNG декодируется");
                assert_eq!(decoded.as_rgba8().unwrap().get_pixel(0, 0).0, [200, 100, 50, 255]);
                assert_eq!(decoded.as_rgba8().unwrap().get_pixel(2, 1).0, [10, 20, 30, 255]);
            }
            other => panic!("ожидался Image, получено {other:?}"),
        }
    }

    /// 32bpp bottom-up DIB (положительная высота) → PNG; смешанная альфа
    /// проходит без изменений (fallback «все нули → 255» — отдельный тест ниже).
    #[test]
    fn parse_dib_32bpp_bottom_up() {
        let mut dib = dib_header(2, 2, 32, 0, 0);
        // Данные в BGRA, bottom-up: ПЕРВАЯ строка данных — нижняя.
        // Низ: (0,1)=красный [B=0,G=0,R=255] с a=0, (1,1)=зелёный [B=0,G=255,R=0] с a=0.
        dib.extend_from_slice(&[0, 0, 255, 0]);
        dib.extend_from_slice(&[0, 255, 0, 0]);
        // Верх: (0,0)=синий [B=255,G=0,R=0] a=255, (1,0)=белый a=255.
        dib.extend_from_slice(&[255, 0, 0, 255]);
        dib.extend_from_slice(&[255, 255, 255, 255]);

        let (png, w, h) = parse_dib(&dib).expect("парсинг DIB");
        assert_eq!((w, h), (2, 2));
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 255, 255], "верх-лево = синий");
        // Смешанная альфа проходит как есть (a=0 у зелёного сохраняется):
        assert_eq!(img.get_pixel(1, 1).0, [0, 255, 0, 0]);
    }

    /// Квирка альфы: если a=0 у ВСЕХ пикселей, картинка на деле непрозрачная
    /// (та же эвристика, что в icons.rs) — все альфы заменяются на 255.
    #[test]
    fn parse_dib_all_zero_alpha_becomes_opaque() {
        let mut dib = dib_header(2, 1, 32, 0, 0);
        dib.extend_from_slice(&[10, 20, 30, 0]);
        dib.extend_from_slice(&[40, 50, 60, 0]);
        let (png, _, _) = parse_dib(&dib).expect("парсинг DIB");
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(0, 0).0, [30, 20, 10, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [60, 50, 40, 255]);
    }

    /// Top-down DIB (отрицательная высота): первая строка данных — верхняя.
    #[test]
    fn parse_dib_32bpp_top_down_negative_height() {
        let mut dib = dib_header(1, -2, 32, 0, 0); // h < 0 → top-down
        dib.extend_from_slice(&[10, 20, 30, 255]); // верх (BGRA)
        dib.extend_from_slice(&[40, 50, 60, 255]); // низ
        let (png, w, h) = parse_dib(&dib).expect("парсинг DIB");
        assert_eq!((w, h), (1, 2));
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(0, 0).0, [30, 20, 10, 255]);
        assert_eq!(img.get_pixel(0, 1).0, [60, 50, 40, 255]);
    }

    /// 24bpp с выравниванием строк на 4 байта (2 пикселя = 6 байт + 2 паддинга).
    #[test]
    fn parse_dib_24bpp_row_padding() {
        let mut dib = dib_header(2, 1, 24, 0, 0);
        dib.extend_from_slice(&[1, 2, 3]); // B,G,R
        dib.extend_from_slice(&[4, 5, 6]);
        dib.extend_from_slice(&[0xAA, 0xBB]); // паддинг до 8 байт
        let (png, w, h) = parse_dib(&dib).expect("парсинг DIB");
        assert_eq!((w, h), (2, 1));
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(0, 0).0, [3, 2, 1, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [6, 5, 4, 255]);
    }

    /// Риск 3: неподдержанные варианты DIB отклоняются с причиной
    /// (16bpp, BI_BITFIELDS, палитра, обрезанные данные).
    #[test]
    fn parse_dib_rejects_unsupported() {
        assert!(parse_dib(&dib_header(2, 2, 16, 0, 0)).is_err(), "16bpp");
        assert!(parse_dib(&dib_header(2, 2, 32, 3, 0)).is_err(), "BI_BITFIELDS");
        assert!(parse_dib(&dib_header(2, 2, 32, 0, 4)).is_err(), "палитра");
        let mut truncated = dib_header(4, 4, 32, 0, 0);
        truncated.extend_from_slice(&[0u8; 16]); // нужно 64 байта пикселей
        assert!(parse_dib(&truncated).is_err(), "обрезанный DIB");
        assert!(parse_dib(&[0u8; 39]).is_err(), "короче заголовка");
    }

    /// D7: после наших set_* владелец NULL (owner_process_name → None), а
    /// helper basename режет полный путь и приводит к нижнему регистру.
    /// (Значение Some не проверяем — владелец мог бы дать только сторонний
    /// процесс, и это гонка; вся цепочка OpenProcess+QueryFullProcessImageNameW
    /// покрыта ручной приёмкой шага 3.)
    #[test]
    fn owner_is_none_for_our_writes_and_basename_helper() {
        let _g = ClipGuard::new();
        set_text("owner test").expect("set_text");
        assert_eq!(owner_process_name(), None, "set_* без окна → владелец NULL");

        assert_eq!(
            base_process_name(r"C:\Windows\System32\Notepad.EXE").as_deref(),
            Some("notepad.exe")
        );
        assert_eq!(base_process_name("относительный/путь/Файл.exe").as_deref(), Some("файл.exe"));
        assert_eq!(base_process_name(r"C:\"), None, "имени файла нет → None");
    }
}
