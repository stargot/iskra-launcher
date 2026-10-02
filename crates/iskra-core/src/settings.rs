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

/// Режим размера окна лончера (прогон 1 D1). В JSON — нижним регистром:
/// "normal" | "double" | "fullscreen". Применение — app/src/window.rs::apply_mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    /// Обычный: 720×480 (синхронно с tauri.conf.json).
    Normal,
    /// Двойной: 1440×960 с клампом в work area монитора.
    Double,
    /// На весь экран (hide-on-blur сохраняется — D4).
    Fullscreen,
}

impl Default for WindowMode {
    fn default() -> Self {
        WindowMode::Normal
    }
}

/// Настройки приложения. camelCase в JSON (контракт IPC, план шага 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub theme: Theme,
    pub hotkey: String,
    pub autostart: bool,
    /// Ф3 D7: мониторинг клипборда включён (сервис живёт, но игнорирует события
    /// при false — применяется на лету). serde default → старый settings.json
    /// без полей читается без ошибок.
    pub clipboard_enabled: bool,
    /// Ф3 D7: исключённые приложения (process names, lowercase); пусто — все пишутся.
    pub clipboard_excluded_apps: Vec<String>,
    /// Прогон 1 D1: режим размера окна. serde default → старый settings.json
    /// без windowMode читается без ошибок (дефолт Normal = сегодняшние 720×480).
    pub window_mode: WindowMode,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::Dark,
            hotkey: "Alt+Space".to_string(),
            autostart: false,
            clipboard_enabled: true,
            clipboard_excluded_apps: Vec::new(),
            window_mode: WindowMode::Normal,
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
    pub clipboard_enabled: Option<bool>,
    pub clipboard_excluded_apps: Option<Vec<String>>,
    pub window_mode: Option<WindowMode>,
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
        if let Some(clipboard_enabled) = patch.clipboard_enabled {
            next.clipboard_enabled = clipboard_enabled;
        }
        if let Some(apps) = &patch.clipboard_excluded_apps {
            next.clipboard_excluded_apps = apps.clone();
        }
        if let Some(mode) = patch.window_mode {
            next.window_mode = mode;
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
        // Ф3 D7: дефолты клипборда — включён, исключений нет
        assert!(s.clipboard_enabled);
        assert!(s.clipboard_excluded_apps.is_empty());
        // Прогон 1 D1: дефолт режима окна — normal (720×480)
        assert_eq!(s.window_mode, WindowMode::Normal);
    }

    /// Ф3 D7: СТАРЫЙ settings.json (до клипборд-полей) читается без ошибок,
    /// недостающие поля берутся из defaults, повторное сохранение — полный круг.
    #[test]
    fn old_settings_json_without_clipboard_fields_roundtrips() {
        let dir = temp_dir("old-json");
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        // дословный формат времён фазы 2 (без clipboard*)
        std::fs::write(
            &path,
            r#"{
  "theme": "light",
  "hotkey": "Ctrl+Alt+K",
  "autostart": true
}"#,
        )
        .unwrap();

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.theme, Theme::Light);
        assert_eq!(loaded.hotkey, "Ctrl+Alt+K");
        assert!(loaded.autostart);
        assert!(loaded.clipboard_enabled, "дефолт D7: включён");
        assert!(loaded.clipboard_excluded_apps.is_empty(), "дефолт D7: пусто");

        // сохранение → чтение: поля появились, старые не потерялись
        loaded.save_to(&path).unwrap();
        let reloaded = Settings::load_from(&path);
        assert_eq!(reloaded, loaded);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(r#""clipboardEnabled": true"#)
                && raw.contains(r#""clipboardExcludedApps": []"#),
            "новые поля в camelCase, старые не потерялись: {raw}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Прогон 1 D1: СТАРЫЙ settings.json (до windowMode) читается без ошибок,
    /// дефолт normal проставляется, save/load — полный круг, в JSON "windowMode".
    #[test]
    fn old_settings_json_without_window_mode_roundtrips() {
        let dir = temp_dir("window-mode-old-json");
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        // дословный формат времён фазы 3 (без windowMode)
        std::fs::write(
            &path,
            r#"{
  "theme": "light",
  "hotkey": "Ctrl+Alt+K",
  "autostart": true
}"#,
        )
        .unwrap();

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.window_mode, WindowMode::Normal, "дефолт D1: normal");

        // сохранение → поле появилось в camelCase/lowercase → читается обратно
        loaded.save_to(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(r#""windowMode": "normal""#),
            "windowMode в JSON, serde lowercase: {raw}"
        );
        let reloaded = Settings::load_from(&path);
        assert_eq!(reloaded, loaded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Прогон 1 D1: все варианты WindowMode — lowercase в JSON, полный круг;
    /// патч применяется точечно (None-поля не трогают остальное).
    #[test]
    fn window_mode_values_roundtrip_and_patch() {
        for (mode, tag) in [
            (WindowMode::Normal, "normal"),
            (WindowMode::Double, "double"),
            (WindowMode::Fullscreen, "fullscreen"),
        ] {
            let mut s = Settings::default();
            s.window_mode = mode;
            let json = serde_json::to_string(&s).unwrap();
            assert!(
                json.contains(&format!(r#""windowMode":"{tag}""#)),
                "serde lowercase {tag}: {json}"
            );
            let back: Settings = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }

        let patched = Settings::default().patched(&SettingsPatch {
            window_mode: Some(WindowMode::Double),
            ..Default::default()
        });
        assert_eq!(patched.window_mode, WindowMode::Double);
        assert_eq!(
            patched.hotkey,
            "Alt+Space",
            "None-поля патча не меняют настройки"
        );
    }

    /// Ф3 D7: новые поля сериализуются в camelCase и патчатся по отдельности.
    #[test]
    fn clipboard_fields_roundtrip_and_patch() {
        let mut s = Settings::default();
        s.clipboard_enabled = false;
        s.clipboard_excluded_apps = vec!["notepad.exe".to_string(), "1password.exe".to_string()];
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains(r#""clipboardEnabled":false"#)
                && json.contains(r#""clipboardExcludedApps":["notepad.exe"#),
            "имена полей — camelCase: {json}"
        );
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);

        let patched = Settings::default().patched(&SettingsPatch {
            clipboard_enabled: Some(false),
            clipboard_excluded_apps: Some(vec!["mstsc.exe".to_string()]),
            ..Default::default()
        });
        assert!(!patched.clipboard_enabled);
        assert_eq!(patched.clipboard_excluded_apps, vec!["mstsc.exe".to_string()]);
        assert_eq!(patched.hotkey, "Alt+Space", "None-поля патча не меняют настройки");
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
