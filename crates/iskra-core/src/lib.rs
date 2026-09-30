//! iskra-core — чистая логика Iskra: настройки, IPC-контракт, логирование.
//! Ограничение крейта: никаких зависимостей от Tauri и WinAPI — быстро тестируется,
//! переиспользуется фазой 2 (плагины). Реализация — шаг 3 Фазы 1
//! (docs/plans/2026-09-29-phase1-implementation.md).

pub mod ipc;
pub mod logging;
pub mod settings;

pub use ipc::{
    HotkeyChanged, RuntimeInfo, SettingsError, EVENT_HOTKEY_CHANGED, EVENT_NAV_SETTINGS,
    EVENT_SETTINGS_CHANGED,
};
pub use settings::{Settings, SettingsPatch, Theme};
