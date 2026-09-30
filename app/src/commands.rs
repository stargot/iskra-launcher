//! Tauri-команды IPC-контракта (тонкие обёртки; логика — в iskra-core и сервисах).
//! Контракт типов: crates/iskra-core/src/ipc.rs, зеркало UI: ui/src/ipc/types.ts.

use tauri::{AppHandle, Emitter, Manager, State};

use iskra_core::logging;
use iskra_core::{
    RuntimeInfo, Settings, SettingsError, SettingsPatch, EVENT_SETTINGS_CHANGED,
};

use crate::tray;
use crate::AppState;

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().expect("state.settings poisoned").clone()
}

#[tauri::command]
pub fn get_runtime_info(state: State<'_, AppState>) -> RuntimeInfo {
    state.hotkey.runtime_info()
}

/// Применить патч настроек: хоткей → remap (при InvalidHotkey/HotkeyBusy ничего
/// не меняется и не сохраняется), автозапуск → реестр + галочка трея, затем
/// атомарный save и `settings://changed`.
#[tauri::command]
pub fn update_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    patch: SettingsPatch,
) -> Result<Settings, SettingsError> {
    let current = state.settings.lock().expect("state.settings poisoned").clone();
    let next = current.patched(&patch);

    // 1. Хоткей: remap до записи на диск — ошибка оставляет всё как было.
    if let Some(spec) = &patch.hotkey {
        state.hotkey.remap(&app, spec)?;
    }

    // 2. Автозапуск: реестр + синхронизация галочки трея.
    if let Some(enable) = patch.autostart {
        apply_autostart(&app, enable).map_err(|e| SettingsError::Io { message: e })?;
    }

    // 3. Сохранение + публикация. (Save после эффектов: сбой диска оставит
    //    применённые эффекты, но не рассинхронизирует хоткей.)
    next.save()
        .map_err(|e| SettingsError::Io { message: e.to_string() })?;
    *state.settings.lock().expect("state.settings poisoned") = next.clone();
    if let Err(err) = app.emit(EVENT_SETTINGS_CHANGED, &next) {
        logging::warn(&format!("commands: emit settings://changed FAILED: {err}"));
    }
    logging::info(&format!("settings updated: {next:?}"));
    Ok(next)
}

/// Записать/удалить значение автозапуска в реестре и перестроить меню трея.
/// Не сохраняет settings.json — вызывающий (update_settings / трей) делает это сам.
pub(crate) fn apply_autostart(app: &AppHandle, enable: bool) -> Result<(), String> {
    let result = if enable {
        iskra_sys::autostart::set()
    } else {
        iskra_sys::autostart::remove()
    };
    if let Err(e) = result {
        let msg = format!("autostart registry: {e}");
        logging::warn(&msg);
        return Err(msg);
    }
    tray::sync_autostart(app, enable);
    Ok(())
}

/// Переключение автозапуска из трея: полный цикл — реестр, settings.json, событие.
/// Возвращает фактическое новое состояние (для перестройки меню).
pub(crate) fn toggle_autostart(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let enable = !state.settings.lock().expect("state.settings poisoned").autostart;
    if let Err(msg) = apply_autostart(app, enable) {
        logging::warn(&format!("tray: toggle autostart FAILED: {msg}"));
        return !enable; // не применилось — галочку не двигаем
    }
    let next = {
        let mut guard = state.settings.lock().expect("state.settings poisoned");
        let mut n = guard.clone();
        n.autostart = enable;
        match n.save() {
            Ok(()) => *guard = n.clone(),
            Err(e) => logging::warn(&format!("tray: autostart save FAILED: {e}")),
        }
        n
    };
    if let Err(err) = app.emit(EVENT_SETTINGS_CHANGED, &next) {
        logging::warn(&format!("tray: emit settings://changed FAILED: {err}"));
    }
    enable
}
