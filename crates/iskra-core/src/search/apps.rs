//! Провайдер приложений (план Ф2, шаг 2, решения D4/D6): обход Start Menu
//! (ProgramData + %APPDATA%) и рабочих столов, разбор .lnk крейтом parselnk
//! (pure Rust, без COM). Битые .lnk — skip+лог, не падение (риск 2).
//!
//! target берётся из LinkInfo.local_base_path (абсолютный путь), при его
//! отсутствии — relative_path (напр. в рукотворных .lnk); нет ни того, ни
//! другого (UWP/Store-заглушки) — пропуск, COM-вариант запланирован на фазу 3.
//! Иконки здесь НЕ извлекаются: AppEntry.icon_source хранит исходник
//! (шаг 3 извлечёт HICON → PNG в кэш, шаг 4 подставит путь в item.icon_path).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use parselnk::Lnk;

use super::fuzzy;
use super::provider::SearchProvider;
use super::ranking;
use super::types::{ItemAction, SearchItem};
use crate::logging;

/// Глубина обхода каталогов ярлыков (Start Menu вложен неглубоко).
pub const MAX_DEPTH: usize = 6;

/// Сколько приложений отдаёт провайдер на один запрос (агрегатор режет до 50).
pub const MAX_PER_QUERY: usize = 30;

/// Одно приложение из .lnk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// Заголовок — имя .lnk без расширения (оригинальный регистр).
    pub title: String,
    /// Цель запуска (из .lnk); абсолютный путь, UNC или относительный fallback.
    pub target: String,
    /// Аргументы командной строки из .lnk.
    pub args: Option<String>,
    /// Исходник иконки (icon_location из .lnk) — шаг 3 извлечёт в кэш PNG.
    pub icon_source: Option<String>,
}

/// Итог сканирования: приложения + число пропущенных битых ярлыков.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub apps: Vec<AppEntry>,
    pub broken: usize,
}

/// Каталоги ярлыков по умолчанию: Start Menu (ProgramData + APPDATA),
/// рабочий стол пользователя и общий рабочий стол (Public Desktop).
pub fn default_app_dirs() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    // ProgramData: Start Menu + Public Desktop
    if let Ok(pd) = std::env::var("ProgramData") {
        let pd = PathBuf::from(pd);
        roots.push(pd.join(r"Microsoft\Windows\Start Menu\Programs"));
        roots.push(pd.join("Desktop"));
    }
    // APPDATA (roaming): Start Menu пользователя
    let appdata = std::env::var("APPDATA").ok().map(PathBuf::from).or_else(dirs::data_dir);
    if let Some(ad) = appdata {
        roots.push(ad.join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    // Рабочий стол пользователя
    if let Some(desktop) = dirs::desktop_dir() {
        roots.push(desktop);
    }
    roots.into_iter().filter(|p| p.is_dir()).collect()
}

/// Сканировать каталоги ярлыков (лог пропущенных — в runtime.log).
pub fn scan_apps(roots: &[PathBuf]) -> Vec<AppEntry> {
    scan_apps_report(roots).apps
}

/// То же с числом битых .lnk (для тестов и диагностики).
///
/// ДЕДУП ПО ЦЕЛИ (полировка фазы 2): несколько .lnk на один exe (ProgramData +
/// пользовательский Start Menu + копии вида «Name (2).lnk») должны давать ОДНУ
/// строку — иначе в выдаче «HWiNFO64 ×5 с одинаковым путём». Ключ —
/// `target.to_lowercase()` (совпадает с id элемента `apps:{target.lowercase()}`).
/// Порядок корней значим: первый найденный .lnk выигрывает, а `default_app_dirs`
/// кладёт ProgramData раньше пользовательских каталогов.
pub fn scan_apps_report(roots: &[PathBuf]) -> ScanReport {
    let mut report = ScanReport::default();
    let mut seen_targets: HashSet<String> = HashSet::new();
    for root in roots {
        // Обход корней ПО ОЧЕРЕДИ (не общий стек) — приоритет ранних корней.
        let mut stack: Vec<(PathBuf, usize)> =
            if root.is_dir() { vec![(root.clone(), 0)] } else { Vec::new() };
        while let Some((dir, depth)) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    logging::warn(&format!("apps: недоступен каталог {}: {e}", dir.display()));
                    continue;
                }
            };
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                let path = entry.path();
                if meta.is_dir() {
                    if depth < MAX_DEPTH {
                        stack.push((path, depth + 1));
                    }
                } else if meta.is_file()
                    && path.extension().map(|e| e.eq_ignore_ascii_case("lnk")).unwrap_or(false)
                {
                    match parse_lnk(&path) {
                        Some(app) => {
                            // Дедуп: одна цель — одна строка; первый .lnk выигрывает.
                            if seen_targets.insert(app.target.to_lowercase()) {
                                report.apps.push(app);
                            }
                        }
                        None => {
                            report.broken += 1;
                            logging::warn(&format!("apps: битый .lnk, пропускаю: {}", path.display()));
                        }
                    }
                }
            }
        }
    }
    report
}

