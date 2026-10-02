-- Фаза 3, шаг 1 (план 2026-09-30, §1 D3/D4/D5; §4.3 спеки): таблицы клипборд-истории
-- и сниппетов в общей БД index.db (WAL уже включён). Превью ищется через
-- normalize_name на стороне репозитория (clipboard.rs) — FTS5 не нужен на 1000
-- записях (D3). Отступление от §4.3 спеки (решение D4, зафиксировано в плане):
-- content BLOB → NULL для изображений; истина — PNG-файл на диске, путь в image_path.

-- Клипборд-история: лимиты 1000 записей / 200 МБ обслуживает репозиторий
-- (LRU по used_at среди pinned = 0, план §1 D3).
CREATE TABLE IF NOT EXISTS clipboard_entries (
    id           INTEGER PRIMARY KEY,
    kind         TEXT NOT NULL CHECK (kind IN ('text', 'image', 'files')),
    content      TEXT,               -- text: полный текст; files: JSON-массив путей; image: NULL (D4)
    image_path   TEXT,               -- путь к PNG на диске (%APPDATA%\iskra\clipboard\{id}.png); только image
    preview      TEXT NOT NULL,      -- строка для списка и поиска (оригинальный регистр)
    pinned       INTEGER NOT NULL DEFAULT 0,
    source_app   TEXT,               -- process name источника (D7); NULL — неизвестен
    content_hash TEXT NOT NULL,      -- FNV-1a 64 (D5) hex: дедуп подряд идущего контента
    created_at   INTEGER NOT NULL,   -- unix-миллисекунды
    used_at      INTEGER NOT NULL,   -- unix-миллисекунды (подъём при повторном копировании)
    used_count   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_clipboard_used_at ON clipboard_entries (used_at);
CREATE INDEX IF NOT EXISTS idx_clipboard_content_hash ON clipboard_entries (content_hash);

-- Сниппеты (§4.3 спеки, D9): CRUD в настройках, вставка из поиска через
-- SnippetsProvider (SearchProvider, priority 85 — ranking.rs).
CREATE TABLE IF NOT EXISTS snippets (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    body       TEXT NOT NULL,
    keywords   TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL      -- unix-миллисекунды
);
