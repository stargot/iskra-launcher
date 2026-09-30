//! Калькулятор/конвертер единиц (план Ф2, шаг 1; решение D5).
//!
//! Два механизма:
//! 1. выражения — `meval` (`2+2*2`, `sqrt(2)`, `sin(pi/2)`);
//! 2. конвертер единиц — собственный парсер шаблона `N unit in|to|в unit`
//!    поверх таблицы коэффициентов (длина, масса, температура, объём, скорость,
//!    данные, время). Валют — НЕТ (без сети, по спеке).
//!
//! Детект запроса: начинается с цифры/скобки/знака и валиден — иначе провайдер
//! молчит. Единицы и их RU-алиасы: см. `UnitTable`. «ms» — миллисекунды; метры
//! в секунду пишутся `m/s`/`м/с` (снимает конфликт с ms). Данные — десятичные
//! (1 kb = 1000 b); двоичные kiB не поддержаны (нужны будут — добавить строку
//! в таблицу).

use serde::{Deserialize, Serialize};

use super::fuzzy;
use super::provider::SearchProvider;
use super::ranking;
use super::types::{ItemAction, SearchItem};

/// Категория единиц: конвертация возможна только внутри категории.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Length,
    Mass,
    Temperature,
    Volume,
    Speed,
    Data,
    Time,
}

/// Единица измерения: алиасы (lowercase) и коэффициент к базовой единице категории.
struct UnitDef {
    category: Category,
    /// Множитель к базе: м, кг, л, м/с, байт, секунда.
    factor: f64,
}

/// Таблица единиц: алиас → определение. Единственная точка правок по единицам.
fn unit_table(alias: &str) -> Option<UnitDef> {
    use Category::*;
    let (category, factor) = match alias {
        // длина (база — метр)
        "mm" | "мм" => (Length, 0.001),
        "cm" | "см" => (Length, 0.01),
        "m" | "м" => (Length, 1.0),
        "km" | "км" => (Length, 1000.0),
        "in" | "inch" => (Length, 0.0254),
        "ft" | "foot" | "feet" => (Length, 0.3048),
        "yd" | "yard" => (Length, 0.9144),
        "mi" | "mile" => (Length, 1609.344),
        // масса (база — килограмм)
        "mg" | "мг" => (Mass, 1.0e-6),
        "g" | "г" => (Mass, 0.001),
        "kg" | "кг" => (Mass, 1.0),
        "t" | "т" | "ton" => (Mass, 1000.0),
        "lb" | "lbs" | "pound" => (Mass, 0.45359237),
        "oz" => (Mass, 0.028349523125),
        // объём (база — литр)
        "ml" | "мл" => (Volume, 0.001),
        "l" | "л" => (Volume, 1.0),
        "gal" | "gallon" => (Volume, 3.785411784),
        "cup" => (Volume, 0.2365882365),
        // скорость (база — м/с)
        "kmh" | "km/h" | "км/ч" | "км/час" => (Speed, 1.0 / 3.6),
        "mph" => (Speed, 0.44704),
        "m/s" | "м/с" => (Speed, 1.0),
        "kn" | "knot" => (Speed, 0.514444),
        // данные (база — байт, десятичные)
        "b" | "б" | "byte" => (Data, 1.0),
        "kb" | "кб" => (Data, 1.0e3),
        "mb" | "мб" => (Data, 1.0e6),
        "gb" | "гб" => (Data, 1.0e9),
        "tb" | "тб" => (Data, 1.0e12),
        // время (база — секунда); «ms» — миллисекунды
        "ms" | "мс" => (Time, 0.001),
        "s" | "с" | "сек" | "sec" => (Time, 1.0),
        "min" | "мин" => (Time, 60.0),
        "h" | "ч" | "час" => (Time, 3600.0),
        "d" | "дн" | "day" | "сут" => (Time, 86400.0),
        "week" | "нед" => (Time, 604800.0),
        // температура — особая обработка, алиасы только маркируем
        "c" | "°c" | "цельсия" => (Temperature, 0.0),
        "f" | "°f" | "фаренгейт" => (Temperature, 0.0),
        "k" | "кельвин" => (Temperature, 0.0),
        _ => return None,
    };
    Some(UnitDef { category, factor })
}

