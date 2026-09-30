//! Настройки Iskra: defaults, атомарный save (tmp + rename), tolerant load.
//! Путь: `%APPDATA%\iskra\settings.json` (план шага 3).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::logging;

/// Тема UI. В JSON — нижним регистром: "dark" | "light" (совпадает с data-theme в ui/).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Dark,
    Light,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Dark
    }
}

/// Настройки приложения. camelCase в JSON (контракт IPC, план шага 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub theme: Theme,
    pub hotkey: String,
    pub autostart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::Dark,
            hotkey: "Alt+Space".to_string(),
            autostart: false,
        }
    }
}

/// Частичное обновление настроек: все поля Option, None = «не менять».
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub theme: Option<Theme>,
    pub hotkey: Option<String>,
    pub autostart: Option<bool>,
}

impl Settings {
    /// Применить патч к копии настроек.
    pub fn patched(&self, patch: &SettingsPatch) -> Settings {
        let mut next = self.clone();
        if let Some(theme) = patch.theme {
            next.theme = theme;
        }
        if let Some(hotkey) = &patch.hotkey {
            next.hotkey = hotkey.clone();
        }
        if let Some(autostart) = patch.autostart {
            next.autostart = autostart;
        }
        next
    }

    // --- пути ---

    /// `%APPDATA%\iskra`.
    pub fn default_config_dir() -> PathBuf {
        crate::logging::base_dir()
    }

    /// `%APPDATA%\iskra\settings.json`.
    pub fn default_path() -> PathBuf {
        Self::default_config_dir().join("settings.json")
    }

    // --- load / save ---

    /// Загрузить из `%APPDATA%\iskra\settings.json`; нет файла/битый JSON → defaults + лог.
    /// Никогда не падает: настройки критичны для старта лончера.
    pub fn load() -> Settings {
        Self::load_from(&Self::default_path())
    }

    /// Загрузка из конкретного пути (для тестов и шага 4).
    pub fn load_from(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str::<Settings>(&raw) {
                Ok(settings) => settings,
                Err(e) => {
                    logging::warn(&format!(
                        "settings: битый JSON в {} ({e}); использую defaults",
                        path.display()
                    ));
                    Settings::default()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                logging::info(&format!(
                    "settings: {} не существует (первый запуск) — defaults",
                    path.display()
                ));
                Settings::default()
            }
            Err(e) => {
                logging::warn(&format!(
                    "settings: не удалось прочитать {} ({e}); использую defaults",
                    path.display()
                ));
                Settings::default()
            }
        }
    }

    /// Сохранить в `%APPDATA%\iskra\settings.json` (атомарно).
    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::default_path())
    }

    /// Атомарное сохранение в конкретный путь: пишем tmp-файл рядом и переименовываем.
    /// `std::fs::rename` на Windows заменяет существующий файл (MoveFileEx с
    /// REPLACE_EXISTING),rename в пределах каталога атомарен — читатель не увидит
    /// полуфайл. При ошибке tmp удаляется.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Уникальный временный каталог для теста (без внешних крейтов).
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iskra-core-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn defaults_match_plan() {
        let s = Settings::default();
        assert_eq!(s.theme, Theme::Dark);
        assert_eq!(s.hotkey, "Alt+Space");
        assert!(!s.autostart);
    }

    #[test]
    fn save_load_roundtrip_in_temp_dir() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        s.theme = Theme::Light;
        s.hotkey = "Ctrl+Alt+K".to_string();
        s.autostart = true;

        s.save_to(&path).expect("save_to");
        // tmp-файл после атомарного сохранения не остаётся
        assert!(!path.with_extension("json.tmp").exists(), "tmp не должен переживать rename");

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded, s, "load_from должен вернуть то, что сохранили");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_json_falls_back_to_defaults() {
        let dir = temp_dir("broken");
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "{ not valid json !!!").unwrap();

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded, Settings::default(), "битый JSON → defaults");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_json_fills_missing_fields_with_defaults() {
        let dir = temp_dir("partial");
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{ "hotkey": "Ctrl+Alt+K" }"#).unwrap();

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.hotkey, "Ctrl+Alt+K");
        assert_eq!(loaded.theme, Theme::Dark, "недостающие поля берутся из defaults");
        assert!(!loaded.autostart);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_defaults() {
        let dir = temp_dir("missing");
        let loaded = Settings::load_from(&dir.join("nope").join("settings.json"));
        assert_eq!(loaded, Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_applies_only_present_fields() {
        let s = Settings::default();
        let patched = s.patched(&SettingsPatch {
            hotkey: Some("Ctrl+Alt+K".to_string()),
            ..Default::default()
        });
        assert_eq!(patched.hotkey, "Ctrl+Alt+K");
        assert_eq!(patched.theme, Theme::Dark, "None-поля патча не меняют настройки");
        assert!(!patched.autostart);
    }
}
