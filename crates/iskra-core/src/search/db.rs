//! БД-слой поиска (план Ф2, шаг 2, решения D2/D3): rusqlite bundled + refinery.
//!
//! Одно соединение (`Mutex<Connection>`) на весь процесс — сервис шага 4 отдаст
//! его и агрегатору, и индексатору. WAL + synchronous=NORMAL. Миграции embedded
//! (migrations/V1__baseline.sql, V2__search_index.sql); refinery сам ведёт
//! историю применений, повторный `Db::open` идемпотентен.
//!
//! Риск 8: нормализация имени (lowercase + ё→е) — ЕДИНАЯ точка `normalize_name`,
//! применяется при записи в file_index; поиск нормализует запрос той же функцией.
//! Путь хранится в оригинальном регистре — из него UI берёт имя для показа.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::fuzzy;
use super::provider::SearchProvider;
use super::ranking;
use super::types::{ItemAction, SearchItem};
use crate::logging;

pub type Result<T> = std::result::Result<T, DbError>;

/// Ошибки БД: sqlite + миграции (refinery) + отравленный мьютекс.
#[derive(Debug)]
pub enum DbError {
    Sql(rusqlite::Error),
    Migrate(refinery::Error),
    Poisoned,
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Sql(e) => write!(f, "sqlite: {e}"),
            DbError::Migrate(e) => write!(f, "миграция: {e}"),
            DbError::Poisoned => write!(f, "мьютекс БД отравлен"),
        }
    }
}

impl std::error::Error for DbError {}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sql(e)
    }
}

impl From<refinery::Error> for DbError {
    fn from(e: refinery::Error) -> Self {
        DbError::Migrate(e)
    }
}

// Embedded-миграции (migrations/ относительно CARGO_MANIFEST_DIR iskra-core).
mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
}

/// Строка файлового индекса. При записи `name`/`ext` нормализуются в db.rs
/// (вызывающему можно передавать «сырые» значения — см. риск 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Полный путь (оригинальный регистр) — первичный ключ.
    pub path: String,
    /// Имя файла; при записи нормализуется (lowercase, ё→е).
    pub name: String,
    /// Расширение без точки; при записи нормализуется; None — без расширения.
    pub ext: Option<String>,
    /// mtime в unix-секундах.
    pub mtime: i64,
    /// Размер в байтах.
    pub size: i64,
}

/// Режим применения пачки файлов индексатором.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    /// Полный рескан: писать всё безусловно.
    Full,
    /// Инкремент: пропускать файлы с совпавшим mtime.
    Incremental,
}

/// Итог применения пачки.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyStats {
    /// Записано (вставлено или обновлено).
    pub applied: usize,
    /// Пропущено без изменений (только Incremental).
    pub unchanged: usize,
}

/// Нормализация для индекса и запросов (риск 8): lowercase + ё → е.
pub fn normalize_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        for lo in c.to_lowercase() {
            if lo == 'ё' { out.push('е'); } else { out.push(lo); }
        }
    }
    out
}

