//! Web-fallback провайдер (план Ф2, шаг 1): по любому непустому запросу,
//! не покрытому другими провайдерами, — «искать в интернете».
//!
//! Ядро не знает, какой браузер по умолчанию: провайдер формирует `Action::WebSearch`
//! c готовым URL по шаблону; открытие через браузер по умолчанию — ShellExecuteW
//! в iskra-sys (шаг 3) / сервис app (шаг 4). Шаблон URL инъектируется (по умолчанию
//! Bing, `{q}` — placeholder для percent-encoded запроса).

use super::provider::SearchProvider;
use super::ranking;
use super::types::{ItemAction, SearchItem};

/// Шаблон поиска по умолчанию (Bing: не требует ключей/согласия на телеметрию
/// конкретного браузера; меняется инъекцией шаблона).
pub const DEFAULT_URL_TEMPLATE: &str = "https://www.bing.com/search?q={q}";

/// Провайдер web-fallback. Приоритет минимальный (10): элемент показывается,
/// но всегда ниже приложений/файлов.
pub struct WebProvider {
    url_template: String,
}

impl Default for WebProvider {
    fn default() -> Self {
        WebProvider { url_template: DEFAULT_URL_TEMPLATE.to_string() }
    }
}

impl WebProvider {
    /// Свой шаблон URL с плейсхолдером `{q}`.
    pub fn with_template(template: impl Into<String>) -> Self {
        WebProvider { url_template: template.into() }
    }

    /// Поисковый URL для запроса: `{q}` → percent-encoded строка.
    pub fn search_url(&self, q: &str) -> String {
        self.url_template.replace("{q}", &percent_encode(q))
    }
}

/// RFC 3986: незарезервированные символы (ALPHA / DIGIT / `-` / `.` / `_` / `~`)
/// остаются как есть, остальное — %XX по UTF-8. Пробел → %20.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

impl SearchProvider for WebProvider {
    fn name(&self) -> &str {
        "web"
    }

    fn priority(&self) -> i64 {
        ranking::PRIORITY_WEB
    }

    fn query(&self, q: &str) -> Vec<SearchItem> {
        let q = q.trim();
        if q.is_empty() {
            return Vec::new();
        }
        vec![SearchItem {
            id: format!("web:{q}"),
            provider: self.name().to_string(),
            title: format!("Искать «{q}» в интернете"),
            subtitle: None,
            icon_path: None,
            // сам запрос совпадает с собой — «точное совпадение» внутри провайдера;
            // итоговый порядок всё равно задаёт приоритет 10
            score: super::fuzzy::MAX_SCORE,
            action: ItemAction::WebSearch { url: self.search_url(q) },
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Фолбэк отвечает ровно одним элементом на любой непустой запрос.
    #[test]
    fn always_answers_once() {
        let p = WebProvider::default();
        for q in ["хрм", "zzqqxx", "кто такой верблюд", "!!"] {
            let items = p.query(q);
            assert_eq!(items.len(), 1, "запрос {q}");
            let it = &items[0];
            assert_eq!(it.provider, "web");
            assert_eq!(it.id, format!("web:{q}"));
            assert!(it.title.contains(q));
            assert_eq!(it.score, crate::search::fuzzy::MAX_SCORE, "фолбэк всегда 1000 внутри провайдера");
        }
        assert!(p.query("").is_empty());
        assert!(p.query("   ").is_empty(), "пробельный запрос — молчание");
    }

    /// URL: шаблон подставлен, кириллица/пробелы percent-encoded (UTF-8).
    #[test]
    fn url_encoding() {
        let p = WebProvider::default();
        let url = p.search_url("хром тест");
        assert!(url.starts_with("https://www.bing.com/search?q="), "{url}");
        assert!(!url.contains(' '), "пробел закодирован: {url}");
        assert!(url.contains("%20"), "{url}");
        // «х» = U+0445 = D1 85
        assert!(url.contains("%D1%85"), "{url}");
        // ASCII-буквы и незарезервированные не кодируются
        let plain = p.search_url("abc-def_ghi~jkl.mno");
        assert!(plain.ends_with("abc-def_ghi~jkl.mno"), "{plain}");

        // свой шаблон
        let ddg = WebProvider::with_template("https://duckduckgo.com/?q={q}");
        assert!(ddg.search_url("a b").starts_with("https://duckduckgo.com/?q=a%20b"));
    }

    /// Приоритет — минимальный из провайдеров (web-fallback 10).
    #[test]
    fn lowest_priority() {
        let p = WebProvider::default();
        assert_eq!(p.priority(), ranking::PRIORITY_WEB);
        assert!(p.priority() < ranking::PRIORITY_SYSTEM);
        assert!(p.priority() < ranking::PRIORITY_FILES);
    }
}
