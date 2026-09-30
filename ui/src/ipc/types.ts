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

// --- Фаза 2, шаг 4: поиск (зеркало ipc.rs + search/types.rs) ---

/** search/types.rs::SystemCommand (serde camelCase). */
export type SystemCommand =
  | "lock"
  | "monitorOff"
  | "sleep"
  | "restart"
  | "shutdown"
  | "emptyRecycleBin";

/** search/types.rs::ItemAction — тегированный enum, тег "type", варианты camelCase. */
export type ItemAction =
  | { type: "launchApp"; path: string; args: string | null }
  | { type: "openPath"; path: string }
  | { type: "openUri"; uri: string }
  | { type: "webSearch"; url: string }
  | { type: "copyText"; text: string }
  | { type: "system"; command: SystemCommand };

/** search/types.rs::SearchItem — строка выдачи (итоговый скор после агрегатора). */
export interface SearchItem {
  id: string;
  provider: "apps" | "calc" | "settings" | "system" | "files" | "web";
  title: string;
  subtitle: string | null;
  /** Полный путь к PNG в %APPDATA%/iskra/icons (для convertFileSrc). */
  iconPath: string | null;
  score: number;
  action: ItemAction;
}

/** Ответ search(q) и payload события search://updated (D8). */
export interface SearchResponse {
  queryId: number;
  items: SearchItem[];
}

/** Ошибка run_item(id): "notFound" | {"action":{"message":...}}. */
export type SearchError = "notFound" | { action: { message: string } };

/** indexer.rs::IndexPhase (serde camelCase). */
export type IndexPhase = "scanning" | "indexing" | "cleaning" | "done";

/** Payload index://progress и ответ get_index_status(). */
export interface IndexStatus {
  phase: IndexPhase;
  done: number;
  total: number;
}

// --- Имена каналов (часть контракта; тестом зафиксированы в ipc.rs) ---

export const EVENT_SETTINGS_CHANGED = "settings://changed";
export const EVENT_HOTKEY_CHANGED = "hotkey://changed";
export const EVENT_NAV_SETTINGS = "nav://settings";
export const EVENT_SEARCH_UPDATED = "search://updated";
export const EVENT_INDEX_PROGRESS = "index://progress";
export const EVENT_ICONS_UPDATED = "icons://updated";