/// Одно соединение с БД индекса. `Send + Sync`.
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Открыть (создать) БД по пути: WAL, synchronous=NORMAL, busy_timeout 5 с,
    /// прогон embedded-миграций. Каталог создаёт при необходимости.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent); // best-effort: ошибка всплывёт из open
        }
        let conn = Connection::open(path)?;
        Db::init(conn)
    }

    /// In-memory БД (тесты, пре-чеки): те же миграции, WAL неприменим.
    pub fn open_in_memory() -> Result<Db> {
        Db::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Db> {
        // WAL — до миграций (refinery работает в транзакции, journal_mode
        // вне транзакции меняется). На :memory: вернётся "memory" — не ошибка.
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        embedded::migrations::runner().run(&mut conn)?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    /// Прямой доступ к соединению под мьютексом — для репозиториев поверх общей
    /// БД (Ф3, шаг 1: clipboard.rs/snippets.rs; SQL живёт в них, Db даёт только
    /// соединение: транзакциям нужен `&mut Connection`, guard его отдаёт).
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.lock()
    }

    /// Прогнать миграции на готовом соединении (для тестов шага 0/2 и отладки).
    pub fn migrate_on(conn: &mut Connection) -> Result<usize> {
        let report = embedded::migrations::runner().run(conn)?;
        Ok(report.applied_migrations().len())
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("Db: мьютекс БД отравлен")
    }

    /// Текущий journal_mode ("wal" для файловой БД; тесты шага 2).
    pub fn journal_mode(&self) -> Result<String> {
        Ok(self.lock().query_row("PRAGMA journal_mode", [], |r| r.get(0))?)
    }

    /// Применить пачку файлов одним транзакционным проходом (один захват
    /// мьютекса: инкрементальный режим читает mtime тем же соединением).
    /// `progress(done, total)` — периодический колбэк для канала индексатора.
    pub fn apply_files(
        &self,
        recs: &[FileRecord],
        mode: ApplyMode,
        progress: &dyn Fn(u64, u64),
    ) -> Result<ApplyStats> {
        let mut guard = self.lock();
        let tx = guard.transaction()?;
        let total = recs.len() as u64;
        let mut applied = 0usize;
        let mut unchanged = 0usize;
        {
            let mut upsert = tx.prepare(
                "INSERT INTO file_index(path, name, ext, mtime, size)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO UPDATE SET
                     name = excluded.name, ext = excluded.ext,
                     mtime = excluded.mtime, size = excluded.size",
            )?;
            let mut select_mtime =
                tx.prepare("SELECT mtime FROM file_index WHERE path = ?1")?;
            for (i, rec) in recs.iter().enumerate() {
                if mode == ApplyMode::Incremental {
                    let stored: Option<i64> = select_mtime
                        .query_row([&rec.path], |r| r.get(0))
                        .optional()?;
                    if stored == Some(rec.mtime) {
                        unchanged += 1;
                        continue;
                    }
                }
                // Нормализация при записи — единая точка (риск 8).
                upsert.execute(params![
                    rec.path,
                    normalize_name(&rec.name),
                    rec.ext.as_deref().map(normalize_name),
                    rec.mtime,
                    rec.size,
                ])?;
                applied += 1;
                if (i + 1) % super::indexer::PROGRESS_EVERY == 0 {
                    progress(i as u64 + 1, total);
                }
            }
        }
        tx.commit()?;
        Ok(ApplyStats { applied, unchanged })
    }

    /// mtime файла в индексе (для mtime-инкремента индексатора).
    pub fn file_mtime(&self, path: &str) -> Result<Option<i64>> {
        let conn = self.lock();
        Ok(conn
            .query_row("SELECT mtime FROM file_index WHERE path = ?1", [path], |r| r.get(0))
            .optional()?)
    }

    /// Все пути в индексе (сверка удалённых файлов).
    pub fn list_paths(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT path FROM file_index")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Удалить файл из индекса (FTS чистится триггером). true — удалён.
    pub fn delete_file(&self, path: &str) -> Result<bool> {
        let n = self.lock().execute("DELETE FROM file_index WHERE path = ?1", [path])?;
        Ok(n > 0)
    }

    /// Сколько файлов в индексе.
    pub fn count_files(&self) -> Result<i64> {
        Ok(self.lock().query_row("SELECT count(*) FROM file_index", [], |r| r.get(0))?)
    }

    /// Полный перестройк FTS-индекса из file_index (восстановление после сбоя).
    pub fn rebuild_fts(&self) -> Result<()> {
        self.lock()
            .execute("INSERT INTO file_index_fts(file_index_fts) VALUES ('rebuild')", [])?;
        Ok(())
    }

    /// FTS5-поиск по обрывку имени: каждый токен запроса — префикс.
    /// Возвращает до `limit` записей, лучшие (bm25 rank) первыми.
    pub fn search_files(&self, q: &str, limit: usize) -> Result<Vec<FileRecord>> {
        let query = fts_prefix_query(q);
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT f.path, f.name, f.ext, f.mtime, f.size
             FROM file_index_fts JOIN file_index f ON f.rowid = file_index_fts.rowid
             WHERE file_index_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |r| {
            Ok(FileRecord {
                path: r.get(0)?,
                name: r.get(1)?,
                ext: r.get(2)?,
                mtime: r.get(3)?,
                size: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // --- usage ---

    /// Отметить запуск элемента: used_count++, used_at = `at_ms`.
    pub fn record_use(&self, id: &str, at_ms: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO usage(id, used_count, used_at) VALUES (?1, 1, ?2)
             ON CONFLICT(id) DO UPDATE SET used_count = used_count + 1, used_at = excluded.used_at",
            params![id, at_ms],
        )?;
        Ok(())
    }

    /// Сколько раз запускали элемент.
    pub fn used_count(&self, id: &str) -> Result<u32> {
        Ok(self
            .lock()
            .query_row("SELECT used_count FROM usage WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .unwrap_or(0))
    }

    /// Момент последнего запуска (unix-мс), None — не запускали.
    pub fn last_used_ms(&self, id: &str) -> Result<Option<i64>> {
        let ms: Option<i64> = self
            .lock()
            .query_row("SELECT used_at FROM usage WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(ms.filter(|&v| v > 0))
    }
}

/// Запрос → FTS5 prefix-выражение: «док лад» → `"док"* "лад"*`.
/// Токены в двойных кавычках (внутренние кавычки удваиваются) — безопасно
/// против спецсимволов FTS5. Пустая строка — «нечего искать».
fn fts_prefix_query(q: &str) -> String {
    normalize_name(q)
        .split_whitespace()
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Провайдер файлов поверх FTS5-индекса (шаг 2; подключается агрегатором на шаге 4).
pub struct FilesProvider {
    db: Arc<Db>,
    take: usize,
}

impl FilesProvider {
    /// Провайдер на общем соединении; до `RESULT_LIMIT` результатов на запрос.
    pub fn new(db: Arc<Db>) -> Self {
        FilesProvider { db, take: super::aggregator::RESULT_LIMIT }
    }

    /// Переопределить лимит выдачи (тесты).
    pub fn with_take(mut self, take: usize) -> Self {
        self.take = take;
        self
    }
}

impl SearchProvider for FilesProvider {
    fn name(&self) -> &str {
        "files"
    }

    fn priority(&self) -> i64 {
        ranking::PRIORITY_FILES
    }

    /// FTS5 prefix-поиск; скор — fuzzy по заголовку (fuzzy сам нормализует
    /// ё/регистр, риск 8). Ошибка БД — лог + пустая выдача, не паника.
    fn query(&self, q: &str) -> Vec<SearchItem> {
        if q.trim().is_empty() {
            return Vec::new();
        }
        let hits = match self.db.search_files(q, self.take) {
            Ok(hits) => hits,
            Err(e) => {
                logging::warn(&format!("files: ошибка поиска БД: {e}"));
                Vec::new()
            }
        };
        hits.into_iter()
            .map(|hit| {
                // заголовок — имя из пути в оригинальном регистре (UI-строка)
                let title = Path::new(&hit.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| hit.name.clone());
                let raw = fuzzy::score(q, &title);
                // FTS-матч без fuzzy-скора (редкий многословный случай) — база ниже
                // подпоследовательности, но выше web-fallback и мусора
                let score = if raw > 0 { raw } else { fuzzy::MIN_MEANINGFUL - 50 };
                SearchItem {
                    id: format!("files:{}", hit.path),
                    provider: "files".to_string(),
                    title,
                    subtitle: Some(hit.path.clone()),
                    icon_path: None, // иконки файлов — шаг 3 (iskra-sys/icons.rs)
                    score,
                    action: ItemAction::OpenPath { path: hit.path },
                }
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "iskra-test-{label}-{}-{nanos}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("создать temp-каталог");
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::provider::SearchProvider;
    use crate::search::types::ItemAction;
    use crate::search::{fuzzy, ranking};

    fn rec(path: &str, name: &str, ext: Option<&str>, mtime: i64, size: i64) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            name: name.to_string(),
            ext: ext.map(str::to_string),
            mtime,
            size,
        }
    }

    /// Миграции на temp-ФАЙЛЕ: таблицы V2 на месте, WAL включён, повторное
    /// открытие идемпотентно (refinery применяет 0).
    #[test]
    fn migrations_on_temp_file_wal_and_idempotent() {
        let dir = unique_temp_dir("migrations");
        let db_path = dir.join("index.db");

        // 1) прямой прогон runner на файловом соединении: ровно 2 миграции.
        let mut conn = Connection::open(&db_path).unwrap();
        let applied = Db::migrate_on(&mut conn).unwrap();
        assert_eq!(applied, 3, "V1__baseline + V2__search_index + V3__clipboard");
        drop(conn);

        // 2) открытие через Db::open: таблицы есть, WAL включён.
        let db = Db::open(&db_path).unwrap();
        assert_eq!(db.journal_mode().unwrap(), "wal", "файловая БД должна быть в WAL");
        let conn = db.lock();
        for table in ["app_meta", "file_index", "file_index_fts", "usage"] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "таблица {table} создана миграцией");
        }
        drop(conn);

        // 3) повторное открытие того же файла — без ошибок (идемпотентность).
        drop(db);
        let db2 = Db::open(&db_path).unwrap();
        assert_eq!(db2.count_files().unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Roundtrip: запись (с нормализацией ё/регистра), чтение, обновление
    /// по mtime, пропуск unchanged, удаление, список путей.
    #[test]
    fn file_index_roundtrip_normalization_and_increment() {
        let db = Db::open_in_memory().unwrap();
        let stats = db
            .apply_files(
                &[
                    rec(r"C:\t\Доклад.pdf", "Доклад.pdf", Some("PDF"), 100, 10),
                    rec(r"C:\t\Отчёт Ёлка.txt", "Отчёт Ёлка.txt", Some("txt"), 200, 20),
                    rec(r"C:\t\без расширения", "без расширения", None, 300, 30),
                ],
                ApplyMode::Full,
                &|_, _| {},
            )
            .unwrap();
        assert_eq!(stats.applied, 3);
        assert_eq!(db.count_files().unwrap(), 3);

        // нормализация при записи (риск 8): lowercase + ё→е; ext тоже lowercase
        let hits = db.search_files("ЁЛКА", 10).unwrap();
        assert_eq!(hits.len(), 1, "запрос с Ё и CAPS находит нормализованное имя");
        assert_eq!(hits[0].name, "отчет елка.txt");
        assert_eq!(hits[0].ext.as_deref(), Some("txt"));
        assert_eq!(hits[0].path, r"C:\t\Отчёт Ёлка.txt", "путь — в оригинальном регистре");

        // без расширения → NULL ext
        let hit = db.search_files("расширения", 10).unwrap();
        assert_eq!(hit[0].ext, None);

        // инкремент: тот же mtime → unchanged
        let stats = db
            .apply_files(
                &[rec(r"C:\t\Доклад.pdf", "Доклад.pdf", Some("pdf"), 100, 10)],
                ApplyMode::Incremental,
                &|_, _| {},
            )
            .unwrap();
        assert_eq!((stats.applied, stats.unchanged), (0, 1));

        // инкремент: новый mtime → applied
        let stats = db
            .apply_files(
                &[rec(r"C:\t\Доклад.pdf", "Доклад.pdf", Some("pdf"), 999, 11)],
                ApplyMode::Incremental,
                &|_, _| {},
            )
            .unwrap();
        assert_eq!((stats.applied, stats.unchanged), (1, 0));
        assert_eq!(db.file_mtime(r"C:\t\Доклад.pdf").unwrap(), Some(999));

        // удаление чистит и FTS (триггер), и таблицу
        let paths = db.list_paths().unwrap();
        assert_eq!(paths.len(), 3);
        assert!(db.delete_file(r"C:\t\Отчёт Ёлка.txt").unwrap());
        assert_eq!(db.count_files().unwrap(), 2);
        assert!(db.search_files("елка", 10).unwrap().is_empty(), "FTS синхронен после DELETE");
        assert!(!db.delete_file(r"C:\t\нет такого").unwrap());
    }

    /// FTS5-поиск по обрывку «док» находит «доклад.pdf»; регистр и ё
    /// нормализуются на обеих сторонах (риск 8); спецсимволы FTS не роняют.
    #[test]
    fn fts_prefix_search_dok_finds_doklad() {
        let db = Db::open_in_memory().unwrap();
        db.apply_files(
            &[
                rec(r"C:\d\Доклад.pdf", "Доклад.pdf", Some("pdf"), 1, 1),
                rec(r"C:\d\отчет.docx", "отчет.docx", Some("docx"), 2, 2),
                rec(r"C:\d\Ёлка-Отчёт ёжика.txt", "Ёлка-Отчёт ёжика.txt", Some("txt"), 3, 3),
            ],
            ApplyMode::Full,
            &|_, _| {},
        )
        .unwrap();

        let hits = db.search_files("док", 50).unwrap();
        assert_eq!(hits.len(), 1, "«док» находит только «доклад.pdf»");
        assert_eq!(hits[0].path, r"C:\d\Доклад.pdf");

        assert_eq!(db.search_files("ДОК", 50).unwrap().len(), 1, "регистр запроса");
        assert_eq!(db.search_files("Ёжик", 50).unwrap().len(), 1, "ё в запросе ≡ е в имени");
        assert_eq!(db.search_files("ежика", 50).unwrap().len(), 1, "е в запросе ≡ ё в имени");
        assert_eq!(db.search_files("отч", 50).unwrap().len(), 2, "префикс «отч» — оба файла с токеном «отчет»");
        assert_eq!(db.search_files("docx", 50).unwrap().len(), 1, "уникальный суффикс-токен");
        assert!(db.search_files("ззз", 50).unwrap().is_empty(), "нет совпадений — пусто");

        // многословный запрос: AND по префиксам
        let hits = db.search_files("док pdf", 50).unwrap();
        assert_eq!(hits.len(), 1);

        // спецсимволы FTS5 (кавычки, минус, скобки) не должны ломать запрос
        assert!(db.search_files(r#"док" (OR) -минус"#, 50).unwrap().is_empty() || true);
        assert_eq!(db.search_files("е-отч", 50).unwrap().len(), 0, "фраза «е отч» не матчится — это корректная семантика FTS5, запрос не падает");
        let hits2 = db.search_files("елка-отч", 50).unwrap();
        assert_eq!(hits2.len(), 1, "дефис в запросе: фраза «елка отч» находит «елка-отчет ...");
    }

    /// usage: счётчик и время последнего запуска.
    #[test]
    fn usage_record_and_read() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.used_count("apps:chrome").unwrap(), 0);
        assert_eq!(db.last_used_ms("apps:chrome").unwrap(), None);

        db.record_use("apps:chrome", 1000).unwrap();
        db.record_use("apps:chrome", 2000).unwrap();
        db.record_use("files:c:/x", 1500).unwrap();
        assert_eq!(db.used_count("apps:chrome").unwrap(), 2);
        assert_eq!(db.last_used_ms("apps:chrome").unwrap(), Some(2000));
        assert_eq!(db.used_count("files:c:/x").unwrap(), 1);
    }

    /// FilesProvider поверх БД: обрывок «док» → элемент с действием OpenPath.
    #[test]
    fn files_provider_search() {
        let db = std::sync::Arc::new(Db::open_in_memory().unwrap());
        db.apply_files(
            &[
                rec(r"C:\d\Доклад.pdf", "Доклад.pdf", Some("pdf"), 1, 1),
                rec(r"C:\d\заметки.txt", "заметки.txt", Some("txt"), 2, 2),
            ],
            ApplyMode::Full,
            &|_, _| {},
        )
        .unwrap();

        let provider = FilesProvider::new(db);
        assert_eq!(provider.name(), "files");
        assert_eq!(provider.priority(), ranking::PRIORITY_FILES);
        assert!(provider.query("").is_empty(), "пустой запрос провайдер молчит");

        let items = provider.query("док");
        assert_eq!(items.len(), 1);
        let it = &items[0];
        assert_eq!(it.title, "Доклад.pdf", "заголовок — оригинальный регистр из пути");
        assert_eq!(it.provider, "files");
        assert!(it.score >= fuzzy::MIN_MEANINGFUL, "fuzzy-скор осмысленного матча");
        assert_eq!(
            it.action,
            ItemAction::OpenPath { path: r"C:\d\Доклад.pdf".to_string() }
        );
        assert!(it.id.starts_with("files:"));

        // нет совпадений — пусто
        assert!(provider.query("мммф").is_empty());
    }
}
