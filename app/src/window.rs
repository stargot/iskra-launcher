//! Окно лончера: показ на активном мониторе (центр-верх, физические px —
//! DPI-корректно по построению: rect монитора из GetMonitorInfoW), toggle,
//! hide-on-blur, bench-инструментирование латентности.
//! Порядок показа из спайка: позиция (до set_focus!) → show → set_focus.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tauri::{AppHandle, Manager, PhysicalPosition, WebviewWindow, WindowEvent};

use iskra_core::logging;

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
