//! IPC-КОНТРАКТ UI ↔ core (план шага 3). Источник истины — этот файл;
//! ui/src/ipc/types.ts — дословное зеркало («при изменении Rust-типов править там»).
//! Все типы в camelCase (serde rename_all), тестируются roundtrip'ом ниже.

use serde::{Deserialize, Serialize};

// Settings/SettingsPatch упоминаются в док-комментариях контракта команд
// (get_settings/update_settings); сами типы живут в settings.rs и переиспользуются app.

// --- События core → UI (имена каналов — часть контракта) ---

/// После изменения настроек (в т.ч. из трея/команд): payload `Settings`.
pub const EVENT_SETTINGS_CHANGED: &str = "settings://changed";
/// После успешного remap/фолбэка хоткея: payload `HotkeyChanged`.
pub const EVENT_HOTKEY_CHANGED: &str = "hotkey://changed";
/// Навигация из трея на экран настроек: payload `null` (Rust: `()`).
pub const EVENT_NAV_SETTINGS: &str = "nav://settings";
/// «Медленные» результаты поиска (файлы) для активного запроса: payload `SearchResponse`.
/// UI применяет payload только при `queryId >= queryId` последнего отправленного
/// запроса (устаревшие ответы отбрасываются — D8).
pub const EVENT_SEARCH_UPDATED: &str = "search://updated";
/// Прогресс фонового индексатора: payload `IndexStatus`.
pub const EVENT_INDEX_PROGRESS: &str = "index://progress";
/// В кэш иконок досыпались новые PNG (полировка фазы 2): payload `null`
/// (Rust: `()`). UI может перезапросить search() с текущим запросом, чтобы
/// подставить иконки; сам сервис больше НИЧЕГО не пере-эмитит (см. D8 —
/// все эмиты списков идут только через search/run-путь с валидным qid).
pub const EVENT_ICONS_UPDATED: &str = "icons://updated";
/// После изменения клипборд-истории (новая запись/дедуп-подъём/удаление/pin):
/// payload — затронутый `ClipboardEntry` (для удаления id может не существовать —
/// UI просто перезапрашивает `clipboard_list`). Ф3, шаг 3 (сервис app).
pub const EVENT_CLIPBOARD_UPDATED: &str = "clipboard://updated";

// --- Типы команд и событий ---
//
// Фаза 1: `get_settings() -> Settings`, `update_settings(patch: SettingsPatch)
//   -> Result<Settings, SettingsError>`, `get_runtime_info() -> RuntimeInfo`.
// Фаза 2, шаг 4 (тонкие обёртки — app/src/commands.rs):
// - `search(q: string) -> SearchResponse` — быстрые провайдеры синхронно; «медленные»
//   (файлы) приходят событием `search://updated` (D8);
// - `run_item(id: string) -> Result<(), SearchError>` — usage++ и исполнение ItemAction;
// - `get_index_status() -> IndexStatus` — последний прогресс индексатора;
// - `reindex() -> ()` — полный рескан в фоновом воркере;
//   - `hide_window() -> ()` — скрыть окно лончера (Esc в UI); минорное расширение
//   контракта шага 4, согласовано в задаче шага 5.
// Фаза 3, шаг 3 (тонкие обёртки — app/src/commands.rs; сервис — clipboard_service.rs;
// D3–D9):
// - `clipboard_list(query: Option<string>) -> Vec<ClipboardEntry>` — список (pinned
//   сверху, далее used_at DESC), поиск по превью с normalize_name;
// - `clipboard_paste(id: number) -> Result<(), ...>` — вставка в активное окно:
//   цепочка D6 (foreground save → скрыть лончер → контент в клипборд → restore
//   → SendInput Ctrl+V); запись после вставки получает used_at/used_count++;
// - `clipboard_delete(id: number) -> bool` — удалить запись (+ PNG-файлы, D4);
// - `clipboard_pin(id: number, pinned: bool) -> bool` — закрепить/открепить (D8);
// - `snippets_list() -> Vec<Snippet>`, `snippet_create(name, body, keywords) -> Snippet`,
//   `snippet_update(id, name, body, keywords) -> bool`, `snippet_delete(id) -> bool`
//   (D9: CRUD в настройках; вставка из поиска — через run_item, действие CopyText).
// Мониторинг: settings.clipboard_enabled + clipboard_excluded_apps (D7); событие —
// EVENT_CLIPBOARD_UPDATED.

