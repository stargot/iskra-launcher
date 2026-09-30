//! Сервис глобального хоткея (плагин global-shortcut): регистрация из настроек,
//! фолбэк Ctrl+Alt+Space при занятом основном, переназначение в рантайме
//! (unregister → register, при ошибке откат + `HotkeyBusy`), события `hotkey://changed`.
//! Паттерн из spikes/tauri-app: занятый хоткей НЕ роняет приложение.

use std::sync::RwLock;

use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use iskra_core::logging;
use iskra_core::{
    HotkeyChanged, RuntimeInfo, SettingsError, EVENT_HOTKEY_CHANGED,
};

/// Фолбэк, если основной хоткей занят другой программой (напр. PowerToys Run).
pub const FALLBACK_HOTKEY: &str = "Ctrl+Alt+Space";
/// Bench-хоткей без системного смысла; регистрируется только при `--bench N`.
pub const BENCH_HOTKEY: &str = "Ctrl+Alt+F9";

/// Что зарегистрировано прямо скажем сейчас (show-хоткей).
#[derive(Debug, Default, Clone)]
struct Active {
    shortcut: Option<Shortcut>,
    /// Строка активного хоткея (как в Settings/RuntimeInfo); None — не зарегистрировано ничего.
    display: Option<String>,
    /// True, если вместо основного стоит фолбэк.
    from_fallback: bool,
}

pub struct HotkeyService {
    active: RwLock<Active>,
    bench: RwLock<Option<Shortcut>>,
}

impl HotkeyService {
    pub fn new() -> Self {
        Self {
            active: RwLock::new(Active::default()),
            bench: RwLock::new(None),
        }
    }

    /// Стартовая регистрация из настроек: занятый основной → фолбэк → без хоткея
    /// (лончер остаётся доступным из трея). Шлёт `hotkey://changed`.
    pub fn init(&self, app: &AppHandle, configured: &str, bench: bool) {
        let active = match Self::try_register(app, configured) {
            Ok(sc) => Active {
                shortcut: Some(sc),
                display: Some(configured.to_string()),
                from_fallback: false,
            },
            Err(err) => {
                logging::warn(&format!(
                    "hotkey: «{configured}» register FAILED: {err}; пробую фолбэк"
                ));
                match Self::try_register(app, FALLBACK_HOTKEY) {
                    Ok(sc) => Active {
                        shortcut: Some(sc),
                        display: Some(FALLBACK_HOTKEY.to_string()),
                        from_fallback: true,
                    },
                    Err(err2) => {
                        logging::warn(&format!(
                            "hotkey: фолбэк тоже FAILED: {err2}; лончер без хоткея (трей работает)"
                        ));
                        Active::default()
                    }
                }
            }
        };
        *self.active.write().expect("hotkey.active poisoned") = active;

        if bench {
            match Self::try_register(app, BENCH_HOTKEY) {
                Ok(sc) => *self.bench.write().expect("hotkey.bench poisoned") = Some(sc),
                Err(err) => logging::warn(&format!("hotkey: bench {BENCH_HOTKEY} FAILED: {err}")),
            }
        }
        self.emit_changed(app);
    }

    /// Это текущий show-хоткей? (вызывается обработчиком плагина на каждое нажатие)
    pub fn matches_show(&self, shortcut: &Shortcut) -> bool {
        self.active
            .read()
            .expect("hotkey.active poisoned")
            .shortcut
            .as_ref()
            == Some(shortcut)
    }

    /// Это bench-хоткей (регистрируется только в bench-режиме)?
    pub fn matches_bench(&self, shortcut: &Shortcut) -> bool {
        *self.bench.read().expect("hotkey.bench poisoned") == Some(*shortcut)
    }

    /// Переназначение: unregister старого → register нового. Ошибка регистрации —
    /// откат (прежний/фолбэк/ничего) + `Err(HotkeyBusy)`; настройки при этом не меняются
    /// (команда update_settings вызывает remap ДО сохранения). В любом исходе шлёт
    /// `hotkey://changed` с фактическим состоянием.
    pub fn remap(&self, app: &AppHandle, configured: &str) -> Result<(), SettingsError> {
        // Парс до любых побочных эффектов: невалидный хоткей ничего не должен менять.
        let new_sc: Shortcut =
            configured.parse().map_err(|_| SettingsError::InvalidHotkey)?;

        let mut active = self.active.write().expect("hotkey.active poisoned");
        let previous = active.clone();

        if let Some(old) = previous.shortcut {
            if let Err(err) = app.global_shortcut().unregister(old) {
                logging::warn(&format!("hotkey: unregister «{old}» FAILED: {err}"));
            }
        }

        match app.global_shortcut().register(new_sc) {
            Ok(()) => {
                *active = Active {
                    shortcut: Some(new_sc),
                    display: Some(configured.to_string()),
                    from_fallback: false,
                };
                drop(active);
                logging::info(&format!("hotkey: remapped to «{configured}»"));
                self.emit_changed(app);
                Ok(())
            }
            Err(err) => {
                logging::warn(&format!(
                    "hotkey: register «{configured}» FAILED: {err}; откат на прежний/фолбэк"
                ));
                // Откат: сначала прежняя строка (вернуть как было), затем фолбэк,
                // в крайнем случае — без хоткея. Сервис остаётся работоспособным.
                let restored = previous
                    .display
                    .as_deref()
                    .and_then(|spec| Self::try_register(app, spec).ok())
                    .map(|sc| Active {
                        shortcut: Some(sc),
                        display: previous.display.clone(),
                        from_fallback: previous.from_fallback,
                    })
                    .or_else(|| {
                        Self::try_register(app, FALLBACK_HOTKEY).ok().map(|sc| Active {
                            shortcut: Some(sc),
                            display: Some(FALLBACK_HOTKEY.to_string()),
                            from_fallback: true,
                        })
                    })
                    .unwrap_or_default();
                *active = restored;
                drop(active);
                self.emit_changed(app);
                Err(SettingsError::HotkeyBusy)
            }
        }
    }

    /// Ответ `get_runtime_info`: что реально зарегистрировано.
    pub fn runtime_info(&self) -> RuntimeInfo {
        let a = self.active.read().expect("hotkey.active poisoned");
        RuntimeInfo {
            active_hotkey: a.display.clone(),
            fallback_active: a.from_fallback,
        }
    }

    fn try_register(app: &AppHandle, spec: &str) -> Result<Shortcut, String> {
        let sc: Shortcut = spec.parse().map_err(|e| format!("parse: {e}"))?;
        app.global_shortcut()
            .register(sc)
            .map_err(|e| e.to_string())?;
        logging::info(&format!("hotkey: registered «{spec}»"));
        Ok(sc)
    }

    /// `hotkey://changed` с фактическим состоянием (после init/remap/отката).
    fn emit_changed(&self, app: &AppHandle) {
        let a = self.active.read().expect("hotkey.active poisoned");
        let Some(display) = &a.display else {
            return; // хоткея нет — событие не шлём; UI узнает из get_runtime_info
        };
        let payload = HotkeyChanged {
            hotkey: display.clone(),
            from_fallback: a.from_fallback,
        };
        drop(a);
        if let Err(err) = app.emit(EVENT_HOTKEY_CHANGED, &payload) {
            logging::warn(&format!("hotkey: emit hotkey://changed FAILED: {err}"));
        }
    }
}