/// Разобрать один .lnk → AppEntry; None = битый/бесцельный ярлык.
fn parse_lnk(path: &Path) -> Option<AppEntry> {
    let lnk = Lnk::try_from(path).ok()?;
    let title = path.file_stem()?.to_string_lossy().into_owned();
    let target = lnk
        .link_info
        .local_base_path
        .clone()
        .or_else(|| lnk.link_info.local_base_path_unicode.clone())
        .or_else(|| lnk.relative_path().map(|p| p.to_string_lossy().into_owned()))?;
    let icon_source = lnk
        .string_data
        .icon_location
        .clone()
        .map(|p| p.to_string_lossy().into_owned());
    Some(AppEntry { title, target, args: lnk.arguments(), icon_source })
}

/// Провайдер приложений: fuzzy по заголовкам предзагруженного списка.
pub struct AppsProvider {
    entries: Vec<AppEntry>,
}

impl AppsProvider {
    /// Провайдер поверх готового списка (тесты, инъекция).
    pub fn new(entries: Vec<AppEntry>) -> Self {
        AppsProvider { entries }
    }

    /// Провайдер, просканировавший заданные каталоги.
    pub fn scan(roots: &[PathBuf]) -> Self {
        AppsProvider::new(scan_apps(roots))
    }

    /// Провайдер с каталогами по умолчанию (реальный Start Menu + Desktop).
    pub fn load() -> Self {
        AppsProvider::scan(&default_app_dirs())
    }

    /// Сколько приложений в списке.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl SearchProvider for AppsProvider {
    fn name(&self) -> &str {
        "apps"
    }

    fn priority(&self) -> i64 {
        ranking::PRIORITY_APPS
    }

