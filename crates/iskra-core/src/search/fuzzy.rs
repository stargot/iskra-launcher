//! Fuzzy-скоринг «запрос → строка» в духе fzf: шкала 0..1000 (план Ф2, шаг 1).
//!
//! Порядок приоритетов (§1 плана): точное совпадение > начало строки (префикс) >
//! подстрока на границе слов > подстрока на camelCase-горбе > подстрока в середине
//! слова > подпоследовательность. Всё регистронезависимо, ё ≡ е (риск 8: та же
//! нормализация будет при записи в FTS5 на шаге 2).
//!
//! Все функции чистые и детерминированные — корпус-тесты рядом (≥ 20 пар RU/EN).

/// Максимальный (идеальный) скор — точное совпадение.
pub const MAX_SCORE: i64 = 1000;

/// Нижняя граница «осмысленного» совпадения: ниже — провайдер молчит.
/// Подпоследовательность с приличными границами слов даёт ~450+, мусор отсекается.
pub const MIN_MEANINGFUL: i64 = 400;

/// Нормализация одного символа: lowercase (первый символ — карта строго 1:1,
/// чтобы позиции нормализованной строки совпадали с исходной) + ё → е.
fn normalize_char(c: char) -> char {
    let lo = c.to_lowercase().next().unwrap_or(c);
    if lo == 'ё' { 'е' } else { lo }
}

/// Нормализация строки в вектор символов (1:1 с исходными позициями).
fn normalize_chars(s: &str) -> Vec<char> {
    s.chars().map(normalize_char).collect()
}

/// Граница слова в позиции `idx` исходной строки:
/// начало строки, предыдущий символ — не буква/цифра, либо camelCase-горб
/// (строчная → заглавная). `orig` — символьный вектор ИСХОДНОЙ строки.
fn is_boundary(orig: &[char], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    let (prev, cur) = (orig[idx - 1], orig[idx]);
    if !prev.is_alphanumeric() {
        return true; // после пробела, точки, дефиса, скобки...
    }
    prev.is_lowercase() && cur.is_uppercase() // camelCase горб
}

/// Поиск подстроки `needle` в `haystack` (по векторам символов).
fn find_sub(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
}

/// Скор подпоследовательности (жадное сопоставление слева направо, как fzf v1):
/// база 450, бонус за границы слов, штраф за пропуски; итог обрезается до 600,
/// чтобы подпоследовательность никогда не обгоняла подстроку (630..650).
fn subsequence_score(q: &[char], c: &[char], c_orig: &[char]) -> i64 {
    if q.len() > c.len() {
        return 0;
    }
    let mut positions = Vec::with_capacity(q.len());
    let mut search_from = 0;
    for &qc in q {
        let found = (search_from..c.len()).find(|&i| c[i] == qc);
        match found {
            Some(i) => {
                positions.push(i);
                search_from = i + 1;
            }
            None => return 0, // не подпоследовательность
        }
    }
    let mut score: i64 = 450;
    let mut gaps: i64 = 0;
    for (k, &pos) in positions.iter().enumerate() {
        if is_boundary(c_orig, pos) {
            score += if k == 0 {
                if pos == 0 || !c_orig[pos - 1].is_alphanumeric() { 80 } else { 60 } // горб дешевле старта
            } else if !c_orig[pos - 1].is_alphanumeric() || (c_orig[pos - 1].is_lowercase() && c_orig[pos].is_uppercase()) {
                25
            } else {
                0
            };
        }
        if k > 0 {
            gaps += (pos - positions[k - 1] - 1) as i64;
        }
    }
    score -= (gaps * 2).min(160);
    score.clamp(0, 600)
}

