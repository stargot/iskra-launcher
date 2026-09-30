//! Агрегатор результатов (план Ф2, шаг 1): сбор со всех провайдеров, итоговый
//! скор через `ranking::total_score` (единственная точка весов), dedupe по
//! (provider, title), лимит 50, реестр id для `run_item`, пустой запрос → recents
//! из usage-хранилища (до шага 2 — трейт `UsageStore`, БД подключит db.rs).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use super::provider::SearchProvider;
use super::ranking;
use super::types::SearchItem;

/// Максимум элементов в выдаче (UI считает, что больше не придёт).
pub const RESULT_LIMIT: usize = 50;

/// Сколько recents запрашивать у usage-хранилища на пустой запрос.
pub const RECENTS_LIMIT: usize = 20;

/// Доступ к статистике использования (usage-таблица БД на шаге 2; в тестах —
/// in-memory фейки). Отделён от Aggregator, чтобы ядро осталось без БД.
pub trait UsageStore: Send + Sync {
    /// Сколько раз элемент запускали.
    fn used_count(&self, id: &str) -> u32;
    /// Когда запускали последний раз.
    fn last_used_at(&self, id: &str) -> Option<SystemTime>;
    /// Кандидаты на показ при пустом запросе (порядок не важен — агрегатор
    /// сам отранжирует по usage).
    fn recents(&self, limit: usize) -> Vec<SearchItem>;
}

/// Заглушка usage: ничего не запускалось (до подключения БД и в тестах).
pub struct NoUsage;

impl UsageStore for NoUsage {
    fn used_count(&self, _id: &str) -> u32 {
        0
    }
    fn last_used_at(&self, _id: &str) -> Option<SystemTime> {
        None
    }
    fn recents(&self, _limit: usize) -> Vec<SearchItem> {
        Vec::new()
    }
}

/// Агрегатор: владеет списком провайдеров и реестром результатов последнего
/// запроса (id → элемент) для исполнения `run_item(id)` без повторного поиска.
pub struct Aggregator {
    providers: Vec<Arc<dyn SearchProvider>>,
    registry: Mutex<HashMap<String, SearchItem>>,
}

impl Aggregator {
    /// Новый агрегатор с фиксированным набором провайдеров.
    pub fn new(providers: Vec<Arc<dyn SearchProvider>>) -> Self {
        Aggregator { providers, registry: Mutex::new(HashMap::new()) }
    }

    /// Полный конвейер: провайдеры/recents → итоговый скор → dedupe → сортировка
    /// → лимит 50 → реестр. Возвращает готовую выдачу.
    pub fn query(&self, q: &str, usage: &dyn UsageStore) -> Vec<SearchItem> {
        let now = SystemTime::now();
        let empty_q = q.trim().is_empty();

        // 1. Сбор сырья: пустой запрос → recents; иначе — все провайдеры.
        let mut raw: Vec<SearchItem> = if empty_q {
            usage.recents(RECENTS_LIMIT)
        } else {
            self.providers.iter().flat_map(|p| p.query(q)).collect()
        };

        // 2. Итоговый скор: fuzzy (уже в item.score) + приоритет + usage.
        let priorities: HashMap<&str, i64> = self
            .providers
            .iter()
            .map(|p| (p.name(), p.priority()))
            .collect();
        for item in raw.iter_mut() {
            let priority = priorities.get(item.provider.as_str()).copied().unwrap_or(0);
            item.score = ranking::total_score(
                item.score,
                priority,
                usage.used_count(&item.id),
                now,
                usage.last_used_at(&item.id),
            );
        }

        // 3. Dedupe по (provider, id) — страховка от дублей приложений с одним
        //    target (id = apps:{target.lowercase()}); разные файлы с одинаковым
        //    именем (разные пути/id) не схлопываются. Оставляем максимальный скор.
        let mut deduped: HashMap<(String, String), SearchItem> = HashMap::new();
        for item in raw {
            let key = (item.provider.clone(), item.id.clone());
            match deduped.get_mut(&key) {
                Some(existing) if existing.score >= item.score => {}
                _ => {
                    deduped.insert(key, item);
                }
            }
        }

        // 4. Сортировка по итоговому скору (тай-брейк — заголовок) и лимит.
        let mut items: Vec<SearchItem> = deduped.into_values().collect();
        items.sort_by(|a, b| b.score.cmp(&a.score).then(a.title.cmp(&b.title)));
        items.truncate(RESULT_LIMIT);

        // 5. Реестр id для run_item (последняя выдача побеждает).
        if let Ok(mut reg) = self.registry.lock() {
            for item in &items {
                reg.insert(item.id.clone(), item.clone());
            }
        }
        items
    }

