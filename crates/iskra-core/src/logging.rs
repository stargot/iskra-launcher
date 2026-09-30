//! Файловое логирование (порт из spikes/tauri-app/src/main.rs, урезанный):
//! runtime.log → `%APPDATA%\iskra\logs\runtime.log`. Никаких зависимостей от Tauri.

use std::{fs::OpenOptions, io::Write, path::PathBuf};

/// Каталог данных Iskra: `%APPDATA%\iskra` (dirs::config_dir на Windows = Roaming).
pub fn base_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("iskra")
}

/// Каталог логов: `%APPDATA%\iskra\logs`.
pub fn logs_dir() -> PathBuf {
    base_dir().join("logs")
}

/// Путь основного runtime-лога: `%APPDATA%\iskra\logs\runtime.log`.
pub fn runtime_log_path() -> PathBuf {
    logs_dir().join("runtime.log")
}

/// Дописать строку в runtime.log (создать каталог при необходимости).
/// Ошибки не паникуют — логирование не должно ронять лончер (урок спайка).
pub fn log_line(line: &str) {
    append("runtime.log", line);
}

/// Дописать строку в произвольный лог каталога logs/ (напр. "latency.log").
pub fn append(name: &str, line: &str) {
    let path = logs_dir().join(name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

/// Очистить лог в logs/ (bench начинает прогон с чистым latency.log).
pub fn reset(name: &str) {
    let path = logs_dir().join(name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, b"");
}

pub fn info(line: &str) {
    log_line(&format!("INFO {line}"));
}

pub fn warn(line: &str) {
    log_line(&format!("WARN {line}"));
}
