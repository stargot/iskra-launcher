// Типизированные обёртки invoke()/listen() поверх каналов контракта
// (план шага 5; шаг 4 Фазы 2 — search/run_item/index/hide_window).
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  EVENT_CLIPBOARD_UPDATED,
  EVENT_HOTKEY_CHANGED,
  EVENT_ICONS_UPDATED,
  EVENT_INDEX_PROGRESS,
  EVENT_NAV_SETTINGS,
  EVENT_SEARCH_UPDATED,
  EVENT_SETTINGS_CHANGED,
  type ClipboardEntry,
  type HotkeyChanged,
  type IndexStatus,
  type RuntimeInfo,
  type SearchResponse,
  type Settings,
  type SettingsPatch,
  type Snippet,
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

// --- Команды поиска (шаг 4 Фазы 2) ---

/** Поиск: быстрые провайдеры синхронно; файловые — событием search://updated. */
export function search(q: string): Promise<SearchResponse> {
  return invoke("search", { q });
}

/** Исполнить элемент выдачи (usage++ + действие); ошибка — SearchError. */
export function runItem(id: string): Promise<void> {
  return invoke("run_item", { id });
}

/** Последний статус фонового индексатора. */
export function getIndexStatus(): Promise<IndexStatus> {
  return invoke("get_index_status");
}

/** Полный рескан в фоне. */
export function reindex(): Promise<void> {
  return invoke("reindex");
}

/** Скрыть окно лончера (Esc). */
export function hideWindow(): Promise<void> {
  return invoke("hide_window");
}

// --- Команды клипборда (шаг 3 Фазы 3) ---

/** История: pinned сверху, далее used_at DESC, топ-100; query — поиск по превью. */
export function clipboardList(query: string | null): Promise<ClipboardEntry[]> {
  return invoke("clipboard_list", { query });
}

/** Вставить запись в активное окно (цепочка D6); лончер скрывает сервис. */
export function clipboardPaste(id: number): Promise<void> {
  return invoke("clipboard_paste", { id });
}

/** Удалить запись (+ PNG/thumbnail на диске). */
export function clipboardDelete(id: number): Promise<boolean> {
  return invoke("clipboard_delete", { id });
}

/** Закрепить/открепить (D8). */
export function clipboardPinned(id: number, pinned: boolean): Promise<boolean> {
  return invoke("clipboard_pinned", { id, pinned });
}

// --- Команды сниппетов (шаг 5 Фазы 3, D9) ---

export function snippetsList(): Promise<Snippet[]> {
  return invoke("snippets_list");
}

/** Создать; пустые имя/тело отклоняются (SnippetError → строка). */
export function snippetCreate(name: string, body: string, keywords: string): Promise<Snippet> {
  return invoke("snippet_create", { name, body, keywords });
}

export function snippetUpdate(
  id: number,
  name: string,
  body: string,
  keywords: string,
): Promise<boolean> {
  return invoke("snippet_update", { id, name, body, keywords });
}

export function snippetDelete(id: number): Promise<boolean> {
  return invoke("snippet_delete", { id });
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

/** «Медленные» результаты (файлы); устаревшие queryId вызывающий отбрасывает. */
export function onSearchUpdated(cb: (response: SearchResponse) => void): Promise<UnlistenFn> {
  return listen<SearchResponse>(EVENT_SEARCH_UPDATED, (e) => cb(e.payload));
}

/** Прогресс фонового индексатора. */
export function onIndexProgress(cb: (status: IndexStatus) => void): Promise<UnlistenFn> {
  return listen<IndexStatus>(EVENT_INDEX_PROGRESS, (e) => cb(e.payload));
}

/** В кэш досыпались новые иконки (без списка) — стоит перезапросить search(). */
export function onIconsUpdated(cb: () => void): Promise<UnlistenFn> {
  return listen<null>(EVENT_ICONS_UPDATED, () => cb());
}

/** История клипборда изменилась (запись/дедуп/удаление/pin) — перезапросить список. */
export function onClipboardUpdated(cb: (entry: ClipboardEntry) => void): Promise<UnlistenFn> {
  return listen<ClipboardEntry>(EVENT_CLIPBOARD_UPDATED, (e) => cb(e.payload));
}
