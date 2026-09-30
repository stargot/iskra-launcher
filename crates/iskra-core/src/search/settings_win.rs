//! Провайдер настроек Windows: топ-50 `ms-settings:` URI (план Ф2, шаг 1).
//!
//! Статический список (без сети/реестра): RU-заголовок + EN-подзаголовок +
//! ключевые слова RU/EN. Скор — максимум fuzzy по заголовку/URI/ключевым словам;
//! провайдер отдаёт не более `MAX_RESULTS` лучших. Открытие URI — `OpenUri`,
//! исполнение на стороне sys/app (шаги 3–4).

use super::fuzzy;
use super::provider::SearchProvider;
use super::ranking;
use super::types::{ItemAction, SearchItem};

/// Максимум результатов от провайдера за запрос.
pub const MAX_RESULTS: usize = 15;

/// Порог осмысленного совпадения (см. fuzzy::MIN_MEANINGFUL).
const MIN_MATCH: i64 = fuzzy::MIN_MEANINGFUL;

/// Одна запись каталога настроек.
struct SettingEntry {
    /// URI без префикса `ms-settings:` (id элемента — полный URI).
    uri: &'static str,
    /// RU-заголовок (как в «Параметрах» Windows).
    title: &'static str,
    /// EN-подзаголовок.
    subtitle: &'static str,
    /// Дополнительные ключевые слова RU/EN через пробел.
    keywords: &'static str,
}