/// Скор соответствия запроса кандидату, 0..1000 (см. модульную документацию).
pub fn score(query: &str, candidate: &str) -> i64 {
    let q = normalize_chars(query);
    if q.is_empty() {
        return 0;
    }
    let c = normalize_chars(candidate);
    if q == c {
        return MAX_SCORE;
    }
    let c_orig: Vec<char> = candidate.chars().collect();
    let len_penalty = ((c.len().saturating_sub(q.len())) as i64).min(150);

    // 1) Подстрока (включая префикс): тир по типу позиции совпадения.
    if let Some(pos) = find_sub(&c, &q) {
        if pos == 0 {
            return 950 - len_penalty / 3; // префикс: 900..950
        }
        let orig_prev = c_orig[pos - 1];
        if !orig_prev.is_alphanumeric() {
            return 800 - len_penalty / 3; // граница слов (после -_. ( и т.п.): 750..800
        }
        if orig_prev.is_lowercase() && c_orig[pos].is_uppercase() {
            return 750 - len_penalty / 3; // camelCase-горб: 700..750
        }
        return 650 - ((c.len() - q.len()) as i64).min(100) / 5; // середина слова: 630..650
    }

    // 2) Подпоследовательность (максимум 600 — ниже любой подстроки).
    subsequence_score(&q, &c, &c_orig)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Абсолютные значения тиров: точное/регистр/ё-фолдинг/префикс/подстрока/подпоследовательность.
    #[test]
    fn absolute_tiers() {
        assert_eq!(score("хром", "Хром"), MAX_SCORE);
        assert_eq!(score("ХРОМ", "хроМ"), MAX_SCORE, "регистр не важен");
        assert_eq!(score("Елка", "Ёлка"), MAX_SCORE, "ё ≡ е (риск 8)");
        assert_eq!(score("2+2", "2+2"), MAX_SCORE, "не только буквы");

        let prefix = score("хро", "Хром");
        assert!((850..=950).contains(&prefix), "префикс 900..950, получено {prefix}");

        let plain = score("ead", "README.md");
        assert!((600..700).contains(&plain), "подстрока в слове 630..650, получено {plain}");

        let boundary = score("chrome", "My Chrome Browser");
        assert!((700..850).contains(&boundary), "граница слов 750..800, получено {boundary}");

        let subseq = score("хрм", "Хром");
        assert!((450..=600).contains(&subseq), "подпоследовательность ≤600, получено {subseq}");

        assert_eq!(score("хрм", "SQLite"), 0, "нет совпадения");
        assert_eq!(score("", "Хром"), 0, "пустой запрос");
        assert_eq!(score("хрм", ""), 0, "пустой кандидат");
        assert_eq!(score("мсто", "молоко"), 0, "не подпоследовательность");
    }

    /// Корпус ≥ 20 пар «запрос → порядок кандидатов»: candidates[0] обязан быть
    /// строже candidates[1]. RU и EN, включая кейс «хрм» → «Хром».
    #[test]
    fn corpus_order_pairs() {
        let rows: &[(&str, &str, &str)] = &[
            // --- RU ---
            ("хром", "Хром", "Термометр"),
            ("хром", "Хром", "Хруст"),            // нет «о» → 0
            ("хрм", "Хром", "Термометр"),         // подпоследовательность vs нет
            ("хрм", "Хром", "Хроники"),           // нет «м» → 0
            ("хрм", "Хром", "Хруст"),
            ("настр", "Настройки", "Настройка экрана"), // оба префикс, короче — выше
            ("звук", "Звук", "Проверка звука"),   // точное vs граница слов
            ("принт", "Принтеры и сканеры", "Принадлежности"), // префикс vs подпоследовательность
            ("свет", "Свет", "Переключить свет"),
            ("экран", "Экран", "Screen"),
            ("мили", "Мили", "Миллион"),          // точное vs подпоследовательность
            ("ноут", "Ноутбук", "Кнопка"),        // «у» нет во втором
            ("впн", "ВПН", "ВпнНастройки"),       // точное (регистр) vs префикс
            ("калькул", "Калькулятор", "Калькулировать"), // префикс: короче — выше
            // --- EN ---
            ("chrom", "Chromium", "Not chrome"),  // префикс vs граница слов
            ("chrom", "chrome.dll", "Not chrome"),
            ("hub", "git hub", "GitHub"),         // граница слов vs подстрока в слове
            ("gc", "GetContent", "SqlConfig"),    // camelCase-горб vs нет «gc» (жадный жребий)
            ("upd", "Update.exe", "Chrome Update"), // префикс vs граница слов
            ("wifi", "wifi-direct", "Wi-Fi"),     // префикс vs подпоследовательность через дефис
            ("display", "Display settings", "Nvidia Display Driver"),
            ("yt", "YouTube", "Tubort"),          // «y» есть только в первом
            ("doc", "document.docx", "My Documents"),
            ("power", "Power & sleep", "Empower"), // префикс vs подстрока в слове
        ];
        assert!(rows.len() >= 20, "корпус обязан содержать ≥ 20 пар, сейчас {}", rows.len());
        for &(q, best, worse) in rows {
            let (b, w) = (score(q, best), score(q, worse));
            assert!(b > w, "запрос {q:?}: {best:?} ({b}) обязан быть выше {worse:?} ({w})");
        }
    }

    /// camelCase-горб дороже обычной подстроки, но дешевле границы слов.
    #[test]
    fn camel_case_tier_between_boundary_and_plain() {
        let hump = score("co", "GetContent"); // горб: 750 - pen/3 → 748
        let plain = score("co", "Scoop");     // середина слова: 650 - pen/5
        let boundary = score("co", "my Config"); // после пробела
        assert!(hump > plain, "горб {hump} > подстрока {plain}");
        assert!(boundary > hump, "граница {boundary} > горб {hump}");
        assert!((700..750).contains(&hump));
    }

    /// Скор симметричен по регистру и стабилен (повторный вызов — то же значение).
    #[test]
    fn deterministic_and_case_insensitive() {
        assert_eq!(score("WIN", "windows update"), score("win", "Windows Update"));
        assert_eq!(score("хрм", "Хром"), score("ХРМ", "хром"));
    }
}