/// Пере-экспорт типов поиска: контракт выдачи живёт в search/types.rs,
/// но зеркало UI (ui/src/ipc/types.ts) описывает его рядом с остальным ipc.
pub use crate::search::types::{ItemAction, SearchItem, SystemCommand};
/// Пере-экспорт типов клипборда/сниппетов (Ф3, шаг 1): контракт в clipboard.rs/snippets.rs.
pub use crate::clipboard::{ClipboardEntry, ClipboardKind};
pub use crate::snippets::Snippet;

/// Команда `get_settings() -> Settings` (тип из settings.rs, общий с конфигом).
/// Команда `update_settings(patch: SettingsPatch) -> Result<Settings, SettingsError>`.
/// Команда `get_runtime_info() -> RuntimeInfo`.

/// Ответ команды `search(q)` и payload события `search://updated`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    /// Монотонный id запроса (генерирует SearchService); свежее — больше.
    pub query_id: u64,
    /// Готовая выдача: итоговый скор, dedupe, максимум 50 элементов.
    pub items: Vec<SearchItem>,
}

/// Ошибка `run_item(id)`: `"notFound"` | `{"action":{"message":"..."}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchError {
    /// Id нет в реестре выдачи (поиск ещё не выполнялся или элемент устарел).
    NotFound,
    /// Действие не выполнилось (ShellExecute/буфер обмена/питание вернули ошибку).
    Action { message: String },
}

/// Фаза индексатора (зеркало search/indexer.rs::IndexPhase в контракте IPC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IndexPhase {
    /// Обход ФС: `done` — найдено файлов, `total` ещё неизвестен (0).
    Scanning,
    /// Запись в БД: `done`/`total` известны.
    Indexing,
    /// Сверка удалённых файлов.
    Cleaning,
    /// Прогон завершён.
    Done,
}

impl From<crate::search::indexer::IndexPhase> for IndexPhase {
    fn from(p: crate::search::indexer::IndexPhase) -> Self {
        use crate::search::indexer::IndexPhase as P;
        match p {
            P::Scanning => IndexPhase::Scanning,
            P::Indexing => IndexPhase::Indexing,
            P::Cleaning => IndexPhase::Cleaning,
            P::Done => IndexPhase::Done,
        }
    }
}

/// Payload события `index://progress` и ответ `get_index_status()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub phase: IndexPhase,
    pub done: u64,
    pub total: u64,
}

/// Payload события `hotkey://changed`: фактический хоткей после remap/фолбэка.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyChanged {
    pub hotkey: String,
    /// True, если стоит фолбэк (основной хоткей занят) — UI показывает предупреждение.
    pub from_fallback: bool,
}

/// Ответ `get_runtime_info()`: что реально зарегистрировано сейчас.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub active_hotkey: Option<String>,
    pub fallback_active: bool,
}

/// Ошибка `update_settings` (см. план шага 3). Формат в JSON (serde, внешне тегированный enum
/// с camelCase-вариантами): `"invalidHotkey"` | `"hotkeyBusy"` | `{"io":{"message":"..."}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsError {
    /// Новый хоткей не парсится / невалидная комбинация.
    InvalidHotkey,
    /// Хоткей занят другим приложением; регистрация не изменилась.
    HotkeyBusy,
    /// Ошибка записи настроек на диск.
    Io { message: String },
}

