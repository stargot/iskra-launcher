//! Bench-режим `--bench N` (порт из spikes/tauri-app): приложение само шлёт
//! Ctrl+Alt+F9 через SendInput (реальные системные события → полный путь
//! RegisterHotKey → обработчик), окно скрывается между итерациями (hidden→shown),
//! латентность пишется в logs/latency.log как микросекунды «вход в обработчик —
//! position+show+focus». По завершении приложение завершается (код 0).

use std::sync::mpsc::Receiver;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use iskra_core::logging;

/// Прогрев перед первым нажатием: даём webview загрузиться.
const WARMUP_MS: u64 = 700;
/// Пауза между итерациями (окно успевает скрыться).
const ITER_PAUSE_MS: u64 = 60;
/// Таймаут ожидания обработки нажатия.
const ITER_TIMEOUT_SECS: u64 = 3;

/// Разобрать `--bench N` из аргументов командной строки.
pub fn parse_bench_arg() -> Option<usize> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--bench")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<usize>().ok())
}

/// Запустить bench-поток (вызывается из setup после manage состояния).
pub fn spawn(app: AppHandle, n: usize, rx: Receiver<u64>) {
    std::thread::spawn(move || run(app, n, rx));
}

fn run(app: AppHandle, n: usize, rx: Receiver<u64>) {
    std::thread::sleep(Duration::from_millis(WARMUP_MS));
    logging::info(&format!("bench start n={n}"));
    let win = app.get_webview_window("main");
    for i in 0..n {
        iskra_sys::keys::send_ctrl_alt_f9();
        match rx.recv_timeout(Duration::from_secs(ITER_TIMEOUT_SECS)) {
            Ok(delta_us) => logging::info(&format!("bench iter {i}: {delta_us} µs")),
            Err(err) => {
                logging::warn(&format!("bench iter {i}: recv err {err}"));
                break;
            }
        }
        // Прячем окно: каждая итерация мерит путь hidden → shown (реальный use-case).
        if let Some(w) = &win {
            let _ = w.hide();
        }
        std::thread::sleep(Duration::from_millis(ITER_PAUSE_MS));
    }
    logging::info("bench done");
    app.exit(0);
}
