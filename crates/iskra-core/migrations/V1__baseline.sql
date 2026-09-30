-- Фаза 2, шаг 0 (D3): базовая миграция refinery — проверка пайплайна
-- embedded-миграций поверх rusqlite (смоук-тест в crates/iskra-core/src/search/mod.rs).
-- Служебная таблица app_meta: версия схемы, отметки времени полных ресканов и т.п.
-- Таблицы индекса (file_index, file_index_fts, usage) добавит шаг 2 миграцией V2.
CREATE TABLE IF NOT EXISTS app_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
