//! Клипборд-история (Фаза 3, шаг 1; план §1 D3/D4/D5, §3 шаг 0–1). Репозиторий
//! поверх общей `Db` (миграция V3): add с дедуп-подъёмом по content_hash (D5),
//! вытеснение по лимитам (1000 записей / 200 МБ, LRU по used_at среди pinned=0),
//! list с normalize_name-поиском по превью (D3: LIKE-семантика без FTS5 —
//! записей ≤ 1000, фильтр в Rust), delete/pin/top-N.
//!
//! Изображения (D4, отступление от §4.3 спеки): истина — PNG-файл на диске
//! (`%APPDATA%\iskra\clipboard\{id}.png`), в БД только `image_path`; `content`
//! для image — NULL. Репозиторий принимает УЖЕ записанный файл (путь) и не
//! создаёт/не перемещает файлы сам: раскладку `{id}.png`/`{id}_thumb.png`
//! делает сервис (app, шаг 3). При дедуп-подъёме изображения сервис обязан
//! удалить застейдженный файл — новая запись не создаётся, путь в БД прежний.
//!
//! Размер изображений для лимита 200 МБ берётся с диска (fs::metadata) —
//! БД размер не хранит (схема §4.3 не менялась). Ограничение «изображение
//! > 20 МБ — skip» (D4) — ответственность сервиса: `MAX_IMAGE_BYTES` экспортирован
//! для него; репозиторий обслуживает только лимиты хранения.
//!
//! Крейт остаётся без Tauri/WinAPI: события и вставка — app/iskra-sys (шаги 2–3).

use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::logging;
use crate::search::db::{normalize_name, Db, DbError};

/// Лимиты хранения (D3): консты кода, не настройки (MVP).
pub const MAX_ENTRIES: usize = 1000;
/// 200 МБ — сумма размеров PNG-файлов изображений на диске.
pub const MAX_TOTAL_BYTES: i64 = 200 * 1024 * 1024;
/// D4/риск 6: изображения больше 20 МБ сервис пропускает (skip + лог).
pub const MAX_IMAGE_BYTES: i64 = 20 * 1024 * 1024;
/// Превью обрезается до 200 символов (2 строки списка, D8).
pub const PREVIEW_MAX_CHARS: usize = 200;

/// Ошибки репозитория клипборда: БД/IO + нарушение формы входа.
#[derive(Debug)]
pub enum ClipboardError {
    Db(DbError),
    Io(std::io::Error),
    /// Файл изображения не существует или не читается.
    ImageFile { path: String, message: String },
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipboardError::Db(e) => write!(f, "БД: {e}"),
            ClipboardError::Io(e) => write!(f, "io: {e}"),
            ClipboardError::ImageFile { path, message } => {
                write!(f, "файл изображения {path}: {message}")
            }
        }
    }
}

impl std::error::Error for ClipboardError {}

impl From<DbError> for ClipboardError {
    fn from(e: DbError) -> Self {
        ClipboardError::Db(e)
    }
}

impl From<rusqlite::Error> for ClipboardError {
    fn from(e: rusqlite::Error) -> Self {
        ClipboardError::Db(DbError::Sql(e))
    }
}

pub type Result<T> = std::result::Result<T, ClipboardError>;

/// Тип записи клипборда (IPC-контракт, зеркало ui/src/ipc/types.ts; D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipboardKind {
    Text,
    Image,
    Files,
}

impl ClipboardKind {
    /// Значение в БД (CHECK(kind IN ('text','image','files'))).
    pub fn as_str(&self) -> &'static str {
        match self {
            ClipboardKind::Text => "text",
            ClipboardKind::Image => "image",
            ClipboardKind::Files => "files",
        }
    }

    fn from_db(s: &str) -> Option<ClipboardKind> {
        match s {
            "text" => Some(ClipboardKind::Text),
            "image" => Some(ClipboardKind::Image),
            "files" => Some(ClipboardKind::Files),
            _ => None,
        }
    }
}

/// Запись клипборда (IPC-контракт; camelCase в JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardEntry {
    pub id: i64,
    pub kind: ClipboardKind,
    /// text: полный текст; files: JSON-массив путей; image: None (D4).
    pub content: Option<String>,
    /// Путь к PNG на диске; только image (D4).
    pub image_path: Option<String>,
    /// Строка для списка/поиска (оригинальный регистр, ≤ 200 символов).
    pub preview: String,
    pub pinned: bool,
    pub source_app: Option<String>,
    /// FNV-1a 64 (D5) в hex.
    pub content_hash: String,
    /// unix-миллисекунды.
    pub created_at: i64,
    /// unix-миллисекунды; подъём при повторном копировании (D5).
    pub used_at: i64,
    pub used_count: u32,
}

