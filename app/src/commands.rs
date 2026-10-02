//! Tauri-команды IPC-контракта (тонкие обёртки; логика — в iskra-core и сервисах).
//! Контракт типов: crates/iskra-core/src/ipc.rs, зеркало UI: ui/src/ipc/types.ts.

use tauri::{AppHandle, Emitter, Manager, State};

use iskra_core::ipc::{IndexStatus, SearchError, SearchResponse};
use iskra_core::logging;
use iskra_core::{
    ClipboardEntry, RuntimeInfo, Settings, SettingsError, SettingsPatch, Snippet,
    EVENT_SETTINGS_CHANGED,
};

use crate::clipboard_service;
use crate::tray;
use crate::window;
use crate::AppState;

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().expect("state.settings poisoned").clone()
}

#[tauri::command]
pub fn get_runtime_info(state: State<'_, AppState>) -> RuntimeInfo {
    state.hotkey.runtime_info()
}

// --- Фаза 2, шаг 4: поиск (тонкие обёртки; логика — SearchService) ---

/// Поиск: быстрые провайдеры синхронно, «медленные» (файлы) — событием
/// `search://updated` (D8). Пустой запрос — recents.
#[tauri::command]
pub fn search(state: State<'_, AppState>, q: String) -> SearchResponse {
    state.search.search(&q)
}

/// Исполнить элемент выдачи: usage++ и ItemAction через iskra-sys/буфер обмена.
/// D9: сниппеты (id "snippets:N", действие CopyText) вставляются сразу —
/// активное окно запоминаем ДО копирования (лончер сейчас в фокусе), после
/// копирования paste_just_copied вернёт фокус и пошлёт Ctrl+V.
#[tauri::command]
pub fn run_item(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), SearchError> {
    let prev = if id.starts_with("snippets:") {
        Some(clipboard_service::foreground_hwnd())
    } else {
        None
    };
    state.search.run_item(&id)?;
    if let Some(prev) = prev {
        clipboard_service::paste_just_copied(&app, prev);
    }
    Ok(())
}

/// Последний статус фонового индексатора.
#[tauri::command]
pub fn get_index_status(state: State<'_, AppState>) -> IndexStatus {
    state.search.index_status()
}

/// Полный рескан в фоновом воркере (не блокирует UI).
#[tauri::command]
pub fn reindex(state: State<'_, AppState>) {
    state.search.request_reindex();
}

/// Скрыть окно лончера (Esc в UI; минорное расширение контракта шага 4).
#[tauri::command]
pub fn hide_window(app: AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
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
    // Ф3 D7: тумблер мониторинга и исключения применяются на лету (listener
    // жив, события игнорируются) — без рестарта.
    state.clipboard.apply_settings(&next);
    // Прогон 1 D2: режим окна применяется после успешного save. Ошибка
    // применения — лог, НЕ Err: настройка уже сохранена (команда не падает,
    // режим доедет при следующем старте).
    if patch.window_mode.is_some() {
        if let Some(win) = app.get_webview_window("main") {
            window::apply_mode(&win, next.window_mode);
        }
    }
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

// --- Фаза 3, шаг 3: клипборд + сниппеты (тонкие обёртки; логика — ClipboardService) ---

/// История клипборда: pinned сверху, далее used_at DESC, топ-100 (D8). Ошибка
/// БД (патология) → пустой список + лог: контракт команды — плоский Vec.
#[tauri::command]
pub fn clipboard_list(state: State<'_, AppState>, query: Option<String>) -> Vec<ClipboardEntry> {
    match state.clipboard.list(query.as_deref()) {
        Ok(items) => items,
        Err(e) => {
            logging::warn(&format!("commands: clipboard_list FAILED: {e}"));
            Vec::new()
        }
    }
}

/// Вставить запись в активное окно (цепочка D6); лончер скрывает сервис.
#[tauri::command]
pub fn clipboard_paste(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
) -> Result<(), String> {
    state.clipboard.paste(&app, id)
}

/// Удалить запись (+ PNG/thumbnail с диска, D4); событие — в сервисе.
#[tauri::command]
pub fn clipboard_delete(app: AppHandle, state: State<'_, AppState>, id: i64) -> bool {
    let existing = state.clipboard.get(id).ok().flatten();
    match state.clipboard.delete(id) {
        Ok(true) => {
            if let Some(entry) = existing {
                state.clipboard.emit_updated(&app, &entry);
            }
            true
        }
        Ok(false) => false,
        Err(e) => {
            logging::warn(&format!("commands: clipboard_delete({id}) FAILED: {e}"));
            false
        }
    }
}

/// Закрепить/открепить (D8); событие — в сервисе.
#[tauri::command]
pub fn clipboard_pinned(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    pinned: bool,
) -> bool {
    match state.clipboard.set_pinned(id, pinned) {
        Ok(true) => {
            if let Ok(Some(entry)) = state.clipboard.get(id) {
                state.clipboard.emit_updated(&app, &entry);
            }
            true
        }
        Ok(false) => false,
        Err(e) => {
            logging::warn(&format!("commands: clipboard_pinned({id}) FAILED: {e}"));
            false
        }
    }
}

// --- сниппеты (D9): CRUD в настройках; вставка из поиска — run_item ---

#[tauri::command]
pub fn snippets_list(state: State<'_, AppState>) -> Vec<Snippet> {
    match state.clipboard.snippets_list() {
        Ok(items) => items,
        Err(e) => {
            logging::warn(&format!("commands: snippets_list FAILED: {e}"));
            Vec::new()
        }
    }
}

#[tauri::command]
pub fn snippet_create(
    state: State<'_, AppState>,
    name: String,
    body: String,
    keywords: String,
) -> Result<Snippet, String> {
    state.clipboard.snippet_create(&name, &body, &keywords).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn snippet_update(
    state: State<'_, AppState>,
    id: i64,
    name: String,
    body: String,
    keywords: String,
) -> Result<bool, String> {
    state.clipboard.snippet_update(id, &name, &body, &keywords).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn snippet_delete(state: State<'_, AppState>, id: i64) -> bool {
    match state.clipboard.snippet_delete(id) {
        Ok(deleted) => deleted,
        Err(e) => {
            logging::warn(&format!("commands: snippet_delete({id}) FAILED: {e}"));
            false
        }
    }
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
