//! Провайдер системных команд (дополнение к плану Ф2, шаг 4): lock, monitor-off,
//! sleep, restart, shutdown, empty-bin → `ItemAction::System(SystemCommand)`.
//! Каталог статический (RU + EN keywords), исполнение — iskra-sys/power.rs,
//! вызывается из app (шаг 4). Крейт остаётся без WinAPI: элемент — только данные.
//!
//! РИСК 9 (обязателен): провайдер матчится ТОЛЬКО при длине запроса ≥ 2 символов
//! — один случайный символ не должен блокировать станцию/выключать экран/сон.

use super::fuzzy;
use super::provider::SearchProvider;
use super::ranking;
use super::types::{ItemAction, SearchItem, SystemCommand};

/// Минимальная длина запроса для матчинга (риск 9). Один символ — молчание.
pub const MIN_QUERY_CHARS: usize = 2;

/// Максимум результатов за запрос (каталог сам по себе короткий).
pub const MAX_RESULTS: usize = 6;

/// Одна запись каталога системных команд.
struct SysEntry {
    /// Ключ элемента (part of id `system:{key}`).
    key: &'static str,
    /// Команда для ItemAction::System.
    command: SystemCommand,
    /// RU-заголовок (как покажем в UI).
    title: &'static str,
    /// EN-подзаголовок.
    subtitle: &'static str,
    /// Дополнительные ключевые слова RU/EN через пробел.
    keywords: &'static str,
}

static COMMANDS: [SysEntry; 6] = [
    SysEntry {
        key: "lock",
        command: SystemCommand::Lock,
        title: "Заблокировать компьютер",
        subtitle: "Lock workstation",
        keywords: "заблокировать блокировка lock win+l лок",
    },
    SysEntry {
        key: "monitor-off",
        command: SystemCommand::MonitorOff,
        title: "Выключить экран",
        subtitle: "Turn off display",
        keywords: "выключить экран монитор дисплей monitor off display",
    },
    SysEntry {
        key: "sleep",
        command: SystemCommand::Sleep,
        title: "Спящий режим",
        subtitle: "Sleep",
        keywords: "сон спящий режим уснуть sleep suspend",
    },
    SysEntry {
        key: "restart",
        command: SystemCommand::Restart,
        title: "Перезагрузка",
        subtitle: "Restart",
        keywords: "перезагрузка рестарт ребут restart reboot",
    },
    SysEntry {
        key: "shutdown",
        command: SystemCommand::Shutdown,
        title: "Завершение работы",
        subtitle: "Shut down",
        keywords: "завершение работы выключение выключить компьютер питание shutdown power off",
    },
    SysEntry {
        key: "empty-bin",
        command: SystemCommand::EmptyRecycleBin,
        title: "Очистить корзину",
        subtitle: "Empty Recycle Bin",
        keywords: "очистить корзину корзина мусор empty recycle bin trash",
    },
];

/// Лучший скор одного слова запроса против элемента: по ПОЛНЫМ полям
/// (заголовок/ключ/подзаголовок/ключевые слова) и по отдельным токенам
/// ключевых слов (короткое слово матчится префиксом длинного токена).
fn word_score(word: &str, e: &SysEntry) -> i64 {
    let mut best = fuzzy::score(word, e.title)
        .max(fuzzy::score(word, e.key))
        .max(fuzzy::score(word, e.subtitle))
        .max(fuzzy::score(word, e.keywords));
    for kw in e.keywords.split_whitespace() {
        best = best.max(fuzzy::score(word, kw));
    }
    best
}

/// Скор элемента под запрос: каждое слово запроса должно найтись (AND —
/// семантика FTS5); итог — минимум по словам (слабейшее звено), чтобы
/// «выключить компьютер» поднимало shutdown, а не monitor-off.
fn entry_score(q: &str, e: &SysEntry) -> i64 {
    let mut min = i64::MAX;
    for w in q.split_whitespace() {
        let s = word_score(w, e);
        if s < min {
            min = s;
        }
        if min < fuzzy::MIN_MEANINGFUL {
            return min; // слово не нашлось — элемент молчит
        }
    }
    min
}

/// Провайдер системных команд. Состояния нет — потокобезопасен по построению.
pub struct SystemProvider;