impl ClipboardEntry {
    /// Пути из files-записи (content — JSON-массив). Для не-files — пусто.
    pub fn files(&self) -> Vec<String> {
        match self.kind {
            ClipboardKind::Files => self
                .content
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

/// Новый элемент для записи в историю (формат читает iskra-sys, шаг 2; D2/D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardNew {
    /// Текст (CF_UNICODETEXT).
    Text { text: String },
    /// Изображение (CF_DIB→PNG, шаг 2): файл УЖЕ на диске, репозиторий хранит путь.
    Image { png_path: String },
    /// Список путей (CF_HDROP).
    Files { paths: Vec<String> },
}

/// Итог `add`: новая запись (или дедуп-подъём — D5) и сколько вытеснено лимитами.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddOutcome {
    /// None — только в патологическом кейсе: переполнение вызвали сами pinned,
    /// и свежая запись вытеснила сама себя (единственный непиннед-кандидат).
    /// Сервису в этом случае нечего показывать/вставлять.
    pub entry: Option<ClipboardEntry>,
    /// true — контент уже был: used_at=now, used_count+=1, новая запись НЕ создана.
    pub deduplicated: bool,
    /// Сколько непиннед записей вытеснено лимитами при этой вставке.
    pub evicted: usize,
}

/// FNV-1a 64 (D5) над доменом вида + байты контента; hex-строка для БД.
/// Доменный байт разводит виды: текст и files-список с теми же байтами не коллидят.
pub fn content_hash64(kind: ClipboardKind, bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in std::iter::once(match kind {
        ClipboardKind::Text => b't',
        ClipboardKind::Image => b'i',
        ClipboardKind::Files => b'f',
    })
    .chain(bytes.iter().copied())
    {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Превью ≤ 200 символов: переводы строк/табы → пробел, подряд идущие пробелы
/// схлопываются (2 строки списка, D8).
fn make_preview(text: &str) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut prev_ws = false;
    for c in text.chars() {
        let mapped = if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c };
        if mapped == ' ' {
            if prev_ws || count == 0 {
                continue; // подряд идущие и ведущие пробелы схлопываются
            }
            prev_ws = true;
        } else {
            prev_ws = false;
        }
        if count == PREVIEW_MAX_CHARS {
            break;
        }
        out.push(mapped);
        count += 1;
    }
    out
}

/// Репозиторий клипборд-истории поверх общего `Db` (соединение — `Db::conn`).
pub struct ClipboardStore {
    db: Arc<Db>,
}

impl ClipboardStore {
    pub fn new(db: Arc<Db>) -> Self {
        ClipboardStore { db }
    }

