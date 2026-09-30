// Iskra, Фаза 1, шаг 4: app-сервисы — хоткей (HotkeyService), окно на активном
// мониторе + hide-on-blur, трей, команды контракта. Склейка: builder + setup.
// Порты из spikes/tauri-app: mica, hide-on-blur, фолбэк-хоткей, bench.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bench;
mod commands;
mod hotkey;
mod tray;
mod window;

use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;

use tauri::Manager;

use iskra_core::logging;
use iskra_core::Settings;

/// Общее состояние (manage в setup, читают команды и обработчик хоткея).
pub struct AppState {
    /// Текущие настройки (источник истины — settings.json; кэш для быстрых чтений).
    pub settings: Mutex<Settings>,
    /// Сервис глобального хоткея (активный хоткей, фолбэк, remap).
    pub hotkey: hotkey::HotkeyService,
    /// Канал латентности bench-режима; Some только при запуске с `--bench N`.
    pub bench_tx: Option<Sender<u64>>,
}

fn main() {
    // GPU-off в релизе: private WS 64–67 МБ против 98–102 без флагов
    // (spikes/tauri-app/RESULTS.md). set_var безопасен — edition 2021 (риск 6);
    // ставится первой строкой, ДО создания окна Builder'ом. Замер RAM на релизной
    // сборке — обязательная проверка (риск 1: если флаги не подхватились — план Б).
    if !cfg!(debug_assertions) {
        std::env::set_var(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "--disable-gpu --disable-gpu-compositing",
        );
    }

    let bench_n = bench::parse_bench_arg();
    let (bench_tx, bench_rx) = channel::<u64>();
    let settings = Settings::load();
    logging::info(&format!(
        "startup pid={} bench={bench_n:?} settings={settings:?}",
        std::process::id()
    ));
    if bench_n.is_some() {
        // Чистый latency.log на прогон: p95 считается только по итерациям bench.
        logging::reset("latency.log");
    }

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        let Some(state) = app.try_state::<AppState>() else {
                            return; // состояние ещё не manage — хоткеи ещё и не зарегистрированы
                        };
                        if state.hotkey.matches_show(shortcut) {
                            window::toggle(app);
                        } else if state.hotkey.matches_bench(shortcut) {
                            window::show(app, window::ShowReason::Bench);
                        }
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::update_settings,
            commands::get_runtime_info
        ])
        .setup(move |app| {
            let win = app.get_webview_window("main").expect("main window");
            let _ = win.hide();

            // Mica backdrop (Win11). Первый чек шага 4 — визуальный при GPU-off
            // (риск 2): чёрный фон → план Б (отказ от vibrancy в релизе, CSS-фон).
            match window_vibrancy::apply_mica(&win, None) {
                Ok(()) => logging::info("mica: applied"),
                Err(e) => logging::warn(&format!("mica: FAILED: {e}")),
            }

            // Состояние ДО регистрации хоткеев (обработчик читает AppState).
            app.manage(AppState {
                settings: Mutex::new(settings.clone()),
                hotkey: hotkey::HotkeyService::new(),
                bench_tx: bench_n.map(|_| bench_tx),
            });

            // Хоткей из настроек (+ фолбэк при занятом; bench-хоткей при --bench).
            let state = app.state::<AppState>();
            state.hotkey.init(app.handle(), &settings.hotkey, bench_n.is_some());

            // Автозапуск: settings.json — источник истины; сводим реестр к нему.
            let enabled_in_registry = iskra_sys::autostart::is_enabled();
            if enabled_in_registry != settings.autostart {
                logging::info(&format!(
                    "autostart: reconcile registry={enabled_in_registry} -> settings={}",
                    settings.autostart
                ));
                let result = if settings.autostart {
                    iskra_sys::autostart::set()
                } else {
                    iskra_sys::autostart::remove()
                };
                if let Err(e) = result {
                    logging::warn(&format!("autostart: reconcile FAILED: {e}"));
                }
            }

            // Трей (иконка + меню). Не критичен для старта — ошибку логируем.
            if let Err(e) = tray::init(app.handle()) {
                logging::warn(&format!("tray: init FAILED: {e}"));
            }

            if let Some(n) = bench_n {
                bench::spawn(app.handle().clone(), n, bench_rx);
            }

            logging::info("setup done");
            Ok(())
        })
        .on_window_event(|window, event| window::handle_window_event(window, event))
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
