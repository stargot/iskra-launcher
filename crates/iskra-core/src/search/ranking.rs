//! Формула итогового ранжирования — ЕДИНСТВЕННАЯ точка весов (план Ф2, §1).
//!
//! `score = fuzzy(0..1000) + provider_priority + min(used_count,50)*2 + recency_boost`
//!
//! Приоритеты провайдеров: apps 100, calc 90 (при матче), settings 80,
//! system 70, files 50, web-fallback 10. Тюнить веса — только здесь.

use std::time::{Duration, SystemTime};

// --- приоритеты провайдеров ---
pub const PRIORITY_APPS: i64 = 100;
pub const PRIORITY_CALC: i64 = 90;
pub const PRIORITY_SETTINGS: i64 = 80;
pub const PRIORITY_SYSTEM: i64 = 70;
pub const PRIORITY_FILES: i64 = 50;
pub const PRIORITY_WEB: i64 = 10;

// --- вклад частоты использования ---
/// +2 за каждый запуск.
pub const USAGE_BONUS_PER_USE: i64 = 2;
/// Учёт не более 50 запусков (потолок вклада +100).
pub const USAGE_BONUS_MAX_USES: u32 = 50;

// --- вклад свежести (recency_boost) ---
const RECENCY_1H: Duration = Duration::from_secs(60 * 60);
const RECENCY_1D: Duration = Duration::from_secs(24 * 60 * 60);
const RECENCY_1W: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const RECENCY_BOOST_RECENT: i64 = 20; // < 1 часа
pub const RECENCY_BOOST_TODAY: i64 = 10; // < 1 суток
pub const RECENCY_BOOST_WEEK: i64 = 5; // < 1 недели
pub const RECENCY_BOOST_NONE: i64 = 0;

/// Вклад частоты: `min(used_count, 50) * 2`.
pub fn used_count_bonus(used_count: u32) -> i64 {
    used_count.min(USAGE_BONUS_MAX_USES) as i64 * USAGE_BONUS_PER_USE
}

/// Вклад свежести последнего запуска (если был). Отрицательный сдвиг часов
/// (переведённые часы) считаем «только что» — не штрафуем.
pub fn recency_boost(now: SystemTime, last_used: Option<SystemTime>) -> i64 {
    match last_used {
        None => RECENCY_BOOST_NONE,
        Some(last) => match now.duration_since(last) {
            Ok(age) if age < RECENCY_1H => RECENCY_BOOST_RECENT,
            Ok(age) if age < RECENCY_1D => RECENCY_BOOST_TODAY,
            Ok(age) if age < RECENCY_1W => RECENCY_BOOST_WEEK,
            Ok(_) => RECENCY_BOOST_NONE,
            Err(_) => RECENCY_BOOST_RECENT, // last_used «в будущем» — считаем свежим
        },
    }
}

/// Итоговый скор элемента (см. документацию модуля).
#[allow(clippy::too_many_arguments)]
pub fn total_score(
    fuzzy_score: i64,
    provider_priority: i64,
    used_count: u32,
    now: SystemTime,
    last_used: Option<SystemTime>,
) -> i64 {
    fuzzy_score + provider_priority + used_count_bonus(used_count) + recency_boost(now, last_used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs_ago: u64) -> (SystemTime, SystemTime) {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        (now, now - Duration::from_secs(secs_ago))
    }

    #[test]
    fn usage_bonus_caps_at_50() {
        assert_eq!(used_count_bonus(0), 0);
        assert_eq!(used_count_bonus(3), 6);
        assert_eq!(used_count_bonus(50), 100);
        assert_eq!(used_count_bonus(1000), 100, "потолок 50 запусков");
    }

    #[test]
    fn recency_tiers() {
        let (now, h) = at(30 * 60);
        assert_eq!(recency_boost(now, Some(h)), 20, "меньше часа");
        let (_, d) = at(5 * 60 * 60);
        assert_eq!(recency_boost(now, Some(d)), 10, "меньше суток");
        let (_, w) = at(3 * 24 * 60 * 60);
        assert_eq!(recency_boost(now, Some(w)), 5, "меньше недели");
        let (_, old) = at(30 * 24 * 60 * 60);
        assert_eq!(recency_boost(now, Some(old)), 0, "давно");
        assert_eq!(recency_boost(now, None), 0, "никогда не запускался");
        let (now2, _) = at(0);
        assert_eq!(recency_boost(now2, Some(now2 + RECENCY_1H)), 20, "будущее = свежее");
    }

    #[test]
    fn total_score_is_sum_of_parts() {
        let (now, last) = at(120); // recency 20
        // apps-элемент с идеальным fuzzy и 10 запусками:
        let s = total_score(1000, PRIORITY_APPS, 10, now, Some(last));
        assert_eq!(s, 1000 + 100 + 20 + 20);
        // web-fallback без usage:
        let s2 = total_score(500, PRIORITY_WEB, 0, now, None);
        assert_eq!(s2, 500 + 10);
        // при равном fuzzy приоритет расставляет порядок: apps (100) выше files (50)
        let apps_item = total_score(900, PRIORITY_APPS, 0, now, None);
        let files_item = total_score(900, PRIORITY_FILES, 0, now, None);
        assert_eq!(apps_item, 1000);
        assert_eq!(files_item, 950);
        assert!(apps_item > files_item);
    }
}