impl From<std::io::Error> for SettingsError {
    fn from(e: std::io::Error) -> Self {
        SettingsError::Io { message: e.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Settings, SettingsPatch, Theme};

    /// Roundtrip: сериализация → десериализация → равенство, плюс проверка
    /// фактических имён полей в JSON (camelCase — часть контракта с ui/).
    #[test]
    fn settings_roundtrip_camel_case() {
        let mut s = Settings::default();
        s.theme = Theme::Light;
        s.hotkey = "Ctrl+Alt+K".to_string();
        s.autostart = true;

        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains(r#""theme":"light""#)
                && json.contains(r#""hotkey":"Ctrl+Alt+K""#)
                && json.contains(r#""autostart":true"#),
            "JSON должен быть в контрактом формате: {json}"
        );
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);

        // пустой JSON → defaults (настройка default на структуре)
        let defaults: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults, Settings::default());
    }

    #[test]
    fn settings_patch_roundtrip_partial() {
        let json = r#"{ "hotkey": "Ctrl+Alt+K", "theme": "dark" }"#;
        let patch: SettingsPatch = serde_json::from_str(json).unwrap();
        assert_eq!(patch.hotkey.as_deref(), Some("Ctrl+Alt+K"));
        assert_eq!(patch.theme, Some(Theme::Dark));
        assert_eq!(patch.autostart, None, "отсутствующее поле = None (не менять)");

        let back = serde_json::to_string(&patch).unwrap();
        let patch2: SettingsPatch = serde_json::from_str(&back).unwrap();
        assert_eq!(patch2, patch);
    }

    #[test]
    fn hotkey_changed_event_roundtrip_camel_case() {
        let ev = HotkeyChanged {
            hotkey: "Ctrl+Alt+Space".to_string(),
            from_fallback: true,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains(r#""hotkey":"Ctrl+Alt+Space""#) && json.contains(r#""fromFallback":true"#),
            "поле fromFallback — camelCase: {json}"
        );
        let back: HotkeyChanged = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn runtime_info_roundtrip_camel_case() {
        let info = RuntimeInfo {
            active_hotkey: Some("Alt+Space".to_string()),
            fallback_active: false,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(
            json.contains(r#""activeHotkey":"Alt+Space""#) && json.contains(r#""fallbackActive":false"#),
            "activeHotkey/fallbackActive — camelCase: {json}"
        );
        let back: RuntimeInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn settings_error_json_shape() {
        let busy = serde_json::to_string(&SettingsError::HotkeyBusy).unwrap();
        assert_eq!(busy, r#""hotkeyBusy""#);

        let invalid = serde_json::to_string(&SettingsError::InvalidHotkey).unwrap();
        assert_eq!(invalid, r#""invalidHotkey""#);

        let io = SettingsError::Io { message: "disk full".to_string() };
        let io_json = serde_json::to_string(&io).unwrap();
        assert_eq!(io_json, r#"{"io":{"message":"disk full"}}"#);

        let back: SettingsError = serde_json::from_str(&io_json).unwrap();
        assert_eq!(back, io);
    }

    #[test]
    fn event_names_are_stable_contract() {
        // Имена каналов — контракт с ui/src/ipc/client.ts. Случайная правка здесь
        // сломает события в рантайме, поэтому фиксируем их тестом.
        assert_eq!(EVENT_SETTINGS_CHANGED, "settings://changed");
        assert_eq!(EVENT_HOTKEY_CHANGED, "hotkey://changed");
        assert_eq!(EVENT_NAV_SETTINGS, "nav://settings");
        assert_eq!(EVENT_SEARCH_UPDATED, "search://updated");
        assert_eq!(EVENT_INDEX_PROGRESS, "index://progress");
        assert_eq!(EVENT_ICONS_UPDATED, "icons://updated");
    }

    // --- Фаза 2, шаг 4: roundtrip-тесты поиска/индексации ---

    /// SearchResponse: поле queryId — camelCase (контракт с ui/ipc/types.ts).
    #[test]
    fn search_response_roundtrip_camel_case() {
        let resp = SearchResponse {
            query_id: 42,
            items: vec![SearchItem::new(
                "system:sleep",
                "system",
                "Спящий режим",
                ItemAction::System { command: SystemCommand::Sleep },
            )],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            json.contains(r#""queryId":42"#) && json.contains(r#""iconPath":null"#),
            "queryId/iconPath — camelCase: {json}"
        );
        let back: SearchResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp);
    }

    /// SearchError: форма JSON — "notFound" | {"action":{"message":...}}.
    #[test]
    fn search_error_json_shape() {
        let nf = serde_json::to_string(&SearchError::NotFound).unwrap();
        assert_eq!(nf, r#""notFound""#);

        let act = SearchError::Action { message: "ShellExecute failed".to_string() };
        let json = serde_json::to_string(&act).unwrap();
        assert_eq!(json, r#"{"action":{"message":"ShellExecute failed"}}"#);
        let back: SearchError = serde_json::from_str(&json).unwrap();
        assert_eq!(back, act);
    }

    /// IndexStatus/фазы: значения — camelCase-строки, зеркалят indexer.rs.
    #[test]
    fn index_status_roundtrip_camel_case() {
        let st = IndexStatus { phase: IndexPhase::Indexing, done: 256, total: 1024 };
        let json = serde_json::to_string(&st).unwrap();
        assert!(
            json.contains(r#""phase":"indexing""#)
                && json.contains(r#""done":256"#)
                && json.contains(r#""total":1024"#),
            "IndexStatus — camelCase: {json}"
        );
        let back: IndexStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, st);

        for (p, s) in [
            (IndexPhase::Scanning, "scanning"),
            (IndexPhase::Cleaning, "cleaning"),
            (IndexPhase::Done, "done"),
        ] {
            assert_eq!(serde_json::to_string(&p).unwrap(), format!(r#""{s}""#));
        }
    }

    /// Зеркало indexer::IndexPhase → ipc::IndexPhase — полный и однозначный.
    #[test]
    fn index_phase_from_indexer_is_total() {
        use crate::search::indexer::IndexPhase as P;
        assert_eq!(IndexPhase::from(P::Scanning), IndexPhase::Scanning);
        assert_eq!(IndexPhase::from(P::Indexing), IndexPhase::Indexing);
        assert_eq!(IndexPhase::from(P::Cleaning), IndexPhase::Cleaning);
        assert_eq!(IndexPhase::from(P::Done), IndexPhase::Done);
    }

    // --- Фаза 3, шаг 1: roundtrip-тесты клипборда/сниппетов ---

    /// ClipboardEntry: все поля — camelCase (imagePath/contentHash/sourceApp/…);
    /// content у image — null, у files — JSON-массив путей.
    #[test]
    fn clipboard_entry_roundtrip_camel_case() {
        let e = ClipboardEntry {
            id: 7,
            kind: ClipboardKind::Image,
            content: None,
            image_path: Some(r"C:\Users\I\AppData\Roaming\iskra\clipboard\7.png".to_string()),
            preview: "[изображение 1920×1080]".to_string(),
            pinned: true,
            source_app: Some("snipaste.exe".to_string()),
            content_hash: "0123456789abcdef".to_string(),
            created_at: 1_700_000_000_000,
            used_at: 1_700_000_050_000,
            used_count: 3,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(
            json.contains(r#""imagePath":"C:\\Users"#)
                && json.contains(r#""contentHash":"0123456789abcdef""#)
                && json.contains(r#""sourceApp":"snipaste.exe""#)
                && json.contains(r#""usedCount":3"#)
                && json.contains(r#""createdAt":1700000000000"#)
                && json.contains(r#""content":null"#)
                && json.contains(r#""kind":"image""#),
            "camelCase и lower-case kind: {json}"
        );
        let back: ClipboardEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }

    /// Виды записей — нижним регистром: "text" | "image" | "files"; files-entry
    /// с content-массивом тоже ходит по IPC (mirror ui/src/ipc/types.ts).
    #[test]
    fn clipboard_kind_values_and_files_entry_roundtrip() {
        assert_eq!(serde_json::to_string(&ClipboardKind::Text).unwrap(), r#""text""#);
        assert_eq!(serde_json::to_string(&ClipboardKind::Image).unwrap(), r#""image""#);
        assert_eq!(serde_json::to_string(&ClipboardKind::Files).unwrap(), r#""files""#);

        let e = ClipboardEntry {
            id: 3,
            kind: ClipboardKind::Files,
            content: Some(r#"["C:\\a.txt","C:\\б б.png"]"#.to_string()),
            image_path: None,
            preview: "a.txt, б б.png".to_string(),
            pinned: false,
            source_app: Some("explorer.exe".to_string()),
            content_hash: "ff00ff00ff00ff00".to_string(),
            created_at: 1,
            used_at: 2,
            used_count: 1,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"files""#) && json.contains(r#""imagePath":null"#));
        let back: ClipboardEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
        assert_eq!(back.files(), vec![r"C:\a.txt", r"C:\б б.png"]);
    }

    /// Snippet: camelCase (createdAt); файл-сниппет с пустыми keywords.
    #[test]
    fn snippet_roundtrip_camel_case() {
        let s = Snippet {
            id: 5,
            name: "Адрес".to_string(),
            body: "ул. Ленина, 1".to_string(),
            keywords: "адрес почта".to_string(),
            created_at: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains(r#""name":"Адрес""#) && json.contains(r#""createdAt":1700000000000"#),
            "camelCase: {json}"
        );
        let back: Snippet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    /// Имя канала клипборда — контракт с ui/src/ipc/client.ts (фиксируем тестом).
    #[test]
    fn clipboard_event_name_is_stable_contract() {
        assert_eq!(EVENT_CLIPBOARD_UPDATED, "clipboard://updated");
    }
}
