//! Сниппеты (Фаза 3, шаг 1; план §1 D9, §4.3 спеки): репозиторий CRUD поверх
//! общей `Db` (миграция V3) + `SnippetsProvider` — вход сниппетов в общий поиск
//! (SearchProvider, priority 85 — `ranking::PRIORITY_SNIPPETS`): матчит
//! name/keywords/body, действие — `ItemAction::CopyText { text: body }`
//! (авто-вставку по цепочке D6 делает сервис app, шаг 3).

use std::sync::Arc;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::search::db::{Db, DbError};
use crate::search::fuzzy;
use crate::search::provider::SearchProvider;
use crate::search::ranking;
use crate::search::types::{ItemAction, SearchItem};

/// Ошибки репозитория сниппетов (БД + валидация формы).
#[derive(Debug)]
pub enum SnippetError {
    Db(DbError),
    /// Пустое имя или тело — сниппет без них не имеет смысла в поиске/вставке.
    EmptyField(&'static str),
}

impl std::fmt::Display for SnippetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnippetError::Db(e) => write!(f, "БД: {e}"),
            SnippetError::EmptyField(name) => write!(f, "пустое поле: {name}"),
        }
    }
}

impl std::error::Error for SnippetError {}

impl From<DbError> for SnippetError {
    fn from(e: DbError) -> Self {
        SnippetError::Db(e)
    }
}

impl From<rusqlite::Error> for SnippetError {
    fn from(e: rusqlite::Error) -> Self {
        SnippetError::Db(DbError::Sql(e))
    }
}

pub type Result<T> = std::result::Result<T, SnippetError>;

/// Сниппет (IPC-контракт; camelCase в JSON). `keywords` — строка пользователя
/// («почта адрес»), матчится как единый текст через fuzzy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub id: i64,
    pub name: String,
    pub body: String,
    pub keywords: String,
    /// unix-миллисекунды создания.
    pub created_at: i64,
}

/// Репозиторий сниппетов поверх общего `Db`.
pub struct SnippetStore {
    db: Arc<Db>,
}

impl SnippetStore {
    pub fn new(db: Arc<Db>) -> Self {
        SnippetStore { db }
    }

    /// Создать сниппет; имя и тело обязательны (MVP-валидация: остальные
    /// формы — на стороне UI/команд).
    pub fn create(&self, name: &str, body: &str, keywords: &str, now_ms: i64) -> Result<Snippet> {
        let name = name.trim();
        if name.is_empty() {
            return Err(SnippetError::EmptyField("name"));
        }
        if body.trim().is_empty() {
            return Err(SnippetError::EmptyField("body"));
        }
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO snippets(name, body, keywords, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, body, keywords.trim(), now_ms],
        )?;
        let id = conn.last_insert_rowid();
        Ok(Snippet {
            id,
            name: name.to_string(),
            body: body.to_string(),
            keywords: keywords.trim().to_string(),
            created_at: now_ms,
        })
    }

    /// Все сниппеты по алфавиту (экран настроек).
    pub fn list(&self) -> Result<Vec<Snippet>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, body, keywords, created_at
             FROM snippets ORDER BY name COLLATE NOCASE ASC, id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Snippet {
                id: r.get(0)?,
                name: r.get(1)?,
                body: r.get(2)?,
                keywords: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Обновить сниппет; true — найден и обновлён.
    pub fn update(&self, id: i64, name: &str, body: &str, keywords: &str) -> Result<bool> {
        let name = name.trim();
        if name.is_empty() {
            return Err(SnippetError::EmptyField("name"));
        }
        if body.trim().is_empty() {
            return Err(SnippetError::EmptyField("body"));
        }
        let conn = self.db.conn();
        let n = conn.execute(
            "UPDATE snippets SET name = ?1, body = ?2, keywords = ?3 WHERE id = ?4",
            params![name, body, keywords.trim(), id],
        )?;
        Ok(n > 0)
    }

    /// Удалить сниппет; true — удалён.
    pub fn delete(&self, id: i64) -> Result<bool> {
        let conn = self.db.conn();
        let n = conn.execute("DELETE FROM snippets WHERE id = ?1", [id])?;
        Ok(n > 0)
    }
}

/// Провайдер сниппетов в общем поиске (D9): каждый сниппет скорится fuzzy по
/// name, keywords и body; берётся лучший скор, мусор (< MIN_MEANINGFUL) отсекается.
pub struct SnippetsProvider {
    store: SnippetStore,
}

impl SnippetsProvider {
    pub fn new(db: Arc<Db>) -> Self {
        SnippetsProvider { store: SnippetStore::new(db) }
    }

    /// Доступ к CRUD-репозиторию (настройки UI, шаг 5).
    pub fn store(&self) -> &SnippetStore {
        &self.store
    }
}

impl SearchProvider for SnippetsProvider {
    fn name(&self) -> &str {
        "snippets"
    }

    fn priority(&self) -> i64 {
        ranking::PRIORITY_SNIPPETS
    }