    /// Достать элемент по id из реестра последней выдачи (для `run_item`).
    pub fn item(&self, id: &str) -> Option<SearchItem> {
        self.registry.lock().ok()?.get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::fuzzy;
    use crate::search::types::ItemAction;
    use std::time::Duration;

    /// Простой провайдер поверх вектора заголовков — скоpит fuzzy, как будет
    /// работать файловый провайдер шага 2. Используется и в bench ниже.
    pub(crate) struct VecProvider {
        name: &'static str,
        priority: i64,
        titles: Vec<String>,
        take: usize,
    }

    impl VecProvider {
        pub(crate) fn new(name: &'static str, priority: i64, titles: Vec<String>) -> Self {
            VecProvider { name, priority, titles, take: 20 }
        }

        pub(crate) fn with_take(mut self, take: usize) -> Self {
            self.take = take;
            self
        }
    }

    impl SearchProvider for VecProvider {
        fn name(&self) -> &str {
            self.name
        }
        fn priority(&self) -> i64 {
            self.priority
        }
        fn query(&self, q: &str) -> Vec<SearchItem> {
            let mut scored: Vec<(i64, usize)> = self
                .titles
                .iter()
                .enumerate()
                .map(|(i, t)| (fuzzy::score(q, t), i))
                .filter(|(s, _)| *s > 0)
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            scored
                .into_iter()
                .take(self.take)
                .map(|(s, i)| SearchItem {
                    id: format!("{}:{}", self.name, i),
                    provider: self.name.to_string(),
                    title: self.titles[i].clone(),
                    subtitle: None,
                    icon_path: None,
                    score: s,
                    action: ItemAction::OpenPath { path: self.titles[i].clone() },
                })
                .collect()
        }
    }

    /// In-memory usage для тестов recents/usage-бонусов.
    struct FakeUsage {
        counts: HashMap<String, u32>,
        items: Vec<SearchItem>,
    }

    impl FakeUsage {
        fn new(items: Vec<SearchItem>) -> Self {
            FakeUsage { counts: HashMap::new(), items }
        }
    }

    impl UsageStore for FakeUsage {
        fn used_count(&self, id: &str) -> u32 {
            self.counts.get(id).copied().unwrap_or(0)
        }
        fn last_used_at(&self, id: &str) -> Option<SystemTime> {
            let n = self.used_count(id);
            if n == 0 {
                None
            } else {
                Some(SystemTime::now() - Duration::from_secs(n as u64 * 60))
            }
        }
        fn recents(&self, limit: usize) -> Vec<SearchItem> {
            self.items.iter().take(limit).cloned().collect()
        }
    }

    fn app_item(id: &str, title: &str) -> SearchItem {
        SearchItem::new(id, "apps", title, ItemAction::OpenPath { path: title.to_string() })
    }

    /// Пустой запрос → recents, отранжированные usage-бонусом (fuzzy = 0).
    #[test]
    fn empty_query_returns_recents_ranked_by_usage() {
        let mut usage = FakeUsage::new(vec![
            app_item("apps:rare", "Редкая"),
            app_item("apps:often", "Частая"),
        ]);
        usage.counts.insert("apps:often".to_string(), 10);
        usage.counts.insert("apps:rare".to_string(), 1);

        let agg = Aggregator::new(vec![]);
        let items = agg.query("", &usage);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "apps:often", "больше запусков — выше");
        // скор частой = 0 (fuzzy) + 100 (apps) + 10*2 (usage) + recency
        assert!(items[0].score > items[1].score);
    }

