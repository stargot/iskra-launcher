//! IPC-КОНТРАКТ UI ↔ core (план шага 3). Источник истины — этот файл;
//! ui/src/ipc/types.ts — дословное зеркало («при изменении Rust-типов править там»).
//! Все типы в camelCase (serde rename_all), тестируются roundtrip'ом ниже.

use serde::{Deserialize, Serialize};

// Settings/SettingsPatch упоминаются в док-комментариях контракта команд
// (get_settings/update_settings); сами типы живут в settings.rs и переиспользуются app.
#[cfg(test)]
use crate::settings::{Settings, SettingsPatch};

// --- События core → UI (имена каналов — часть контракта) ---

/// После изменения настроек (в т.ч. из трея/команд): payload `Settings`.
pub const EVENT_SETTINGS_CHANGED: &str = "settings://changed";
/// После успешного remap/фолбэка хоткея: payload `HotkeyChanged`.
pub const EVENT_HOTKEY_CHANGED: &str = "hotkey://changed";
/// Навигация из трея на экран настроек: payload `null` (Rust: `()`).
pub const EVENT_NAV_SETTINGS: &str = "nav://settings";

// --- Типы команд и событий ---

/// Команда `get_settings() -> Settings` (тип из settings.rs, общий с конфигом).
/// Команда `update_settings(patch: SettingsPatch) -> Result<Settings, SettingsError>`.
/// Команда `get_runtime_info() -> RuntimeInfo`.

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
    }
}