    /// Пустой запрос → провайдер молчит (контракт трейта). Ошибка БД — лог +
    /// пустая выдача, не паника (как у FilesProvider).
    fn query(&self, q: &str) -> Vec<SearchItem> {
        if q.trim().is_empty() {
            return Vec::new();
        }
        let snippets = match self.store.list() {
            Ok(s) => s,
            Err(e) => {
                crate::logging::warn(&format!("snippets: ошибка чтения БД: {e}"));
                return Vec::new();
            }
        };
        let mut items: Vec<SearchItem> = snippets
            .into_iter()
            .filter_map(|sn| {
                let score = fuzzy::score(q, &sn.name)
                    .max(fuzzy::score(q, &sn.keywords))
                    .max(fuzzy::score(q, &sn.body));
                if score < fuzzy::MIN_MEANINGFUL {
                    return None;
                }
                let mut item = SearchItem::new(
                    format!("snippets:{}", sn.id),
                    self.name(),
                    sn.name.clone(),
                    ItemAction::CopyText { text: sn.body.clone() },
                );
                // вторая строка: keywords, иначе первая строка тела
                item.subtitle = if sn.keywords.is_empty() {
                    Some(sn.body.lines().next().unwrap_or_default().to_string())
                } else {
                    Some(sn.keywords.clone())
                };
                item.score = score;
                Some(item)
            })
            .collect();
        items.sort_by(|a, b| b.score.cmp(&a.score).then(a.title.cmp(&b.title)));
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::db::Db;

    fn store() -> (Arc<Db>, SnippetStore) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let st = SnippetStore::new(db.clone());
        (db, st)
    }

    /// CRUD: create → list (по алфавиту) → update → delete; валидация пустых.
    #[test]
    fn crud_roundtrip() {
        let (_db, st) = store();
        let a = st.create("Адрес", "ул. Ленина, 1", "адрес почта", 1000).unwrap();
        let b = st.create("Блок", "/* код */", "", 2000).unwrap();
        assert_eq!(a.id, 1);
        assert_eq!(b.id, 2);

        let all = st.list().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "Адрес", "list по алфавиту");
        assert_eq!(all[0].keywords, "адрес почта");

        assert!(st.update(b.id, "Блок кода", "fn main() {}", "код").unwrap());
        let got = &st.list().unwrap()[1];
        assert_eq!((got.name.as_str(), got.body.as_str(), got.keywords.as_str()),
                   ("Блок кода", "fn main() {}", "код"));

        assert!(st.delete(a.id).unwrap());
        assert!(!st.delete(a.id).unwrap(), "повторное удаление → false");
        assert_eq!(st.list().unwrap().len(), 1);

        assert!(matches!(st.create("", "x", "", 1), Err(SnippetError::EmptyField("name"))));
        assert!(matches!(st.create("n", "  ", "", 1), Err(SnippetError::EmptyField("body"))));
        assert!(matches!(st.update(999, "", "x", ""), Err(SnippetError::EmptyField(_))));
    }

    /// Провайдер: матчит name/keywords/body, priority 85 (D9), пустой запрос
    /// молчит, действие — CopyText с телом сниппета.
    #[test]
    fn provider_matches_name_keywords_body() {
        let (db, st) = store();
        st.create("Адрес", "ул. Ленина, 1", "адрес почта", 1000).unwrap();
        st.create("Приветствие", "Привет мир!", "", 2000).unwrap();
        st.create("Совсем другое", "таблица умножения", "справка", 3000).unwrap();

        let p = SnippetsProvider::new(db);
        assert_eq!(p.name(), "snippets");
        assert_eq!(p.priority(), 85);
        assert_eq!(p.priority(), ranking::PRIORITY_SNIPPETS);
        assert!(p.query("").is_empty(), "пустой запрос провайдер молчит");

        // по имени
        let by_name = p.query("адрес");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].title, "Адрес");
        assert_eq!(by_name[0].id, format!("snippets:{}", 1));
        assert_eq!(by_name[0].provider, "snippets");
        assert_eq!(by_name[0].subtitle.as_deref(), Some("адрес почта"));
        assert!(by_name[0].score >= fuzzy::MIN_MEANINGFUL);
        assert_eq!(
            by_name[0].action,
            ItemAction::CopyText { text: "ул. Ленина, 1".to_string() },
            "D9: действие — копирование тела"
        );

        // по keywords (body-слово «почта» есть только в keywords)…
        assert_eq!(p.query("почта").len(), 1, "матч по keywords");
        // …по body («ленина» — только в теле)
        let by_body = p.query("ленина");
        assert_eq!(by_body.len(), 1, "матч по body");
        assert_eq!(by_body[0].title, "Адрес");
        // по ё/регистру в имени
        assert_eq!(p.query("ПРИВЕТСТВИЕ").len(), 1);

        // ничего не подходит
        assert!(p.query("зззф").is_empty());
        // лучший матч первым при пересечении
        st.create("Адрес 2", "второй адрес", "адрес", 4000).unwrap();
        let two = p.query("адрес");
        assert_eq!(two.len(), 2);
        assert!(two[0].score >= two[1].score, "сортировка по скору");
    }
}