    /// Записать элемент (шаг сервиса: дедуп источника и исключения — до вызова).
    /// Дедуп-подъём (D5): запись с таким content_hash уже есть → used_at=now,
    /// used_count+=1, НОВАЯ запись не создаётся. Новая запись → обслуживание
    /// лимитов (LRU по used_at среди pinned=0; файлы вытесненных изображений
    /// удаляются с диска, best-effort). Один вызов = одна транзакция.
    pub fn add(
        &self,
        item: &ClipboardNew,
        source_app: Option<&str>,
        now_ms: i64,
    ) -> Result<AddOutcome> {
        let (kind, content, image_path, preview, hash) = match item {
            ClipboardNew::Text { text } => {
                let preview = make_preview(text);
                (
                    ClipboardKind::Text,
                    Some(text.clone()),
                    None,
                    preview,
                    content_hash64(ClipboardKind::Text, text.as_bytes()),
                )
            }
            ClipboardNew::Image { png_path } => {
                let bytes = std::fs::read(png_path).map_err(|e| ClipboardError::ImageFile {
                    path: png_path.clone(),
                    message: e.to_string(),
                })?;
                let dims = image::image_dimensions(png_path)
                    .map(|(w, h)| format!("[изображение {w}×{h}]"))
                    .unwrap_or_else(|_| "[изображение]".to_string());
                (
                    ClipboardKind::Image,
                    None, // D4: content для изображений пуст — истина в файле
                    Some(png_path.clone()),
                    dims,
                    content_hash64(ClipboardKind::Image, &bytes),
                )
            }
            ClipboardNew::Files { paths } => {
                let preview = make_preview(&file_names_joined(paths));
                // Vec<String> сериализуется всегда — unwrap_or_default для спокойствия lints
                let content = serde_json::to_string(paths).unwrap_or_default();
                let hash = content_hash64(ClipboardKind::Files, content.as_bytes());
                (ClipboardKind::Files, Some(content), None, preview, hash)
            }
        };

        let mut conn = self.db.conn();
        let tx = conn.transaction()?;
        let mut evicted_paths: Vec<String> = Vec::new();
        let mut evicted = 0usize;

        // Дедуп-подъём (D5, риск 5): одинаковый контент не плодит записи.
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM clipboard_entries WHERE content_hash = ?1",
                [&hash],
                |r| r.get(0),
            )
            .optional()?;
        let entry = if let Some(id) = existing {
            tx.execute(
                "UPDATE clipboard_entries SET used_at = ?1, used_count = used_count + 1
                 WHERE id = ?2",
                params![now_ms, id],
            )?;
            Some(fetch_entry(&tx, id)?)
        } else {
            tx.execute(
                "INSERT INTO clipboard_entries
                     (kind, content, image_path, preview, pinned, source_app,
                      content_hash, created_at, used_at, used_count)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?7, 1)",
                params![
                    kind.as_str(),
                    content,
                    image_path,
                    preview,
                    source_app,
                    hash,
                    now_ms
                ],
            )?;
            let id = tx.last_insert_rowid();
            // Лимиты (D3): число записей и суммарный размер изображений на диске.
            evicted = evict_over_limits(&tx, &mut evicted_paths)?;
            // None — запись тут же вытеснила сама себя (все остальные pinned).
            fetch_entry_opt(&tx, id)?
        };
        tx.commit()?;
        // Файлы вытесненных изображений — после коммита (в транзакции FS не нужен).
        remove_image_files(&evicted_paths);
        Ok(AddOutcome { entry, deduplicated: existing.is_some(), evicted })
    }

    /// Список: pinned сверху, далее по used_at DESC. `query` — поиск по превью
    /// (D3): нормализация normalize_name с обеих сторон (lowercase+ё→е),
    /// каждый токен запроса должен встречаться в превью (substring).
    pub fn list(&self, query: Option<&str>, limit: usize) -> Result<Vec<ClipboardEntry>> {
        let tokens: Vec<String> = query
            .map(|q| {
                normalize_name(q)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, kind, content, image_path, preview, pinned, source_app,
                    content_hash, created_at, used_at, used_count
             FROM clipboard_entries
             ORDER BY pinned DESC, used_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], row_to_entry)?;
        let mut out = Vec::new();
        for r in rows {
            let e = r?;
            let matched = tokens.is_empty()
                || {
                    let np = normalize_name(&e.preview);
                    tokens.iter().all(|t| np.contains(t))
                };
            if matched {
                out.push(e);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Последние N записей без поиска (экран клипборда, D8).
    pub fn top(&self, n: usize) -> Result<Vec<ClipboardEntry>> {
        self.list(None, n)
    }

    /// Одна запись по id (путь вставки: сервису нужен полный контент).
    pub fn get(&self, id: i64) -> Result<Option<ClipboardEntry>> {
        let conn = self.db.conn();
        fetch_entry_opt(&conn, id)
    }

    /// Удалить запись; файл изображения (и thumbnail рядом, D4) — best-effort
    /// с диска. true — запись удалена.
    pub fn delete(&self, id: i64) -> Result<bool> {
        let mut conn = self.db.conn();
        let tx = conn.transaction()?;
        let image_path: Option<Option<String>> = tx
            .query_row(
                "SELECT image_path FROM clipboard_entries WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(image_path) = image_path else { return Ok(false) };
        tx.execute("DELETE FROM clipboard_entries WHERE id = ?1", [id])?;
        tx.commit()?;
        if let Some(p) = image_path {
            remove_image_files(std::slice::from_ref(&p));
        }
        Ok(true)
    }

    /// Закрепить/открепить (D8). used_at не трогаем — хронология LRU сохраняется.
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<bool> {
        let conn = self.db.conn();
        let n = conn.execute(
            "UPDATE clipboard_entries SET pinned = ?1 WHERE id = ?2",
            params![pinned as i64, id],
        )?;
        Ok(n > 0)
    }
}

/// Имена файлов через «, » — превью files-записи.
fn file_names_joined(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| {
            Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn row_to_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<ClipboardEntry> {
    Ok(ClipboardEntry {
        id: r.get(0)?,
        kind: ClipboardKind::from_db(&r.get::<_, String>(1)?)
            .unwrap_or(ClipboardKind::Text), // CHECK в схеме; страховка сериализации
        content: r.get(2)?,
        image_path: r.get(3)?,
        preview: r.get(4)?,
        pinned: r.get::<_, i64>(5)? != 0,
        source_app: r.get(6)?,
        content_hash: r.get(7)?,
        created_at: r.get(8)?,
        used_at: r.get(9)?,
        used_count: r.get::<_, i64>(10)?.max(0) as u32,
    })
}

fn fetch_entry(conn: &Connection, id: i64) -> Result<ClipboardEntry> {
    fetch_entry_opt(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows.into())
}

fn fetch_entry_opt(conn: &Connection, id: i64) -> Result<Option<ClipboardEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, content, image_path, preview, pinned, source_app,
                content_hash, created_at, used_at, used_count
         FROM clipboard_entries WHERE id = ?1",
    )?;
    Ok(stmt.query_row([id], row_to_entry).optional()?)
}

/// Вытеснение по лимитам (вызывать в транзакции после INSERT): LRU по
/// used_at среди pinned=0. Сначала лимит числа записей, затем — суммарного
/// размера изображений (размеры — с диска, D4). Если непиннед не осталось,
/// а лимит превышен — останавливаемся с логом (pinned защищены).
fn evict_over_limits(tx: &rusqlite::Transaction<'_>, evicted_paths: &mut Vec<String>) -> Result<usize> {
    let mut evicted = 0usize;

    // 1) Лимит числа записей.
    loop {
        let count: i64 =
            tx.query_row("SELECT count(*) FROM clipboard_entries", [], |r| r.get(0))?;
        if count <= MAX_ENTRIES as i64 {
            break;
        }
        let Some(victim) = oldest_unpinned(tx)? else {
            logging::warn(&format!(
                "clipboard: лимит {MAX_ENTRIES} записей превышен, но все записи pinned — вытеснение остановлено"
            ));
            break;
        };
        delete_victim(tx, victim.0, evicted_paths);
        evicted += 1;
    }

    // 2) Лимит суммарного размера изображений (только image весит; размеры с диска).
    let mut total = total_image_bytes(tx)?;
    while total > MAX_TOTAL_BYTES {
        let Some((id, sz)) = oldest_unpinned_image_size(tx)? else {
            logging::warn(&format!(
                "clipboard: лимит {MAX_TOTAL_BYTES} байт превышен, но непиннед не осталось — вытеснение остановлено"
            ));
            break;
        };
        delete_victim(tx, id, evicted_paths);
        total -= sz;
        evicted += 1;
    }
    Ok(evicted)
}

/// Старейшая непиннед запись: (id, image_path). None — все pinned/пусто.
fn oldest_unpinned(tx: &rusqlite::Transaction<'_>) -> Result<Option<(i64, Option<String>)>> {
    Ok(tx
        .query_row(
            "SELECT id, image_path FROM clipboard_entries WHERE pinned = 0
             ORDER BY used_at ASC, id ASC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Размер файла старейшего непиннед ИЗОБРАЖЕНИЯ (для вычета из лимита байт).
fn oldest_unpinned_image_size(tx: &rusqlite::Transaction<'_>) -> Result<Option<(i64, i64)>> {
    let row: Option<(i64, Option<String>)> = tx
        .query_row(
            "SELECT id, image_path FROM clipboard_entries WHERE pinned = 0 AND kind = 'image'
             ORDER BY used_at ASC, id ASC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(id, p)| {
        let sz = p
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        (id, sz)
    }))
}

/// Суммарный размер PNG-файлов изображений на диске (D4).
fn total_image_bytes(tx: &rusqlite::Transaction<'_>) -> Result<i64> {
    let mut stmt = tx.prepare(
        "SELECT image_path FROM clipboard_entries WHERE kind = 'image' AND image_path IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut total = 0i64;
    for p in rows {
        if let Ok(meta) = std::fs::metadata(p?) {
            total += meta.len() as i64;
        }
    }
    Ok(total)
}

/// Удалить строку жертвы; путь файла — в список на удаление после коммита.
fn delete_victim(tx: &rusqlite::Transaction<'_>, id: i64, evicted_paths: &mut Vec<String>) {
    if let Ok(Some(Some(path))) = tx
        .query_row(
            "SELECT image_path FROM clipboard_entries WHERE id = ?1",
            [id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
    {
        evicted_paths.push(path);
    }
    let _ = tx.execute("DELETE FROM clipboard_entries WHERE id = ?1", [id]);
}

/// Best-effort удаление PNG и thumbnail рядом (имя `{stem}_thumb.png`, D4).
fn remove_image_files(paths: &[String]) {
    for p in paths {
        let path = Path::new(p);
        let _ = std::fs::remove_file(path);
        if let (Some(stem), Some(ext)) = (path.file_stem(), path.extension()) {
            if let Some(thumb) = path.parent().map(|dir| {
                dir.join(format!("{}_thumb.{}", stem.to_string_lossy(), ext.to_string_lossy()))
            }) {
                let _ = std::fs::remove_file(thumb);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::db::Db;

    fn store() -> (Arc<Db>, ClipboardStore) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let st = ClipboardStore::new(db.clone());
        (db, st)
    }

    /// add без вытеснения: возвращает запись (в обычных тестах новая запись живёт).
    fn add_entry(st: &ClipboardStore, item: &ClipboardNew, now_ms: i64) -> ClipboardEntry {
        st.add(item, None, now_ms)
            .expect("add ok")
            .entry
            .expect("в обычных тестах новая запись не вытесняется")
    }

    fn small_png(dir: &Path, name: &str, declared_len: Option<u64>) -> String {
        let path = dir.join(name);
        // цвет из имени, чтобы контенты файлов различались (иначе дедуп по хэшу схлопнет)
        let n = name.bytes().map(usize::from).sum::<usize>() as u8;
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([n, 20, 30, 255]));
        img.save(&path).expect("сохранить PNG");
        if let Some(len) = declared_len {
            let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.set_len(len).expect("set_len (разрежённый файл)");
        }
        path.to_string_lossy().into_owned()
    }

    /// База: add текста возвращает запись; поля и строки контракта на месте.
    #[test]
    fn add_text_and_get_roundtrip() {
        let (_db, st) = store();
        let out = st
            .add(&ClipboardNew::Text { text: "Привет мир".to_string() }, Some("notepad.exe"), 1000)
            .unwrap();
        assert!(!out.deduplicated);
        assert_eq!(out.evicted, 0);
        let e = out.entry.expect("новая запись на месте");
        assert_eq!(e.kind, ClipboardKind::Text);
        assert_eq!(e.content.as_deref(), Some("Привет мир"));
        assert_eq!(e.preview, "Привет мир");
        assert_eq!(e.source_app.as_deref(), Some("notepad.exe"));
        assert!(!e.pinned);
        assert_eq!((e.created_at, e.used_at, e.used_count), (1000, 1000, 1));

        let got = st.get(e.id).unwrap().expect("запись на месте");
        assert_eq!(got, e);
        assert!(st.get(999).unwrap().is_none());
    }

    /// Дедуп-подъём (D5, риск 5): повторный add того же текста → одна запись,
    /// used_at = новый момент, used_count растёт, created_at не меняется.
    #[test]
    fn dedup_bump_existing_entry() {
        let (_db, st) = store();
        let first = st
            .add(&ClipboardNew::Text { text: "excel данные".to_string() }, Some("excel.exe"), 1000)
            .unwrap()
            .entry
            .expect("первая запись на месте");
        let second = st
            .add(&ClipboardNew::Text { text: "excel данные".to_string() }, Some("excel.exe"), 5000)
            .unwrap();
        assert!(second.deduplicated, "вторая вставка того же контента — дедуп");
        assert_eq!(second.evicted, 0);

        let all = st.list(None, 100).unwrap();
        assert_eq!(all.len(), 1, "новая запись НЕ создаётся");
        let e = &all[0];
        assert_eq!(e.id, first.id);
        assert_eq!(e.used_at, 5000, "used_at поднят наверх");
        assert_eq!(e.used_count, 2);
        assert_eq!(e.created_at, 1000, "момент создания не меняется");

        // третий повтор → used_count = 3 (приёмка шага 3: «3× подряд → used_count=3»)
        st.add(&ClipboardNew::Text { text: "excel данные".to_string() }, None, 9000).unwrap();
        assert_eq!(st.list(None, 10).unwrap()[0].used_count, 3);

        // изображения с тем же байтовым контентом дедупятся отдельно по виду:
        // hash-домены разведены, «текст == байтам файла» не схлопывает записи
        assert_ne!(
            content_hash64(ClipboardKind::Text, b"x"),
            content_hash64(ClipboardKind::Files, b"x")
        );
    }

    /// Дедуп изображения по СОДЕРЖИМОМУ файла: тот же PNG из другого пути
    /// поднимает существующую запись (D5), новая не создаётся.
    #[test]
    fn image_dedup_by_file_content() {
        let dir = crate::search::db::unique_temp_dir("clip-img-dedup");
        let (_db, st) = store();
        let p1 = small_png(&dir, "a.png", None);
        let out1 = st
            .add(&ClipboardNew::Image { png_path: p1.clone() }, Some("snipaste.exe"), 1000)
            .unwrap();
        assert!(!out1.deduplicated);
        let e1 = out1.entry.expect("первая запись на месте");
        assert_eq!(e1.kind, ClipboardKind::Image);
        assert_eq!(e1.image_path.as_deref(), Some(p1.as_str()));
        assert_eq!(e1.content, None, "D4: content у изображений пуст");
        assert!(e1.preview.starts_with("[изображение 4×4]"), "{}", e1.preview);

        // тот же контент под другим именем → дедуп, путь в БД прежний
        let p2 = dir.join("b.png");
        std::fs::copy(&p1, &p2).unwrap();
        let out2 = st
            .add(
                &ClipboardNew::Image { png_path: p2.to_string_lossy().into_owned() },
                None,
                2000,
            )
            .unwrap();
        assert!(out2.deduplicated);
        let e2 = out2.entry.expect("дедуп возвращает существующую запись");
        assert_eq!(e2.id, e1.id);
        assert_eq!(e2.image_path.as_deref(), Some(p1.as_str()), "путь первой записи сохранён");
        assert_eq!(st.list(None, 10).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Поиск по превью с нормализацией (D3): «прив» находит «Привет мир»,
    /// регистр и ё ≡ е, многословный запрос — AND по токенам.
    #[test]
    fn search_preview_normalizes_case_and_yo() {
        let (_db, st) = store();
        st.add(&ClipboardNew::Text { text: "Привет мир".to_string() }, None, 1000).unwrap();
        st.add(&ClipboardNew::Text { text: "Отчёт за ёлку".to_string() }, None, 2000).unwrap();
        st.add(&ClipboardNew::Text { text: "untitled note".to_string() }, None, 3000).unwrap();

        let hits = st.list(Some("прив"), 10).unwrap();
        assert_eq!(hits.len(), 1, "«прив» → «Привет мир»");
        assert_eq!(hits[0].preview, "Привет мир");

        assert_eq!(st.list(Some("ПРИВ"), 10).unwrap().len(), 1, "регистр запроса");
        assert_eq!(st.list(Some("мир"), 10).unwrap().len(), 1);
        assert_eq!(st.list(Some("елк"), 10).unwrap().len(), 1, "е в запросе ≡ ё в превью");
        assert_eq!(st.list(Some("ЁЛК"), 10).unwrap().len(), 1, "ё в запросе ≡ е в превью");
        assert_eq!(st.list(Some("рив ми"), 10).unwrap().len(), 1, "substring в середине");
        assert_eq!(st.list(Some("прив untitled"), 10).unwrap().len(), 0, "токены — AND");
        assert!(st.list(Some("ззз"), 10).unwrap().is_empty());
        assert_eq!(st.list(Some(""), 10).unwrap().len(), 3, "пустой запрос = весь список");
        assert_eq!(st.list(None, 2).unwrap().len(), 2, "limit работает");
    }

    /// Порядок списка: pinned сверху, далее по used_at DESC.
    #[test]
    fn list_orders_pinned_first_then_recency() {
        let (_db, st) = store();
        let a = add_entry(&st, &ClipboardNew::Text { text: "a".into() }, 1000);
        let b = add_entry(&st, &ClipboardNew::Text { text: "b".into() }, 2000);
        let c = add_entry(&st, &ClipboardNew::Text { text: "c".into() }, 3000);

        assert_eq!(
            st.top(10).unwrap().iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![c.id, b.id, a.id],
            "свежие сверху"
        );
        assert!(st.set_pinned(a.id, true).unwrap());
        assert_eq!(
            st.top(10).unwrap().iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![a.id, c.id, b.id],
            "pinned сверху, потом used_at DESC"
        );
        assert!(st.set_pinned(a.id, false).unwrap());
        assert_eq!(st.top(10).unwrap()[0].id, c.id, "unpin возвращает в хронологию");
        assert!(!st.set_pinned(42, true).unwrap(), "несуществующий id → false");
    }

    /// Вытеснение 1000+10: лимит MAX_ENTRIES, LRU по used_at среди pinned=0,
    /// pinned переживает вытеснение даже будучи старейшей (план §3 шаг 1).
    #[test]
    fn eviction_1000_plus_10_spares_pinned() {
        let (_db, st) = store();
        let mut now = 1000i64;
        let pinned = add_entry(&st, &ClipboardNew::Text { text: "pinned-старейшина".into() }, now);
        assert!(st.set_pinned(pinned.id, true).unwrap());

        // +1005 непиннед (итого 1006 записей) → лишние 6 вытеснены
        for i in 0..1005 {
            now += 1;
            st.add(&ClipboardNew::Text { text: format!("текст {i}") }, None, now).unwrap();
        }
        let list = st.top(2000).unwrap();
        assert_eq!(list.len(), MAX_ENTRIES, "лимит 1000 записей соблюдён");
        assert!(list.iter().any(|e| e.id == pinned.id), "pinned выжил, хотя он старейший");
        assert!(
            !list.iter().any(|e| e.preview == "текст 0"),
            "старейшие непиннед вытеснены первыми"
        );
        assert_eq!(list.last().unwrap().preview, "текст 6", "старейший выживший непиннед на дне списка");

        // ещё +10 → снова 1000, ничего не растёт
        for i in 0..10 {
            now += 1;
            let out = st.add(&ClipboardNew::Text { text: format!("добавка {i}") }, None, now).unwrap();
            assert_eq!(out.evicted, 1, "каждая новая вставка вытесняет ровно одну");
        }
        let list = st.top(2000).unwrap();
        assert_eq!(list.len(), MAX_ENTRIES);
        assert!(list.iter().any(|e| e.id == pinned.id), "pinned жив и после дозаполнения");
        assert!(list.iter().any(|e| e.preview == "добавка 9"), "свежая добавка на месте");
    }

    /// Лимит 200 МБ: размер изображений считается с диска (D4); при переполнении
    /// вытесняется старейшее непиннед ИЗОБРАЖЕНИЕ (только они весят), pinned
    /// переживает любые переполнения.
    #[test]
    fn eviction_200mb_by_image_disk_size() {
        let dir = crate::search::db::unique_temp_dir("clip-200mb");
        let (_db, st) = store();
        // разрежённые файлы: metadata показывает заданный размер, байты не пишем
        let big1 = small_png(&dir, "big1.png", Some((120 * 1024 * 1024) as u64));
        let big2 = small_png(&dir, "big2.png", Some((120 * 1024 * 1024) as u64));

        let first = st
            .add(&ClipboardNew::Image { png_path: big1.clone() }, None, 1000)
            .unwrap();
        assert_eq!(first.evicted, 0, "120 МБ < 200 МБ — вытеснения нет");
        let second = st
            .add(&ClipboardNew::Image { png_path: big2.clone() }, None, 2000)
            .unwrap();
        assert_eq!(second.evicted, 1, "240 МБ > 200 МБ → старейшее изображение вытеснено");

        let list = st.list(None, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].image_path.as_deref(), Some(big2.as_str()));
        assert!(!std::path::Path::new(&big1).exists(), "файл вытесненного изображения удалён с диска");
        assert!(std::path::Path::new(&big2).exists());

        // pinned-изображение защищено: новый 120 МБ-файл при pinned big2
        // превышает лимит и вытесняет САМ себя (он — единственный непиннед)
        st.set_pinned(list[0].id, true).unwrap();
        let big3 = small_png(&dir, "big3.png", Some((120 * 1024 * 1024) as u64));
        let third = st.add(&ClipboardNew::Image { png_path: big3.clone() }, None, 3000).unwrap();
        assert_eq!(third.evicted, 1, "переполнение при единственном непиннед-кандидате (себя)");
        assert!(
            third.entry.is_none(),
            "свежая запись вытеснила сама себя — entry пуст (сервису нечего показывать)"
        );
        let list = st.list(None, 10).unwrap();
        assert_eq!(list.len(), 1, "остался только pinned big2");
        assert_eq!(list[0].image_path.as_deref(), Some(big2.as_str()), "pinned выжил");
        assert!(!std::path::Path::new(&big3).exists());

        // после пина — обычный LRU: 60 МБ влезает, следующие 60 МБ вытесняют предыдущие
        let big4 = small_png(&dir, "big4.png", Some((60 * 1024 * 1024) as u64));
        let fourth = st.add(&ClipboardNew::Image { png_path: big4.clone() }, None, 4000).unwrap();
        assert_eq!(fourth.evicted, 0, "120+60 ≤ 200 МБ");
        let big5 = small_png(&dir, "big5.png", Some((60 * 1024 * 1024) as u64));
        let fifth = st.add(&ClipboardNew::Image { png_path: big5.clone() }, None, 5000).unwrap();
        assert_eq!(fifth.evicted, 1, "120+60+60 > 200 → старейший непиннед (big4) вытеснен");
        let list = st.list(None, 10).unwrap();
        assert_eq!(list.len(), 2, "pinned big2 + свежий big5");
        assert!(list.iter().any(|e| e.image_path.as_deref() == Some(big5.as_str())));
        assert!(list.iter().all(|e| e.image_path.as_deref() != Some(big4.as_str())), "LRU среди непиннед");
        assert!(list.iter().any(|e| e.pinned), "pinned пережил все переполнения");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// delete: строка исчезает, PNG + thumbnail удаляются с диска (D4).
    #[test]
    fn delete_removes_row_and_image_files() {
        let dir = crate::search::db::unique_temp_dir("clip-delete");
        let (_db, st) = store();
        let png = small_png(&dir, "12.png", None);
        std::fs::write(dir.join("12_thumb.png"), b"thumb").unwrap();

        let e = add_entry(&st, &ClipboardNew::Image { png_path: png.clone() }, 1000);
        assert!(st.delete(e.id).unwrap());
        assert!(!st.delete(e.id).unwrap(), "повторное удаление → false");
        assert!(st.get(e.id).unwrap().is_none());
        assert!(!std::path::Path::new(&png).exists(), "PNG удалён");
        assert!(!dir.join("12_thumb.png").exists(), "thumbnail удалён");

        let t = add_entry(&st, &ClipboardNew::Text { text: "temp".into() }, 1000);
        assert!(st.delete(t.id).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// files: content — JSON-массив путей, files() разбирает; превью — имена.
    #[test]
    fn files_entry_content_and_preview() {
        let (_db, st) = store();
        let paths = vec![
            r"C:\docs\Отчёт.pdf".to_string(),
            r"C:\imgs\фото ёлки.png".to_string(),
        ];
        let e = add_entry(&st, &ClipboardNew::Files { paths: paths.clone() }, 1000);
        assert_eq!(e.kind, ClipboardKind::Files);
        assert_eq!(e.files(), paths, "content — JSON-массив, files() разбирает");
        assert_eq!(e.preview, "Отчёт.pdf, фото ёлки.png", "превью — оригинальный регистр");
        assert_eq!(st.list(Some("отчет"), 10).unwrap().len(), 1, "поиск по превью: е ≡ ё");

        // дедуп того же списка
        let again = st.add(&ClipboardNew::Files { paths }, None, 2000).unwrap();
        assert!(again.deduplicated);
        assert_eq!(st.list(None, 10).unwrap().len(), 1);
    }

    /// Превью длинного текста обрезается до 200 символов, переводы строк → пробел.
    #[test]
    fn preview_truncation_and_newlines() {
        let (_db, st) = store();
        let long = format!("x{}\ny", "о".repeat(PREVIEW_MAX_CHARS + 50));
        let e = add_entry(&st, &ClipboardNew::Text { text: long }, 1000);
        assert_eq!(e.preview.chars().count(), PREVIEW_MAX_CHARS);
        assert!(!e.preview.contains('\n'));

        let e2 = add_entry(&st, &ClipboardNew::Text { text: "строка1\r\nстрока2".into() }, 2000);
        assert_eq!(e2.preview, "строка1 строка2", "\\r\\n схлопывается в один пробел");
        assert_eq!(e2.content.as_deref(), Some("строка1\r\nстрока2"), "content хранится как есть");

        let e3 = add_entry(&st, &ClipboardNew::Text { text: "  много    пробелов   тут ".into() }, 3000);
        assert_eq!(e3.preview, "много пробелов тут ", "подряд идущие пробелы схлопываются");
    }

    /// ШАГ 0 (план §3, риск 7): V3 накатывается на КОПИЮ живой index.db
    /// пользователя (снимок VACUUM INTO через read-only соединение — живой БД
    /// и запущенному iskra.exe ничего не пишется). Старые таблицы и данные целы,
    /// повторное открытие идемпотентно.
    #[test]
    fn v3_migrates_copy_of_live_user_db() {
        let live = crate::logging::base_dir().join("index.db");
        if !live.exists() {
            eprintln!("skip: живой {} не найден (машина без фазы 2)", live.display());
            return;
        }
        let dir = crate::search::db::unique_temp_dir("clip-live-db");
        let copy = dir.join("index.db");

        // Снимок: read-only соединение + VACUUM INTO (согласованная копия WAL-БД);
        // фолбэк — файловое копирование db+wal ( shm не переносим).
        let snapshot = || -> std::io::Result<()> {
            let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_URI;
            let ro = rusqlite::Connection::open_with_flags(&live, flags)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            ro.busy_timeout(std::time::Duration::from_secs(5))
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let sql = format!(
                "VACUUM INTO '{}'",
                copy.to_string_lossy().replace('\'', "''")
            );
            ro.execute(&sql, [])
                .map_err(|e| std::io::Error::other(format!("VACUUM INTO: {e}")))?;
            Ok(())
        };
        if snapshot().is_err() {
            std::fs::copy(&live, &copy).expect("фолбэк: файловое копирование живой БД");
            for suffix in ["-wal", "-shm"] {
                let src = live.with_file_name(format!("index.db{suffix}"));
                if src.exists() {
                    std::fs::copy(&src, copy.with_file_name(format!("index.db{suffix}")))
                        .expect("фолбэк: копирование wal/shm");
                }
            }
        }
        assert!(copy.exists(), "копия живой БД создана");

        // Живая БД в V1+V2 → накатывается ровно V3; после раскатки Ф3 на машине
        // (живой iskra.exe открыл index.db) — уже в V3 → 0 применений, и это тоже
        // корректно (миграция идемпотентна, см. chек «повторное открытие» ниже).
        let mut conn = Connection::open(&copy).unwrap();
        let applied = Db::migrate_on(&mut conn).unwrap();
        assert!(
            applied <= 1,
            "на копии живой БД применяется не больше V3__clipboard (получено {applied})"
        );
        for table in ["clipboard_entries", "snippets"] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "таблица {table} создана V3");
        }
        // Старые данные не тронуты: file_index читается, usage на месте.
        let files: i64 = conn.query_row("SELECT count(*) FROM file_index", [], |r| r.get(0)).unwrap();
        let idx: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_clipboard%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 2, "оба индекса V3 созданы");
        eprintln!("v3 на копии живой БД: file_index строк = {files}");
        drop(conn);

        // Повторное открытие через Db::open — идемпотентно (0 миграций),
        // и обе старые фичи работают поверх расширенной схемы.
        {
            let db = Db::open(&copy).unwrap();
            assert_eq!(db.count_files().unwrap(), files);
            db.record_use("smoke:test", 1).unwrap();
            assert_eq!(db.used_count("smoke:test").unwrap(), 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