/// Успешная конвертация единиц: сырой текст числа, единицы и результат.
#[derive(Debug, Clone, PartialEq)]
pub struct UnitConversion {
    /// Число как его написал пользователь (для показа в заголовке).
    pub value_text: String,
    pub from: String,
    pub to: String,
    pub result: f64,
}

/// Запрос похож на математику: начинается с цифры/скобки/знака и содержит цифру.
fn looks_like_math(q: &str) -> bool {
    let t = q.trim_start();
    let first = match t.chars().next() {
        Some(c) => c,
        None => return false,
    };
    if !(first.is_ascii_digit() || first == '(' || first == '-' || first == '+' || first == '.') {
        return false;
    }
    t.chars().any(|c| c.is_ascii_digit())
}

/// Разбор «числа» с запятой-десятичным разделителем (3,14 → 3.14).
fn parse_number(s: &str) -> Option<f64> {
    let normalized = if s.contains(',') && !s.contains('.') {
        s.replace(',', ".")
    } else {
        s.to_string()
    };
    normalized.trim().parse::<f64>().ok()
}

/// Разделить токен «10km» на (число, единица). Токен из одних цифр → None.
fn split_value_unit(token: &str) -> Option<(&str, &str)> {
    let bytes = token.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b',') {
        i += 1;
    }
    if i == 0 || i >= bytes.len() {
        return None; // нет числа или нет единицы
    }
    Some((&token[..i], &token[i..]))
}

/// Температура → Цельсий.
fn to_celsius(category: Category, value: f64, unit: &str) -> f64 {
    match unit {
        "f" | "°f" | "фаренгейт" => (value - 32.0) * 5.0 / 9.0,
        "k" | "кельвин" => value - 273.15,
        _ if category == Category::Temperature => value,
        _ => value,
    }
}

/// Цельсий → целевая температура.
fn from_celsius(celsius: f64, unit: &str) -> f64 {
    match unit {
        "f" | "°f" | "фаренгейт" => celsius * 9.0 / 5.0 + 32.0,
        "k" | "кельвин" => celsius + 273.15,
        _ => celsius,
    }
}

/// Конвертация единиц: `10 km in mi`, `72 f to c`, `1,5 кг в г`, `10km in mi`.
/// Разделитель — отдельное слово `in` | `to` | `в` (первый встретившийся).
pub fn convert_units(q: &str) -> Option<UnitConversion> {
    let tokens: Vec<&str> = q.split_whitespace().collect();
    let sep_pos = tokens.iter().position(|t| {
        *t == "in" || *t == "to" || *t == "IN" || *t == "TO" || *t == "в" || *t == "В"
    })?;
    if sep_pos == 0 || sep_pos > 2 || sep_pos + 1 >= tokens.len() {
        return None; // нужно: [число[+единица]] [единица] [in|to|в] [единица]
    }
    let to_unit_raw = tokens[sep_pos + 1];

    // «10 km ...» — число и единица отдельными токенами (sep на позиции 2);
    // «10km ...» — слитно (sep на позиции 1).
    let (value_text, from_unit_raw) = match sep_pos {
        1 => {
            let (v, u) = split_value_unit(tokens[0])?;
            (v.to_string(), u)
        }
        2 => (tokens[0].to_string(), tokens[1]),
        _ => return None,
    };

    let value = parse_number(&value_text)?;
    let from_unit = from_unit_raw.trim_start_matches('°').to_lowercase();
    let to_unit = to_unit_raw.trim_start_matches('°').to_lowercase();
    if from_unit.is_empty() || to_unit.is_empty() {
        return None;
    }

    let from = unit_table(&from_unit)?;
    let to = unit_table(&to_unit)?;
    if from.category != to.category {
        return None; // км в килограммы не конвертируем
    }

    let result = if from.category == Category::Temperature {
        from_celsius(to_celsius(from.category, value, &from_unit), &to_unit)
    } else {
        value * from.factor / to.factor
    };
    if !result.is_finite() {
        return None;
    }
    Some(UnitConversion {
        value_text,
        from: from_unit,
        to: to_unit,
        result,
    })
}