    /// Fuzzy-поиск по заголовкам (нормализация ё/регистра — внутри fuzzy).
    /// Порог MIN_MEANINGFUL отсекает мусорные подпоследовательности.
    fn query(&self, q: &str) -> Vec<SearchItem> {
        if q.trim().is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(i64, usize)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (fuzzy::score(q, &e.title), i))
            .filter(|(s, _)| *s >= fuzzy::MIN_MEANINGFUL)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(MAX_PER_QUERY)
            .map(|(s, i)| {
                let e = &self.entries[i];
                SearchItem {
                    id: format!("apps:{}", e.target.to_lowercase()),
                    provider: "apps".to_string(),
                    title: e.title.clone(),
                    subtitle: Some(e.target.clone()),
                    // PNG-иконка в кэше появится на шагах 3–4 (D7).
                    icon_path: None,
                    score: s,
                    action: ItemAction::LaunchApp {
                        path: e.target.clone(),
                        args: e.args.clone(),
                    },
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::db::unique_temp_dir;
    use std::fs;

    /// Минимальный валидный .lnk (MS-SHELLINK): заголовок 76 байт + StringData
    /// (relative_path, arguments, icon_location; UTF-16LE, u16-счётчик символов).
    /// parselnk не требует LinkInfo/IDList/ExtraData — EOF завершает разбор.
    fn minimal_lnk(relative: &str, args: &str, icon: &str) -> Vec<u8> {
        const HAS_RELATIVE_PATH: u32 = 0x8;
        const HAS_ARGUMENTS: u32 = 0x20;
        const HAS_ICON_LOCATION: u32 = 0x40;
        const IS_UNICODE: u32 = 0x80;
        let mut b = Vec::new();
        b.extend_from_slice(&0x4Cu32.to_le_bytes()); // HeaderSize
        // LinkCLSID 00021401-0000-0000-C000-000000000046 (байтовый порядок в файле)
        b.extend_from_slice(&[
            0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x46,
        ]);
        b.extend_from_slice(
            &(HAS_RELATIVE_PATH | HAS_ARGUMENTS | HAS_ICON_LOCATION | IS_UNICODE).to_le_bytes(),
        );
        b.extend_from_slice(&0x20u32.to_le_bytes()); // FILE_ATTRIBUTE_ARCHIVE
        b.extend_from_slice(&0u64.to_le_bytes()); // CreationTime
        b.extend_from_slice(&0u64.to_le_bytes()); // AccessTime
        b.extend_from_slice(&0u64.to_le_bytes()); // WriteTime
        b.extend_from_slice(&0u32.to_le_bytes()); // FileSize
        b.extend_from_slice(&0u32.to_le_bytes()); // IconIndex
        b.extend_from_slice(&1u32.to_le_bytes()); // ShowCommand = SW_SHOWNORMAL
        b.extend_from_slice(&0u16.to_le_bytes()); // HotKey
        b.extend_from_slice(&0u16.to_le_bytes()); // Reserved1
        b.extend_from_slice(&0u32.to_le_bytes()); // Reserved2
        b.extend_from_slice(&0u32.to_le_bytes()); // Reserved3
        for s in [relative, args, icon] {
            let units: Vec<u16> = s.encode_utf16().collect();
            b.extend_from_slice(&(units.len() as u16).to_le_bytes());
            for u in units {
                b.extend_from_slice(&u.to_le_bytes());
            }
        }
        b
    }

    /// Записать минимальный .lnk в каталог; вернуть путь.
    fn write_lnk(dir: &Path, name: &str, relative: &str, args: &str, icon: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, minimal_lnk(relative, args, icon)).unwrap();
        p
    }

    /// tmp-дерево с валидным .lnk: скан находит, парсит (title/target/args/icon),
    /// провайдер отдаёт элемент с LaunchApp.
    #[test]
    fn scan_and_parse_minimal_lnk_on_tmp_tree() {
        let dir = unique_temp_dir("apps-tmp");
        let programs = dir.join("programs");
        fs::create_dir_all(&programs).unwrap();

        write_lnk(&programs, "Тест приложение.lnk", "target.exe", "--min", "C:\\i\\icon.dll");

        let report = scan_apps_report(&[dir.clone()]);
        assert_eq!(report.broken, 0);
        assert_eq!(report.apps.len(), 1);
        let app = &report.apps[0];
        assert_eq!(app.title, "Тест приложение", "заголовок — stem .lnk");
        assert_eq!(app.target, "target.exe");
        assert_eq!(app.args.as_deref(), Some("--min"));
        assert_eq!(app.icon_source.as_deref(), Some("C:\\i\\icon.dll"));

        let provider = AppsProvider::new(report.apps);
        assert_eq!(provider.name(), "apps");
        assert_eq!(provider.priority(), ranking::PRIORITY_APPS);
        assert!(provider.query("").is_empty(), "пустой запрос провайдер молчит");

        let items = provider.query("тест");
        assert_eq!(items.len(), 1, "«тест» находит «Тест приложение»");
        let it = &items[0];
        assert_eq!(it.title, "Тест приложение");
        assert_eq!(it.provider, "apps");
        assert!(it.score >= fuzzy::MIN_MEANINGFUL);
        assert_eq!(
            it.action,
            ItemAction::LaunchApp {
                path: "target.exe".to_string(),
                args: Some("--min".to_string()),
            }
        );
        assert!(it.id.starts_with("apps:"));

        let _ = fs::remove_dir_all(&dir);
    }

    /// Битый .lnk (мусорные байты) — skip+лог, скан продолжается; бесцельный
    /// (валидный, но без строк) тоже пропускается.
    #[test]
    fn broken_lnk_skipped_with_log() {
        let dir = unique_temp_dir("apps-broken");
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("мусор.lnk"), b"not a lnk file at all").unwrap();
        // валидный заголовок, но строк нет → нет цели → пропуск
        let mut empty = minimal_lnk("", "", "");
        // обрежем до заголовка (76 байт + 3 пустые строки уже в minimal_lnk; соберём чистый)
        empty.truncate(76);
        fs::write(dir.join("без цели.lnk"), &empty).unwrap();
        write_lnk(&dir, "рабочий.lnk", "app.exe", "", "");

        let report = scan_apps_report(&[dir.clone()]);
        assert_eq!(report.broken, 2, "мусорный и бесцельный — оба в broken");
        assert_eq!(report.apps.len(), 1);
        assert_eq!(report.apps[0].title, "рабочий");

        let _ = fs::remove_dir_all(&dir);
    }

    /// ДЕДУП ПО ЦЕЛИ (полировка фазы 2): два .lnk с одним target → ОДИН
    /// SearchItem; приоритет корня — первый найденный выигрывает
    /// (ProgramData раньше пользовательского в default_app_dirs).
    #[test]
    fn duplicate_targets_collapse_first_root_wins() {
        let dir = unique_temp_dir("apps-dedup");
        // «ProgramData» (первый корень) и «пользовательский» (второй):
        // одинаковая цель, разные ярлыки/иконки.
        let pd = dir.join("ProgramData");
        let user = dir.join("user");
        fs::create_dir_all(&pd).unwrap();
        fs::create_dir_all(&user).unwrap();
        write_lnk(&pd, "HWiNFO64.lnk", "hwinfo64.exe", "", "C:\\i\\pd.dll");
        // тот же target, другой стем и другая иконка — должен схлопнуться:
        write_lnk(&user, "HWiNFO64 (2).lnk", "hwinfo64.exe", "", "C:\\i\\user.dll");
        // и другой ярлык на ДРУГУЮ цель — не должен пропасть:
        write_lnk(&user, "Comet.lnk", "comet.exe", "", "");

        let report = scan_apps_report(&[pd.clone(), user.clone()]);
        assert_eq!(report.broken, 0);
        assert_eq!(report.apps.len(), 2, "дубль цели схлопнут, уникальные остались");
        assert_eq!(report.apps[0].title, "HWiNFO64", "выиграл .lnk из ПЕРВОГО корня");
        assert_eq!(report.apps[0].icon_source.as_deref(), Some("C:\\i\\pd.dll"));
        assert_eq!(report.apps[1].title, "Comet");

        // Провайдер поверх дедуплицированного списка: ровно один элемент на цель,
        // id = apps:{target.lowercase()} (страховка агрегатора дедупит и по нему).
        let provider = AppsProvider::new(scan_apps(&[pd, user]));
        let items = provider.query("hwinfo");
        assert_eq!(items.len(), 1, "два .lnk с одним target → один SearchItem");
        assert_eq!(items[0].id, "apps:hwinfo64.exe");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Два .lnk с одним target в ОДНОМ каталоге (копии «(2)») — тоже одна строка.
    #[test]
    fn duplicate_targets_in_same_dir_collapse() {
        let dir = unique_temp_dir("apps-dedup-samedir");
        fs::create_dir_all(&dir).unwrap();
        write_lnk(&dir, "App.lnk", "app.exe", "", "");
        write_lnk(&dir, "App (2).lnk", "app.exe", "", "");
        write_lnk(&dir, "App (3).lnk", "APP.EXE", "", ""); // регистр цели не важен

        let report = scan_apps_report(&[dir.clone()]);
        assert_eq!(report.apps.len(), 1, "одна цель (без учёта регистра) — одна строка");
        assert_eq!(report.broken, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    /// Пустое дерево → ни приложений, ни битых.
    #[test]
    fn empty_tree_yields_nothing() {
        let dir = unique_temp_dir("apps-empty");
        fs::create_dir_all(&dir).unwrap();
        let report = scan_apps_report(&[dir.clone()]);
        assert!(report.apps.is_empty() && report.broken == 0);
        assert!(AppsProvider::new(vec![]).query("хром").is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Реальный Start Menu: если каталоги доступны — там есть приложения
    /// (и все записи имеют непустые title/target). Пропуск, если окружение
    /// без Start Menu.
    #[test]
    fn real_start_menu_scan_when_available() {
        let roots = default_app_dirs();
        if roots.is_empty() {
            eprintln!("skip: каталоги Start Menu/Desktop недоступны");
            return;
        }
        let report = scan_apps_report(&roots);
        assert!(
            !report.apps.is_empty(),
            "в реальном Start Menu есть .lnk (broken = {})",
            report.broken
        );
        for app in &report.apps {
            assert!(!app.title.is_empty());
            assert!(!app.target.is_empty());
        }
    }
}
