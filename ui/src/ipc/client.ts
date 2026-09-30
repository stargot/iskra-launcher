// Типизированные обёртки invoke()/listen() поверх каналов контракта
// (план шага 5). Команды: get_settings / update_settings / get_runtime_info.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  EVENT_HOTKEY_CHANGED,
  EVENT_NAV_SETTINGS,
  EVENT_SETTINGS_CHANGED,
  type HotkeyChanged,
  type RuntimeInfo,
  type Settings,
  type SettingsPatch,
} from "./types";

// --- Команды (UI → core) ---

export function getSettings(): Promise<Settings> {
  return invoke("get_settings");
}

export function updateSettings(patch: SettingsPatch): Promise<Settings> {
  return invoke("update_settings", { patch });
}

export function getRuntimeInfo(): Promise<RuntimeInfo> {
  return invoke("get_runtime_info");
}

// --- События (core → UI) ---

export function onSettingsChanged(cb: (settings: Settings) => void): Promise<UnlistenFn> {
  return listen<Settings>(EVENT_SETTINGS_CHANGED, (e) => cb(e.payload));
}

export function onHotkeyChanged(cb: (changed: HotkeyChanged) => void): Promise<UnlistenFn> {
  return listen<HotkeyChanged>(EVENT_HOTKEY_CHANGED, (e) => cb(e.payload));
}

/** Навигация «Настройки» из трея; payload — null. */
export function onNavSettings(cb: () => void): Promise<UnlistenFn> {
  return listen<null>(EVENT_NAV_SETTINGS, () => cb());
}