/// Вычислить выражение через meval; NaN/inf → None (провайдер молчит).
pub fn evaluate_expression(expr: &str) -> Option<f64> {
    meval::eval_str(expr).ok().filter(|v| v.is_finite())
}

/// Человекочитаемый формат числа: целые без дробной части, дробные — до 6 знаков
/// с обрезкой хвостовых нулей.
pub fn format_result(v: f64) -> String {
    if (v - v.round()).abs() < 1.0e-9 && v.abs() < 1.0e15 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_string()
    }
}

/// Результат вычисления для показа: `display` — строка для заголовка элемента,
/// `copy` — что положить в буфер по Enter, `expression` — исходный ввод.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalcAnswer {
    pub display: String,
    pub copy: String,
    pub expression: String,
}

/// Считать запрос (конвертация приоритетнее выражения): `10 km in mi`,
/// `2+2*2`, `sqrt(2)`, `sin(pi/2)`. Ничего не подошло — None.
pub fn calculate(q: &str) -> Option<CalcAnswer> {
    let q = q.trim();
    if !looks_like_math(q) {
        return None;
    }
    if let Some(c) = convert_units(q) {
        let result = format_result(c.result);
        return Some(CalcAnswer {
            display: format!("{} {} = {} {}", c.value_text, c.from, result, c.to),
            copy: result,
            expression: q.to_string(),
        });
    }
    evaluate_expression(q).map(|v| {
        let display = format_result(v);
        CalcAnswer { copy: display.clone(), display, expression: q.to_string() }
    })
}

/// Провайдер калькулятора: не более одного элемента на запрос, действие —
/// скопировать результат. Порядок поиска для конвертера: см. `convert_units`.
pub struct CalcProvider;

impl SearchProvider for CalcProvider {
    fn name(&self) -> &str {
        "calc"
    }

    fn priority(&self) -> i64 {
        ranking::PRIORITY_CALC
    }