/// Топ-50 настроек: покрывает частые сценарии лончера (экран, звук, сеть,
/// обновления, принтеры, bluetooth и т.д.). Расширять — в конец списка.
static SETTINGS: [SettingEntry; 50] = [
    SettingEntry { uri: "display", title: "Дисплей", subtitle: "Display", keywords: "экран дисплей монитор яркость разрешение display monitor brightness" },
    SettingEntry { uri: "nightlight", title: "Ночной свет", subtitle: "Night light", keywords: "ночной свет синий фильтр night light" },
    SettingEntry { uri: "sound", title: "Звук", subtitle: "Sound", keywords: "звук громкость аудио колонки микрофон sound volume audio" },
    SettingEntry { uri: "notifications", title: "Уведомления", subtitle: "Notifications", keywords: "уведомления оповещения notifications" },
    SettingEntry { uri: "powersleep", title: "Питание и спящий режим", subtitle: "Power & sleep", keywords: "питание сон спящий режим батарея power sleep" },
    SettingEntry { uri: "batterysaver", title: "Экономия заряда", subtitle: "Battery saver", keywords: "батарея заряд экономия battery saver" },
    SettingEntry { uri: "storagesense", title: "Память", subtitle: "Storage", keywords: "память диск хранилище место очистка storage disk" },
    SettingEntry { uri: "appsfeatures", title: "Приложения и компоненты", subtitle: "Apps & features", keywords: "приложения программы удаление uninstall apps" },
    SettingEntry { uri: "defaultapps", title: "Приложения по умолчанию", subtitle: "Default apps", keywords: "по умолчанию ассоциации форматы default apps" },
    SettingEntry { uri: "startupapps", title: "Автозагрузка", subtitle: "Startup apps", keywords: "автозагрузка автозапуск startup" },
    SettingEntry { uri: "bluetooth", title: "Bluetooth и устройства", subtitle: "Bluetooth & devices", keywords: "блютуз bluetooth устройства устройства devices" },
    SettingEntry { uri: "printers", title: "Принтеры и сканеры", subtitle: "Printers & scanners", keywords: "принтер печать сканер printer scan" },
    SettingEntry { uri: "mouse", title: "Мышь", subtitle: "Mouse", keywords: "мышь курсор указатель mouse pointer" },
    SettingEntry { uri: "keyboard", title: "Ввод", subtitle: "Typing", keywords: "клавиатура ввод раскладка keyboard typing" },
    SettingEntry { uri: "touchpad", title: "Сенсорная панель", subtitle: "Touchpad", keywords: "тачпад сенсорная панель touchpad" },
    SettingEntry { uri: "network-status", title: "Состояние сети", subtitle: "Network status", keywords: "сеть интернет ethernet network internet" },
    SettingEntry { uri: "network-wifi", title: "Wi-Fi", subtitle: "Wi-Fi", keywords: "вайфай wifi беспроводная сеть wireless" },
    SettingEntry { uri: "mobilehotspot", title: "Мобильный хот-спот", subtitle: "Mobile hotspot", keywords: "хотспот точка доступа раздача hotspot tethering" },
    SettingEntry { uri: "airplanemode", title: "Режим «в самолёте»", subtitle: "Airplane mode", keywords: "самолёт авиарежим airplane" },
    SettingEntry { uri: "vpn", title: "VPN", subtitle: "VPN", keywords: "впн vpn туннель" },
    SettingEntry { uri: "proxy", title: "Прокси-сервер", subtitle: "Proxy", keywords: "прокси proxy" },
    SettingEntry { uri: "personalization", title: "Персонализация", subtitle: "Personalization", keywords: "персонализация оформление personalization" },
    SettingEntry { uri: "personalization-background", title: "Фон", subtitle: "Background", keywords: "фон обои картинка стола wallpaper background" },
    SettingEntry { uri: "personalization-colors", title: "Цвета", subtitle: "Colors", keywords: "цвета тема акцент colors theme accent" },
    SettingEntry { uri: "lockscreen", title: "Экран блокировки", subtitle: "Lock screen", keywords: "экран блокировки lockscreen" },
    SettingEntry { uri: "themes", title: "Темы", subtitle: "Themes", keywords: "тема темы themes" },
    SettingEntry { uri: "taskbar", title: "Панель задач", subtitle: "Taskbar", keywords: "панель задач трей taskbar" },
    SettingEntry { uri: "multitasking", title: "Многозадачность", subtitle: "Multitasking", keywords: "многозадачность прикрепление snap multitasking" },
    SettingEntry { uri: "fonts", title: "Шрифты", subtitle: "Fonts", keywords: "шрифт шрифты fonts" },
    SettingEntry { uri: "yourinfo", title: "Ваши данные", subtitle: "Your info", keywords: "аккаунт учётная запись профиль yourinfo account" },
    SettingEntry { uri: "signinoptions", title: "Варианты входа", subtitle: "Sign-in options", keywords: "вход пин пароль windows hello signinoptions" },
    SettingEntry { uri: "dateandtime", title: "Дата и время", subtitle: "Date & time", keywords: "дата время часовой пояс часы date time clock" },
    SettingEntry { uri: "regionformatting", title: "Регион", subtitle: "Region", keywords: "регион страна формат региональные region" },
    SettingEntry { uri: "privacy-location", title: "Расположение", subtitle: "Location", keywords: "геолокация расположение gps location" },
    SettingEntry { uri: "privacy-camera", title: "Камера", subtitle: "Camera", keywords: "камера вебка camera webcam" },
    SettingEntry { uri: "privacy-microphone", title: "Микрофон", subtitle: "Microphone", keywords: "микрофон microphone" },
    SettingEntry { uri: "windowsdefender", title: "Безопасность Windows", subtitle: "Windows Security", keywords: "защитник безопасность антивирус defender security antivirus" },
    SettingEntry { uri: "windowsupdate", title: "Центр обновления Windows", subtitle: "Windows Update", keywords: "обновление обновления апдейт update" },
    SettingEntry { uri: "recovery", title: "Восстановление", subtitle: "Recovery", keywords: "восстановление откат сброс recovery" },
    SettingEntry { uri: "about", title: "О системе", subtitle: "About", keywords: "о системе характеристики система about system specs" },
    SettingEntry { uri: "troubleshoot", title: "Устранение неполадок", subtitle: "Troubleshoot", keywords: "неполадки проблемы диагностика troubleshoot" },
    SettingEntry { uri: "backup", title: "Архивация", subtitle: "Backup", keywords: "бэкап архивация резервная копия backup" },
    SettingEntry { uri: "remotedesktop", title: "Удалённый рабочий стол", subtitle: "Remote desktop", keywords: "удалённый рабочий стол rdp remote desktop" },
    SettingEntry { uri: "clipboard", title: "Буфер обмена", subtitle: "Clipboard", keywords: "буфер обмена clipboard" },
    SettingEntry { uri: "easeofaccess", title: "Специальные возможности", subtitle: "Ease of Access", keywords: "специальные возможности accessibility" },
    SettingEntry { uri: "easeofaccess-display", title: "Размер текста", subtitle: "Text size", keywords: "размер текста масштаб text size" },
    SettingEntry { uri: "easeofaccess-narrator", title: "Экранный диктор", subtitle: "Narrator", keywords: "диктор озвучивание narrator" },
    SettingEntry { uri: "easeofaccess-magnifier", title: "Экранная лупа", subtitle: "Magnifier", keywords: "лупа увеличение magnifier zoom" },
    SettingEntry { uri: "gamemode", title: "Игровой режим", subtitle: "Game Mode", keywords: "игры игровой режим game mode" },
    SettingEntry { uri: "developers", title: "Для разработчиков", subtitle: "For developers", keywords: "разработчик разработка developers" },
];

