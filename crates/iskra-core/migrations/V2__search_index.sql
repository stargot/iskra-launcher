-- Фаза 2, шаг 2 (план 2026-09-30, §3; решение 6, §4.3 спеки): таблицы файлового
-- индекса. Риск 8: name пишется НОРМАЛИЗОВАННЫМ (lowercase + ё→е) — нормализация
-- выполняется на стороне db.rs при записи (единая точка), FTS5 unicode61.
-- path хранится в оригинальном регистре — из него UI берёт имя для показа.

-- Файловый индекс (полный рескан — INSERT..ON CONFLICT, mtime-инкремент).
CREATE TABLE IF NOT EXISTS file_index (
    path  TEXT PRIMARY KEY,     -- полный путь, оригинальный регистр
    name  TEXT NOT NULL,        -- имя файла, нормализованное (lowercase, ё→е)
    ext   TEXT,                 -- расширение без точки, нормализованное; NULL если нет
    mtime INTEGER NOT NULL,     -- unix-секунды модификации
    size  INTEGER NOT NULL      -- размер в байтах
);

-- FTS5 external content поверх file_index: индекс живёт rowid-в-rowid,
-- синхронизация — триггерами (стандартная схема external-content FTS5).
CREATE VIRTUAL TABLE IF NOT EXISTS file_index_fts USING fts5(
    name,
    content = 'file_index',
    content_rowid = 'rowid',
    tokenize = 'unicode61'
);

CREATE TRIGGER IF NOT EXISTS file_index_ai AFTER INSERT ON file_index BEGIN
    INSERT INTO file_index_fts(rowid, name) VALUES (new.rowid, new.name);
END;

CREATE TRIGGER IF NOT EXISTS file_index_ad AFTER DELETE ON file_index BEGIN
    INSERT INTO file_index_fts(file_index_fts, rowid, name)
    VALUES ('delete', old.rowid, old.name);
END;

CREATE TRIGGER IF NOT EXISTS file_index_au AFTER UPDATE OF name ON file_index BEGIN
    INSERT INTO file_index_fts(file_index_fts, rowid, name)
    VALUES ('delete', old.rowid, old.name);
    INSERT INTO file_index_fts(rowid, name) VALUES (new.rowid, new.name);
END;

-- Статистика запусков элементов (агрегатор: usage-бонус и recents на шаге 4).
CREATE TABLE IF NOT EXISTS usage (
    id         TEXT PRIMARY KEY,        -- "{provider}:{ключ}", напр. "apps:c:\...\chrome.exe"
    used_count INTEGER NOT NULL DEFAULT 0,
    used_at    INTEGER NOT NULL DEFAULT 0   -- unix-миллисекунды последнего запуска, 0 = никогда
);