    fn query(&self, q: &str) -> Vec<SearchItem> {
        match calculate(q) {
            Some(answer) => vec![SearchItem {
                id: format!("calc:{}", answer.expression),
                provider: self.name().to_string(),
                title: answer.display.clone(),
                subtitle: Some(answer.expression),
                icon_path: None,
                // выражение совпадает с запросом целиком — «точное совпадение»
                score: fuzzy::MAX_SCORE,
                action: ItemAction::CopyText { text: answer.copy },
            }],
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::fuzzy::MAX_SCORE;

    /// Выражения из чек-листа приёмки (§5): `2+2*2`, `sqrt(2)`, `sin(pi/2)`.
    #[test]
    fn expressions() {
        assert_eq!(format_result(evaluate_expression("2+2*2").unwrap()), "6");
        assert_eq!(format_result(evaluate_expression("sin(pi/2)").unwrap()), "1");
        assert_eq!(format_result(evaluate_expression("sqrt(2)").unwrap()), "1.414214");
        assert_eq!(format_result(evaluate_expression("10/4").unwrap()), "2.5");
        assert!(evaluate_expression("10 kg in lb").is_none(), "мусор для meval — None");
        assert!(evaluate_expression("1/0").is_none(), "inf не показываем");
        // meval: унарный минус крепче степени → -(3^2) = -9
        assert_eq!(format_result(evaluate_expression("-3^2").unwrap()), "-9");
    }

    /// Конвертер из чек-листа: `10 km in mi`, `72 f to c` + RU-алиасы и форматы.
    #[test]
    fn unit_conversions() {
        let mi = convert_units("10 km in mi").unwrap();
        assert_eq!(mi.value_text, "10");
        assert_eq!((mi.result * 1.0e4).round(), 62137.0, "10 км ≈ 6.2137 мили");

        let c = convert_units("72 f to c").unwrap();
        assert!((c.result - 22.222222).abs() < 1.0e-5, "72°F ≈ 22.22°C, получено {}", c.result);

        let kg = convert_units("1,5 кг в г").unwrap();
        assert_eq!(format_result(kg.result), "1500", "запятая-десятичная + RU-алиасы");

        let glued = convert_units("10km in mi").unwrap();
        assert_eq!(format_result(glued.result), "6.213712");

        let gb = convert_units("2 gb in mb").unwrap();
        assert_eq!(format_result(gb.result), "2000", "данные десятичные");

        let h = convert_units("90 min in h").unwrap();
        assert_eq!(format_result(h.result), "1.5");

        // температура по Кельвину и обратная
        let k = convert_units("0 c in k").unwrap();
        assert!((k.result - 273.15).abs() < 1.0e-9);

        // отрицательные
        let neg = convert_units("-40 f to c").unwrap();
        assert_eq!(format_result(neg.result), "-40", "знаменитая точка пересечения шкал");
    }

    /// Конвертер молчит: разные категории, нет единицы, нет разделителя, не «in».
    #[test]
    fn conversions_rejected() {
        assert!(convert_units("10 km in kg").is_none(), "длина в массу");
        assert!(convert_units("10 in mi").is_none(), "нет единицы-источника");
        assert!(convert_units("10 km mi").is_none(), "нет разделителя");
        assert!(convert_units("10 km into mi").is_none(), "«into» не разделитель");
        assert!(convert_units("привет in mi").is_none(), "не число");
        assert!(convert_units("10 xyz in mi").is_none(), "неизвестная единица");
    }

    /// Детект запроса: начинается с цифры/скобки/знака и валиден — иначе молчим.
    #[test]
    fn query_detection() {
        assert!(looks_like_math("2+2*2"));
        assert!(looks_like_math("(1+2)*3"));
        assert!(looks_like_math("-5+1"));
        assert!(looks_like_math("10 km in mi"));
        assert!(!looks_like_math("хром"));
        assert!(!looks_like_math("скачат"));
        assert!(!looks_like_math(""));
        assert!(!looks_like_math("-")); // знак без цифры
        assert!(calculate("хром").is_none());
        assert!(calculate("settings display").is_none());
    }

    /// Провайдер: ≤ 1 элемент, id/provider/action/скор; для левого запроса — пусто.
    #[test]
    fn provider_shape() {
        let p = CalcProvider;
        assert_eq!(p.name(), "calc");
        assert_eq!(p.priority(), ranking::PRIORITY_CALC);

        let items = p.query("2+2*2");
        assert_eq!(items.len(), 1);
        let it = &items[0];
        assert_eq!(it.id, "calc:2+2*2");
        assert_eq!(it.provider, "calc");
        assert_eq!(it.title, "6");
        assert_eq!(it.subtitle.as_deref(), Some("2+2*2"));
        assert_eq!(it.score, MAX_SCORE, "выражение совпадает с запросом — точное");
        assert_eq!(it.action, ItemAction::CopyText { text: "6".to_string() });

        let conv = p.query("10 km in mi");
        assert_eq!(conv.len(), 1);
        assert_eq!(conv[0].title, "10 km = 6.213712 mi");
        assert_eq!(conv[0].action, ItemAction::CopyText { text: "6.213712".to_string() });

        assert!(p.query("").is_empty(), "пустой запрос — молчание");
        assert!(p.query("chrome").is_empty());
    }

    /// Формат чисел: целые без «.0», дробные — хвостовые нули срезаны.
    #[test]
    fn number_formatting() {
        assert_eq!(format_result(6.0), "6");
        assert_eq!(format_result(-0.0), "0");
        assert_eq!(format_result(2.5), "2.5");
        assert_eq!(format_result(1.4142135623730951), "1.414214");
        assert_eq!(format_result(1.0e6 + 0.5), "1000000.5");
        assert_eq!(format_result(2.0e15), "2000000000000000");
    }
}
