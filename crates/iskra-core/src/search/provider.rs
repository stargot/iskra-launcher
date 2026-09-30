//! Трейт провайдера поиска (план Ф2, шаг 1, §1 D1).
//!
//! Провайдеры — синхронные и быстрые (< 20 мс суммарно на 10k записей): вернули
//! `Vec<SearchItem>` с сырым fuzzy-скор в поле `score`. Медленные источники
//! (полный файловый индекс) в финале отдаются событием `search://updated` —
//! это склейка на стороне app (шаг 4), контракт трейта этого не меняет.

use super::types::SearchItem;

/// Источник результатов поиска. Реализации: calc, settings_win, web (шаг 1);
/// apps, files (поверх db.rs) — шаг 2.
pub trait SearchProvider: Send + Sync {
    /// Короткое имя провайдера — ключ приоритета и part of id элементов.
    /// Должно совпадать с константами ranking::PRIORITY_*: `apps`, `calc`,
    /// `settings`, `system`, `files`, `web`.
    fn name(&self) -> &str;

    /// Приоритет провайдера (единственная точка значений — ranking.rs).
    fn priority(&self) -> i64;

    /// Поиск по запросу. Контракт:
    /// - `""` → провайдер молчит (пустой запрос агрегатор обслуживает через
    ///   `UsageStore::recents`, см. aggregator.rs);
    /// - каждый элемент: `id = "{name}:{ключ}"`, в `score` — сырой fuzzy-скор
    ///   (итоговый скор считает агрегатор);
    /// - метод обязан быть потокобезопасным и быстро возвращаться.
    fn query(&self, q: &str) -> Vec<SearchItem>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::types::ItemAction;

    /// Проверка контракта на минимальной реализации: имя/id/пустой запрос.
    struct Echo;

    impl SearchProvider for Echo {
        fn name(&self) -> &str {
            "files"
        }
        fn priority(&self) -> i64 {
            crate::search::ranking::PRIORITY_FILES
        }
        fn query(&self, q: &str) -> Vec<SearchItem> {
            if q.is_empty() {
                return Vec::new();
            }
            vec![SearchItem::new(
                format!("files:{q}"),
                self.name(),
                q.to_string(),
                ItemAction::OpenPath { path: q.to_string() },
            )]
        }
    }

    #[test]
    fn provider_contract() {
        let p = Echo;
        assert_eq!(p.name(), "files");
        assert_eq!(p.priority(), crate::search::ranking::PRIORITY_FILES);
        assert!(p.query("").is_empty(), "пустой запрос провайдер молчит");
        let items = p.query("заметки");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "files:заметки");
        assert_eq!(items[0].provider, "files");
    }
}
