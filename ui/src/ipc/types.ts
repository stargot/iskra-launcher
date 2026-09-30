// ЗЕРКАЛО crates/iskra-core/src/ipc.rs (+ settings.rs). Источник истины — Rust;
// при изменении Rust-типов править здесь дословно (план шага 5).
// Формат: camelCase (serde rename_all), события core → UI, команды UI → core.

export type Theme = "dark" | "light";

/** crates/iskra-core/src/settings.rs::Settings */
export interface Settings {
  theme: Theme;
  hotkey: string;
  autostart: boolean;
}

/** crates/iskra-core/src/settings.rs::SettingsPatch — отсутствующее поле = «не менять». */
export interface SettingsPatch {
  theme?: Theme;
  hotkey?: string;
  autostart?: boolean;
}

/** Payload события hotkey://changed: фактический хоткей после remap/фолбэка. */
export interface HotkeyChanged {
  hotkey: string;
  fromFallback: boolean;
}

/** Ответ get_runtime_info(): что реально зарегистрировано сейчас. */
export interface RuntimeInfo {
  activeHotkey: string | null;
  fallbackActive: boolean;
}

/** Ошибка update_settings: "invalidHotkey" | "hotkeyBusy" | {"io":{"message":...}}. */
export type SettingsError = "invalidHotkey" | "hotkeyBusy" | { io: { message: string } };

// --- Имена каналов (часть контракта; тестом зафиксированы в ipc.rs) ---

export const EVENT_SETTINGS_CHANGED = "settings://changed";
export const EVENT_HOTKEY_CHANGED = "hotkey://changed";
export const EVENT_NAV_SETTINGS = "nav://settings";