impl SearchProvider for SystemProvider {
    fn name(&self) -> &str {
        "system"
    }

    fn priority(&self) -> i64 {
        ranking::PRIORITY_SYSTEM
    }

    /// Риск 9: запрос короче MIN_QUERY_CHARS символов → провайдер молчит
    /// (в т.ч. пустой запрос — но агрегатор и так не вызывает провайдеров на "").
    fn query(&self, q: &str) -> Vec<SearchItem> {
        let q = q.trim();
        if q.chars().count() < MIN_QUERY_CHARS {
            return Vec::new();
        }
        let mut scored: Vec<(i64, &SysEntry)> = COMMANDS
            .iter()
            .map(|e| (entry_score(q, e), e))
            .filter(|(s, _)| *s >= fuzzy::MIN_MEANINGFUL)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(s, e)| SearchItem {
                id: format!("system:{}", e.key),
                provider: self.name().to_string(),
                title: e.title.to_string(),
                subtitle: Some(e.subtitle.to_string()),
                icon_path: None,
                score: s,
                action: ItemAction::System { command: e.command },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(q: &str) -> Vec<String> {
        SystemProvider.query(q).into_iter().map(|i| i.id).collect()
    }

    /// РИСК 9: один символ (и пустой запрос) — строгое молчание, никаких команд.
    #[test]
    fn single_char_query_never_matches() {
        for q in ["", "л", "с", "s", "ы"] {
            assert!(ids(q).is_empty(), "запрос {q:?} должен молчать (риск 9)");
        }
    }

    /// Два символа уже матчатся, если совпадение осмысленное («со» → «сон»).
    #[test]
    fn two_char_query_can_match() {
        assert_eq!(ids("сон").first().map(String::as_str), Some("system:sleep"));
    }

    /// RU-запросы приёмки: топ-результат — ожидаемая команда.
    #[test]
    fn russian_queries() {
        let top = |q: &str| ids(q).first().map(String::as_str).map(str::to_owned);
        assert_eq!(top("сон").as_deref(), Some("system:sleep"));
        assert_eq!(top("спящий").as_deref(), Some("system:sleep"));
        assert_eq!(top("перезагрузка").as_deref(), Some("system:restart"));
        assert_eq!(top("завершение работы").as_deref(), Some("system:shutdown"));
        assert_eq!(top("корзина").as_deref(), Some("system:empty-bin"));
        assert_eq!(top("заблокировать").as_deref(), Some("system:lock"));
    }

    /// «выключить …» разводит близкие команды: «выключить экран» → monitor-off,
    /// «выключить компьютер» → shutdown (ключевые слова не пересекаются по объекту).
    #[test]
    fn turn_off_display_vs_shut_down() {
        assert_eq!(
            ids("выключить экран").first().map(String::as_str),
            Some("system:monitor-off")
        );
        assert_eq!(
            ids("выключить компьютер").first().map(String::as_str),
            Some("system:shutdown")
        );
    }

    /// EN-запросы и форма результата: провайдер/system, действие System,
    /// сортировка по убыванию скора, лимит.
    #[test]
    fn english_queries_and_result_shape() {
        let items = SystemProvider.query("reboot");
        assert_eq!(items.first().map(|i| i.id.as_str()), Some("system:restart"));

        let items = SystemProvider.query("trash");
        assert_eq!(items.first().map(|i| i.id.as_str()), Some("system:empty-bin"));

        let items = SystemProvider.query("сон");
        assert!(!items.is_empty() && items.len() <= MAX_RESULTS);
        let first = &items[0];
        assert_eq!(first.provider, "system");
        assert_eq!(
            first.action,
            ItemAction::System { command: SystemCommand::Sleep }
        );
        assert!(first.subtitle.is_some(), "EN-подзаголовок присутствует");
        assert!(first.score >= fuzzy::MIN_MEANINGFUL);
        for w in items.windows(2) {
            assert!(w[0].score >= w[1].score, "отсортировано по убыванию скора");
        }
    }

    /// Контракт трейта: имя совпадает с константой приоритета ranking::PRIORITY_SYSTEM.
    #[test]
    fn provider_contract() {
        let p = SystemProvider;
        assert_eq!(p.name(), "system");
        assert_eq!(p.priority(), ranking::PRIORITY_SYSTEM);
    }
}