/// Полный URI настройки.
fn full_uri(e: &SettingEntry) -> String {
    format!("ms-settings:{}", e.uri)
}

/// Провайдер настроек Windows. Состояния нет — потокобезопасен по построению.
pub struct SettingsProvider;

impl SearchProvider for SettingsProvider {
    fn name(&self) -> &str {
        "settings"
    }

    fn priority(&self) -> i64 {
        ranking::PRIORITY_SETTINGS
    }

    fn query(&self, q: &str) -> Vec<SearchItem> {
        let q = q.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(i64, &SettingEntry)> = SETTINGS
            .iter()
            .map(|e| {
                // максимум по заголовку, uri, полному URI и ключевым словам
                let mut best = fuzzy::score(q, e.title)
                    .max(fuzzy::score(q, e.uri))
                    .max(fuzzy::score(q, &full_uri(e)));
                for kw in e.keywords.split_whitespace() {
                    best = best.max(fuzzy::score(q, kw));
                }
                (best, e)
            })
            .filter(|(s, _)| *s >= MIN_MATCH)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(s, e)| SearchItem {
                id: full_uri(e),
                provider: self.name().to_string(),
                title: e.title.to_string(),
                subtitle: Some(e.subtitle.to_string()),
                icon_path: None,
                score: s,
                action: ItemAction::OpenUri { uri: full_uri(e) },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каталог фиксирован планом: ровно 50 URI, все уникальные и непустые.
    #[test]
    fn catalog_is_50_unique() {
        assert_eq!(SETTINGS.len(), 50);
        let mut uris: Vec<_> = SETTINGS.iter().map(full_uri).collect();
        uris.sort();
        assert_eq!(uris.len(), uris.iter().len(), "URI уникальны");
        assert!(uris.iter().all(|u| u.starts_with("ms-settings:")));
    }

    /// RU-запросы из §5 приёмки: «экран», «звук», «принтер», «обновление».
    #[test]
    fn russian_queries() {
        let p = SettingsProvider;
        let top = |q: &str| p.query(q).first().map(|i| i.id.clone());

        assert_eq!(top("экран").as_deref(), Some("ms-settings:display"));
        assert_eq!(top("звук").as_deref(), Some("ms-settings:sound"));
        assert_eq!(top("принтер").as_deref(), Some("ms-settings:printers"));
        assert_eq!(top("обновление").as_deref(), Some("ms-settings:windowsupdate"));
        assert_eq!(top("блютуз").as_deref(), Some("ms-settings:bluetooth"));
        assert_eq!(top("обои").as_deref(), Some("ms-settings:personalization-background"));
    }

    /// EN-запросы и поиск по uri.
    #[test]
    fn english_queries_and_uri() {
        let p = SettingsProvider;
        let top = |q: &str| p.query(q).first().map(|i| i.id.clone());

        assert_eq!(top("display").as_deref(), Some("ms-settings:display"));
        assert_eq!(top("sound").as_deref(), Some("ms-settings:sound"));
        assert_eq!(top("printer").as_deref(), Some("ms-settings:printers"));
        assert_eq!(top("vpn").as_deref(), Some("ms-settings:vpn"));
        assert_eq!(top("ms-settings:display").as_deref(), Some("ms-settings:display"));
    }

    /// Форма результата: провайдер/settings, OpenUri, лимит, сортировка по скору.
    #[test]
    fn result_shape_and_limits() {
        let p = SettingsProvider;
        let items = p.query("экран");
        assert!(!items.is_empty());
        assert!(items.len() <= MAX_RESULTS);
        for w in items.windows(2) {
            assert!(w[0].score >= w[1].score, "отсортировано по убыванию скора");
        }
        let first = &items[0];
        assert_eq!(first.provider, "settings");
        assert_eq!(first.id, format!("ms-settings:{}", first.id.trim_start_matches("ms-settings:")), "id = полный URI");
        assert_eq!(
            first.action,
            ItemAction::OpenUri { uri: first.id.clone() },
            "действие — открыть тот же URI, что и id"
        );
        assert!(first.subtitle.is_some(), "EN-подзаголовок присутствует");

        assert!(p.query("").is_empty(), "пустой запрос — молчание");
        assert!(p.query("qqqqqqqqq").is_empty(), "мусор — молчание");
    }
}