    /// Итоговый скор = fuzzy + приоритет провайдера (+usage): при одинаковых
    /// заголовках разница скоров ровно равна разнице приоритетов.
    #[test]
    fn final_score_includes_provider_priority() {
        let apps = VecProvider::new("apps", ranking::PRIORITY_APPS, vec!["Хром".to_string()]);
        let files = VecProvider::new("files", ranking::PRIORITY_FILES, vec!["Хром".to_string()]);
        let agg = Aggregator::new(vec![Arc::new(apps), Arc::new(files)]);
        let items = agg.query("хром", &NoUsage);
        assert_eq!(items.len(), 2, "разные провайдеры не дедуплицируются");
        // у обоих fuzzy совпадает (точное 1000), но apps (100) выше files (50)
        assert_eq!(items[0].provider, "apps");
        assert_eq!(items[1].provider, "files");
        assert_eq!(items[0].score - items[1].score, ranking::PRIORITY_APPS - ranking::PRIORITY_FILES);
    }

    /// Dedupe по (provider, id): дубль цели схлопывается (максимальный скор),
    /// а разные элементы с одинаковым заголовком (одинаковые имена файлов в
    /// разных каталогах) НЕ теряются.
    #[test]
    fn dedupes_by_provider_and_id() {
        // Одинаковый (provider, id), разные заголовки → одна строка (max скор).
        struct SameId;
        impl SearchProvider for SameId {
            fn name(&self) -> &str {
                "apps"
            }
            fn priority(&self) -> i64 {
                ranking::PRIORITY_APPS
            }
            fn query(&self, _q: &str) -> Vec<SearchItem> {
                vec![
                    SearchItem::new(
                        "apps:hwinfo64.exe",
                        "apps",
                        "HWiNFO64",
                        ItemAction::OpenPath { path: "hwinfo64.exe".into() },
                    ),
                    SearchItem::new(
                        "apps:hwinfo64.exe",
                        "apps",
                        "HWiNFO64 (2)",
                        ItemAction::OpenPath { path: "hwinfo64.exe".into() },
                    ),
                ]
            }
        }
        // Одинаковый заголовок, разные id (одинаково названные файлы в разных
        // каталогах) → ОБЕ строки сохраняются.
        struct SameTitle;
        impl SearchProvider for SameTitle {
            fn name(&self) -> &str {
                "files"
            }
            fn priority(&self) -> i64 {
                ranking::PRIORITY_FILES
            }
            fn query(&self, _q: &str) -> Vec<SearchItem> {
                vec![
                    SearchItem::new(
                        "files:c:\\a\\otchet.pdf",
                        "files",
                        "otchet.pdf",
                        ItemAction::OpenPath { path: "c:\\a\\otchet.pdf".into() },
                    ),
                    SearchItem::new(
                        "files:c:\\b\\otchet.pdf",
                        "files",
                        "otchet.pdf",
                        ItemAction::OpenPath { path: "c:\\b\\otchet.pdf".into() },
                    ),
                ]
            }
        }

        let agg = Aggregator::new(vec![Arc::new(SameId), Arc::new(SameTitle)]);
        let items = agg.query("ot", &NoUsage);
        let apps: Vec<_> = items.iter().filter(|i| i.provider == "apps").collect();
        let files: Vec<_> = items.iter().filter(|i| i.provider == "files").collect();
        assert_eq!(apps.len(), 1, "дубль (provider,id) схлопнут");
        assert_eq!(apps[0].title, "HWiNFO64", "оставлен максимальный скор (первый)");
        assert_eq!(files.len(), 2, "разные id с одним заголовком не теряются");
    }

