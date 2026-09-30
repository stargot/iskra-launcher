//! Типы результатов поиска: `SearchItem` и `ItemAction` (план Ф2, шаг 1).
//! serde — camelCase (единый контракт с UI; зеркало в ui/src/ipc/types.ts и
//! пере-экспорт в ipc.rs появятся в шаге 4). Крейт остаётся без Tauri/WinAPI:
//! действие — данные, исполнение — app/iskra-sys (шаги 3–4).

use serde::{Deserialize, Serialize};

/// Однострочный результат поиска. Порядок полей и имена — контракт IPC.
///
/// Поле `score` меняет смысл по стадиям конвейера:
/// - провайдер кладёт сюда сырой fuzzy-скор 0..1000 (`crate::search::fuzzy`);
/// - агрегатор перезаписывает итоговым скором `crate::search::ranking::total_score`
///   (fuzzy + приоритет провайдера + usage). Единственная точка весов — ranking.rs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchItem {
    /// Стабильный идентификатор вида `"{provider}:{ключ}"` (напр. `apps:chrome`,
    /// `settings:display`, `calc:2+2*2`). Ключ для usage-таблицы и `run_item`.
    pub id: String,
    /// Имя провайдера: `apps` | `calc` | `settings` | `system` | `files` | `web`.
    pub provider: String,
    /// Заголовок строки (название приложения/файла/настройки или результат calc).
    pub title: String,
    /// Вторая строка (путь файла, EN-имя настройки, исходное выражение).
    pub subtitle: Option<String>,
    /// Путь к PNG-иконке в кэше `%APPDATA%\iskra\icons\{hash}.png` (D7, шаг 3).
    pub icon_path: Option<String>,
    /// См. описание структуры: fuzzy у провайдера, итоговый скор после агрегатора.
    pub score: i64,
    /// Что выполнить по Enter (исполняется в app через iskra-sys, шаги 3–4).
    pub action: ItemAction,
}

impl SearchItem {
    /// Удобный конструктор: subtitle/icon_path пустые, score = 0.
    pub fn new(id: impl Into<String>, provider: &str, title: impl Into<String>, action: ItemAction) -> Self {
        SearchItem {
            id: id.into(),
            provider: provider.to_string(),
            title: title.into(),
            subtitle: None,
            icon_path: None,
            score: 0,
            action,
        }
    }
}

/// Действие по выбору элемента. Только данные: исполнение — ShellExecuteW/иконки
/// (iskra-sys, шаг 3) и сервис app (шаг 4); CopyText — буфер обмена на стороне app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ItemAction {
    /// Запуск приложения (цель .lnk/exe) с аргументами (D4: из parselnk).
    LaunchApp { path: String, args: Option<String> },
    /// Открыть файл/папку ассоциированным приложением.
    OpenPath { path: String },
    /// Открыть URI (ms-settings:, https:, shell:...).
    OpenUri { uri: String },
    /// Поиск в интернете через браузер по умолчанию (fallback web-провайдера).
    WebSearch { url: String },
    /// Скопировать текст (результат калькулятора).
    CopyText { text: String },
    /// Системная команда (iskra-sys/power.rs, шаг 3). По спеке — риск 9: UI
    /// требует ≥ 2 символов запроса; список команд фиксируется в шаге 3.
    System { command: SystemCommand },
}

/// Каталог системных команд (шаг 3 реализует вызовы в iskra-sys/power.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SystemCommand {
    /// Win+L (LockWorkStation).
    Lock,
    /// Выключение монитора (SC_MONITORPOWER).
    MonitorOff,
    /// Сон (SetSuspendState).
    Sleep,
    /// Перезагрузка (ExitWindowsEx).
    Restart,
    /// Завершение работы (ExitWindowsEx + AdjPriv, риск 4).
    Shutdown,
    /// Очистить корзину (SHEmptyRecycleBin).
    EmptyRecycleBin,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roundtrip SearchItem + проверка camelCase-имён полей (контракт с ui/).
    #[test]
    fn search_item_roundtrip_camel_case() {
        let item = SearchItem {
            id: "apps:chrome".to_string(),
            provider: "apps".to_string(),
            title: "Хром".to_string(),
            subtitle: Some(r"C:\Program Files\Chrome\chrome.exe".to_string()),
            icon_path: Some("icons/ab12cd.png".to_string()),
            score: 987,
            action: ItemAction::LaunchApp {
                path: r"C:\Program Files\Chrome\chrome.exe".to_string(),
                args: None,
            },
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(
            json.contains(r#""iconPath":"icons/ab12cd.png""#) && json.contains(r#""score":987"#),
            "имена полей должны быть camelCase: {json}"
        );
        let back: SearchItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item);
    }

    /// Roundtrip всех вариантов ItemAction; тег типа — "type", значения camelCase.
    #[test]
    fn item_action_roundtrip_all_variants() {
        let actions = vec![
            ItemAction::LaunchApp {
                path: "a.exe".to_string(),
                args: Some("--flag".to_string()),
            },
            ItemAction::OpenPath { path: "C:\\".to_string() },
            ItemAction::OpenUri { uri: "ms-settings:display".to_string() },
            ItemAction::WebSearch { url: "https://x/?q=a%20b".to_string() },
            ItemAction::CopyText { text: "42".to_string() },
            ItemAction::System { command: SystemCommand::MonitorOff },
        ];
        for a in actions {
            let json = serde_json::to_string(&a).unwrap();
            let back: ItemAction = serde_json::from_str(&json).unwrap();
            assert_eq!(back, a, "roundtrip {json}");
        }
        // проверка значений тега и поля команды (camelCase)
        let json = serde_json::to_string(&ItemAction::System { command: SystemCommand::MonitorOff })
            .unwrap();
        assert!(
            json.contains(r#""type":"system""#) && json.contains(r#""command":"monitorOff""#),
            "тег/вариант в camelCase: {json}"
        );
    }
}
