// Phase-0 spike for docs/specs/2026-09-17-windows-raycast-clone-spec.md (§4.2: решения 1 и 7)
// Проверяет: Tauri 2 shell, frameless-окно 720x480, mica, hide-on-focus-loss,
// глобальный хоткей Alt+Space c инструментированием латентности, bench-режим,
// WM_CLIPBOARDUPDATE через windows-rs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicIsize, AtomicUsize, Ordering},
        mpsc::{Receiver, Sender},
        OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tauri::{Emitter, Manager, PhysicalPosition, WebviewWindow, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use windows::Win32::{
    Foundation::{HGLOBAL, HWND, LPARAM, LRESULT, WPARAM},
    System::{
        DataExchange::{
            AddClipboardFormatListener, CloseClipboard, GetClipboardData,
            IsClipboardFormatAvailable, OpenClipboard,
        },
        Memory::{GlobalLock, GlobalSize, GlobalUnlock},
        Ole::{CF_DIB, CF_UNICODETEXT},
    },
    UI::{
        Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
            KEYEVENTF_KEYUP, VK_CONTROL, VK_F9, VK_MENU, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallWindowProcW, DefWindowProcW, SetWindowLongPtrW, GWLP_WNDPROC,
            WM_CLIPBOARDUPDATE,
        },
    },
};

const HOTKEY_SHOW: &str = "Alt+Space";
// Фолбэк, если Alt+Space занят другой программой (напр. PowerToys Run).
const HOTKEY_SHOW_FALLBACK: &str = "Ctrl+Alt+Space";
// Второй хоткей без системного смысла — для bench-режима (SendInput реальных клавиш).
const HOTKEY_BENCH: &str = "Ctrl+Alt+F9";

static ITER: AtomicUsize = AtomicUsize::new(0);
static OLD_WNDPROC: AtomicIsize = AtomicIsize::new(0);
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

type RawWndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

fn base_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn latency_log() -> PathBuf {
    base_dir().join("latency.log")
}
fn clipboard_log() -> PathBuf {
    base_dir().join("clipboard.log")
}
fn runtime_log() -> PathBuf {
    base_dir().join("runtime.log")
}

fn log_line(path: &PathBuf, line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

/// t0 = вход в обработчик (как можно ближе к моменту получения WM_HOTKEY),
/// t1 = после применения position (центр по X / верх по Y основного монитора) + show + focus.
fn show_launcher(app: &tauri::AppHandle, bench_tx: Option<&Sender<u64>>) {
    let t0 = Instant::now();
    if let Some(win) = app.get_webview_window("main") {
        if let (Ok(Some(mon)), Ok(outer)) = (win.primary_monitor(), win.outer_size()) {
            let mp = mon.position();
            let ms = mon.size();
            let x = mp.x + (ms.width as i32 - outer.width as i32) / 2;
            let y = mp.y;
            let _ = win.set_position(PhysicalPosition::new(x, y));
        }
        let _ = win.show();
        let _ = win.set_focus();
    }
    let delta_us = t0.elapsed().as_micros() as u64;
    let iter = ITER.fetch_add(1, Ordering::Relaxed);
    log_line(&latency_log(), &format!("{iter},{delta_us}"));
    if let Some(tx) = bench_tx {
        let _ = tx.send(delta_us);
    }
}

// ---------- clipboard (решение 7: AddClipboardFormatListener + подмена wndproc) ----------

fn install_clipboard_listener(win: &WebviewWindow) {
    let Ok(h) = win.hwnd() else {
        log_line(&runtime_log(), "clipboard: no hwnd");
        return;
    };
    let hwnd = HWND(h.0);
    unsafe {
        if let Err(e) = AddClipboardFormatListener(hwnd) {
            log_line(&runtime_log(), &format!("AddClipboardFormatListener FAILED: {e}"));
            return;
        }
        let old = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, clipboard_wndproc as RawWndProc as usize as isize);
        OLD_WNDPROC.store(old, Ordering::Relaxed);
        log_line(&runtime_log(), "clipboard listener installed (wndproc subclassed)");
    }
}

