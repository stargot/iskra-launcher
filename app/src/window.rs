//! Окно лончера: показ на активном мониторе (центр-верх, физические px —
//! DPI-корректно по построению: rect монитора из GetMonitorInfoW), toggle,
//! hide-on-blur, bench-инструментирование латентности.
//! Порядок показа из спайка: позиция (до set_focus!) → show → set_focus.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewWindow, WindowEvent};

use iskra_core::logging;
use iskra_core::WindowMode;

use crate::AppState;

/// Счётчик итераций bench (строки latency.log: `iter,delta_us`).
static BENCH_ITER: AtomicUsize = AtomicUsize::new(0);

/// Почему показываем: обычное нажатие/трей — без инструментирования;
/// Bench — мерим латентность и уведомляем bench-поток.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowReason {
    User,
    Bench,
}

/// Показать окно (с инструментированием в bench-режиме).
pub fn show(app: &AppHandle, reason: ShowReason) {
    let t0 = Instant::now();
    if let Some(win) = app.get_webview_window("main") {
        show_window(&win);
    }
    if reason == ShowReason::Bench {
        let delta_us = t0.elapsed().as_micros() as u64;
        let iter = BENCH_ITER.fetch_add(1, Ordering::Relaxed);
        logging::append("latency.log", &format!("{iter},{delta_us}"));
        if let Some(state) = app.try_state::<AppState>() {
            if let Some(tx) = &state.bench_tx {
                let _ = tx.send(delta_us);
            }
        }
    }
}

/// Toggle из обработчика хоткея: видимо — скрыть, иначе показать.
pub fn toggle(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
    } else {
        show_window(&win);
    }
}

/// Скрытие при потере фокуса — поведение лончера (порт из спайка).
pub fn handle_window_event(window: &tauri::Window, event: &WindowEvent) {
    if let WindowEvent::Focused(false) = event {
        let _ = window.hide();
    }
}

/// Путь показа: rect foreground-монитора (ДО set_focus — после фокуса foreground
/// станет наше окно) → set_position(Physical) → show → set_focus. Если монитор
/// не определился — показываем на прежней позиции (лучше, чем не показать).
fn show_window(win: &WebviewWindow) {
    if let Some(mon) = iskra_sys::monitor::foreground_monitor_rect() {
        if let Ok(outer) = win.outer_size() {
            let x = mon.x + (mon.width as i32 - outer.width as i32) / 2;
            let y = mon.y;
            let _ = win.set_position(PhysicalPosition::new(x, y));
        }
    }
    let _ = win.show();
    let _ = win.set_focus();
}

/// Базовый логический размер окна — СИНХРОННО с app/tauri.conf.json (контракт:
/// Normal = 720×480; Double = 1440×960 = 2×). Менять только парой.
const BASE_W: f64 = 720.0;
const BASE_H: f64 = 480.0;

/// Применить режим размера окна (прогон 1 D2/D3). Вызовы: setup в main.rs
/// (старт) и update_settings (смена на лету). Ошибки не роняют ни старт, ни
/// команду — только лог (настройка уже сохранена, режим применится при
/// следующем старте).
pub fn apply_mode(win: &WebviewWindow, mode: WindowMode) {
    match mode {
        WindowMode::Fullscreen => {
            if let Err(e) = win.set_fullscreen(true) {
                logging::warn(&format!("window: set_fullscreen FAILED: {e}"));
            }
        }
        // Сначала выходим из fullscreen (иначе размер игнорируется ОС), затем размер.
        WindowMode::Normal => {
            if let Err(e) = win.set_fullscreen(false) {
                logging::warn(&format!("window: set_fullscreen(false) FAILED: {e}"));
            }
            set_size_clamped(win, BASE_W, BASE_H);
        }
        WindowMode::Double => {
            if let Err(e) = win.set_fullscreen(false) {
                logging::warn(&format!("window: set_fullscreen(false) FAILED: {e}"));
            }
            set_size_clamped(win, BASE_W * 2.0, BASE_H * 2.0);
        }
    }
}

/// set_size (логические px) с клампом физического размера в work area монитора
/// окна (D3: 2× не должен вылезать за экран на 1366×768/150% DPI) и recenter
/// по монитору окна (формула show_window). Программный set_size работает при
/// resizable:false — tao 0.35 идёт через SetWindowPos(SWP_NOMOVE|NOACTIVATE),
/// не зависящий от WS_THICKFRAME (проверено по исходнику; ручной чек — п. 1–2
/// чек-листа приёмки).
fn set_size_clamped(win: &WebviewWindow, logical_w: f64, logical_h: f64) {
    let scale = win.scale_factor().unwrap_or(1.0);
    let mon = window_monitor_rect(win);
    let mut phys_w = logical_w * scale;
    let mut phys_h = logical_h * scale;
    if let Some(rc) = mon {
        phys_w = phys_w.min(rc.width as f64);
        phys_h = phys_h.min(rc.height as f64);
    }
    if let Err(e) = win.set_size(PhysicalSize::new(phys_w.round() as u32, phys_h.round() as u32)) {
        logging::warn(&format!("window: set_size FAILED: {e}"));
        return;
    }
    if let (Some(rc), Ok(outer)) = (mon, win.outer_size()) {
        let x = rc.x + (rc.width as i32 - outer.width as i32) / 2;
        let y = rc.y;
        let _ = win.set_position(PhysicalPosition::new(x, y));
    }
}

/// Rect work area монитора, где живёт окно (физические px, rcWork — без
/// таскбара: 2× не накрывает панель задач); None — окно/монитор не определились
/// (кламп тогда не применяется — поведение как раньше).
fn window_monitor_rect(win: &WebviewWindow) -> Option<iskra_sys::monitor::Rect> {
    // tauri тянет windows 0.61, workspace — 0.62: конверсия по raw-указателю
    // (HWND — кортежная структура вокруг *mut c_void в обеих версиях).
    let hwnd = win.hwnd().ok()?;
    iskra_sys::monitor::monitor_work_rect_from_hwnd(windows::Win32::Foundation::HWND(hwnd.0))
}