    /// Лимит выдачи 50 и реестр id для run_item.
    #[test]
    fn result_limit_and_registry() {
        let titles: Vec<String> = (0..10_000).map(|i| format!("документ{i}.txt")).collect();
        let p = VecProvider::new("files", ranking::PRIORITY_FILES, titles).with_take(60);
        let agg = Aggregator::new(vec![Arc::new(p)]);
        let items = agg.query("документ", &NoUsage);
        assert_eq!(items.len(), RESULT_LIMIT, "выдача обрезается до 50");

        let first_id = items[0].id.clone();
        let from_registry = agg.item(&first_id).expect("id из выдачи есть в реестре");
        assert_eq!(from_registry.id, first_id);
        assert!(agg.item("files:нет-такого").is_none());
    }

    /// Приоритет провайдера без совпадений не создаёт элементов; агрегатор
    /// не падает без провайдеров.
    #[test]
    fn empty_aggregator_and_no_matches() {
        let agg = Aggregator::new(vec![]);
        assert!(agg.query("хром", &NoUsage).is_empty());
        assert!(agg.query("", &NoUsage).is_empty(), "NoUsage: recents пусты");
    }

    /// BENCH (план шага 1): 10 000 записей, p95 < 20 мс. Запускать в release:
    /// `cargo test -r -p iskra-core -- --ignored` (nearest-rank по 200 запросам).
    #[test]
    #[ignore = "bench: только в release (cargo test -r -- --ignored)"]
    fn bench_10k_items_p95_under_20ms() {
        // --- генерация детерминированного корпуса 10k (RU/EN вперемешку) ---
        let ru = ["доклад", "отчёт", "заметки", "резюме", "книга", "обои", "фото", "инструкция", "договор", "проект"];
        let en = ["report", "config", "image", "setup", "backup", "screenshot", "music", "download", "readme", "notes"];
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        let titles: Vec<String> = (0..10_000)
            .map(|i| {
                let base = if next() % 2 == 0 { ru[next() % ru.len()] } else { en[next() % en.len()] };
                let ext = [".txt", ".pdf", ".docx", ".png", ".lnk"][next() % 5];
                format!("{base}_{i}{ext}")
            })
            .collect();
        assert_eq!(titles.len(), 10_000);

        // --- один файловый провайдер поверх 10k записей + calc/settings/web ---
        let files = VecProvider::new("files", ranking::PRIORITY_FILES, titles).with_take(30);
        let agg = Aggregator::new(vec![
            Arc::new(files),
            Arc::new(crate::search::calc::CalcProvider),
            Arc::new(crate::search::settings_win::SettingsProvider),
            Arc::new(crate::search::web::WebProvider::default()),
        ]);

        let queries = [
            "доклад", "отчет", "img", "config", "хрм", "книга", "screenshot",
            "резюме", "обои", "setup", "заметки", "download", "архив", "проект",
            "music", "фото 2025", "backup", "инструкц", "json", "2+2*2",
        ];

        let mut latencies_ms: Vec<u128> = Vec::with_capacity(200);
        for i in 0..200 {
            let q = queries[i % queries.len()];
            let t0 = std::time::Instant::now();
            let items = agg.query(q, &NoUsage);
            let dt = t0.elapsed();
            assert!(!items.is_empty(), "запрос {q} обязан давать результаты");
            latencies_ms.push(dt.as_millis());
        }
        latencies_ms.sort_unstable();

        // p95 по nearest-rank: ceil(0.95 * n)-й элемент
        let idx = ((0.95 * latencies_ms.len() as f64).ceil() as usize).saturating_sub(1);
        let p50 = latencies_ms[latencies_ms.len() / 2];
        let p95 = latencies_ms[idx];
        println!(
            "bench 10k записей: n={} запросов, p50={} мс, p95={} мс (лимит 20)",
            latencies_ms.len(),
            p50,
            p95
        );
        assert!(p95 < 20, "p95 = {} мс ≥ 20 мс", p95);
    }
}