unsafe extern "system" fn clipboard_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        handle_clipboard_update(hwnd);
    }
    let old = OLD_WNDPROC.load(Ordering::Relaxed);
    if old != 0 {
        CallWindowProcW(Some(std::mem::transmute::<isize, RawWndProc>(old)), hwnd, msg, wp, lp)
    } else {
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

unsafe fn handle_clipboard_update(hwnd: HWND) {
    let mut opened = false;
    for _ in 0..10 {
        if OpenClipboard(Some(hwnd)).is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if !opened {
        log_line(&runtime_log(), "clipboard: OpenClipboard failed after retries");
        return;
    }
    let ts = now_ms();
    if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() {
        match GetClipboardData(CF_UNICODETEXT.0 as u32) {
            Ok(h) => {
                let hg = HGLOBAL(h.0);
                let bytes = GlobalSize(hg);
                let text = {
                    let ptr = GlobalLock(hg) as *const u16;
                    if ptr.is_null() {
                        String::new()
                    } else {
                        let mut len = 0usize;
                        while *ptr.add(len) != 0 {
                            len += 1;
                        }
                        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
                    }
                };
                let _ = GlobalUnlock(hg);
                log_line(&clipboard_log(), &format!("{ts},text,{bytes}"));
                if let Some(app) = APP.get() {
                    let _ = app.emit("clipboard-text", &text);
                }
            }
            Err(e) => log_line(&runtime_log(), &format!("clipboard text: GetClipboardData err {e}")),
        }
    } else if IsClipboardFormatAvailable(CF_DIB.0 as u32).is_ok() {
        match GetClipboardData(CF_DIB.0 as u32) {
            Ok(h) => {
                let bytes = GlobalSize(HGLOBAL(h.0));
                log_line(&clipboard_log(), &format!("{ts},image,{bytes}"));
            }
            Err(e) => log_line(&runtime_log(), &format!("clipboard image: GetClipboardData err {e}")),
        }
    } else {
        log_line(&clipboard_log(), &format!("{ts},other,0"));
    }
    let _ = CloseClipboard();
}

// ---------- bench (SendInput реального хоткея Ctrl+Alt+F9 → полный системный путь) ----------

fn send_ctrl_alt_f9() {
    unsafe {
        let mk = |vk: VIRTUAL_KEY, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let seq = [
            mk(VK_CONTROL, false),
            mk(VK_MENU, false),
            mk(VK_F9, false),
            mk(VK_F9, true),
            mk(VK_MENU, true),
            mk(VK_CONTROL, true),
        ];
        SendInput(&seq, std::mem::size_of::<INPUT>() as i32);
    }
}

fn run_bench(app: tauri::AppHandle, n: usize, rx: Receiver<u64>) {
    std::thread::sleep(Duration::from_millis(700));
    log_line(&runtime_log(), &format!("bench start n={n}"));
    let win = app.get_webview_window("main");
    for i in 0..n {
        send_ctrl_alt_f9();
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(_) => {}
            Err(e) => {
                log_line(&runtime_log(), &format!("bench iter {i}: recv err {e}"));
                break;
            }
        }
        // Прячем окно, чтобы каждая итерация мерила путь hidden → shown (как в реальном use-case).
        if let Some(w) = &win {
            let _ = w.hide();
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    log_line(&runtime_log(), "bench done");
    app.exit(0);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bench = args
        .iter()
        .position(|a| a == "--bench")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<usize>().ok());

    let (tx, rx) = std::sync::mpsc::channel::<u64>();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let show: Shortcut = HOTKEY_SHOW.parse().expect("parse show hotkey");
                        let show_fb: Shortcut = HOTKEY_SHOW_FALLBACK.parse().expect("parse show fallback");
                        let bench_hotkey: Shortcut = HOTKEY_BENCH.parse().expect("parse bench hotkey");
                        if shortcut == &show || shortcut == &show_fb || shortcut == &bench_hotkey {
                            show_launcher(app, Some(&tx));
                        }
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let _ = APP.set(app.handle().clone());
            log_line(&runtime_log(), &format!("startup pid={}", std::process::id()));

            let win = app.get_webview_window("main").expect("main window");

            // Mica backdrop (Win11). Результат пишем в runtime.log — это квирк-диагностика.
            match window_vibrancy::apply_mica(&win, None) {
                Ok(()) => log_line(&runtime_log(), "mica: applied"),
                Err(e) => log_line(&runtime_log(), &format!("mica: FAILED: {e}")),
            }

            install_clipboard_listener(&win);

            // Регистрацию хоткеев не роняем через `?`: занятый хоткей — не повод убивать
            // приложение. Ошибку пишем в runtime.log и stderr; для Alt+Space пробуем фолбэк.
            match app.global_shortcut().register(HOTKEY_SHOW) {
                Ok(()) => log_line(&runtime_log(), "hotkey registered: Alt+Space"),
                Err(e) => {
                    log_line(&runtime_log(), &format!("hotkey Alt+Space FAILED: {e}"));
                    eprintln!("hotkey Alt+Space FAILED: {e}");
                    match app.global_shortcut().register(HOTKEY_SHOW_FALLBACK) {
                        Ok(()) => log_line(&runtime_log(), "hotkey fallback registered: Ctrl+Alt+Space"),
                        Err(e2) => {
                            log_line(&runtime_log(), &format!("hotkey fallback Ctrl+Alt+Space FAILED: {e2}"));
                            eprintln!("hotkey fallback Ctrl+Alt+Space FAILED: {e2}");
                        }
                    }
                }
            }
            if let Err(e) = app.global_shortcut().register(HOTKEY_BENCH) {
                log_line(&runtime_log(), &format!("hotkey Ctrl+Alt+F9 FAILED: {e}"));
                eprintln!("hotkey Ctrl+Alt+F9 FAILED: {e}");
            }
            log_line(&runtime_log(), "hotkey registration done");

            if let Some(n) = bench {
                let handle = app.handle().clone();
                std::thread::spawn(move || run_bench(handle, n, rx));
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Скрытие при потере фокуса — поведение лончера.
            if let WindowEvent::Focused(false) = event {
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
