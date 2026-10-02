//! Провайдер приложений (план Ф2, шаг 2, решения D4/D6): обход Start Menu
//! (ProgramData + %APPDATA%) и рабочих столов, разбор .lnk крейтом parselnk
//! (pure Rust, без COM). Битые .lnk — skip+лог, не падение (риск 2).
//!
//! Scoop-шимы (минифича): пакеты scoop не кладут ярлыки в Start Menu, поэтому
//! сканируются каталоги шимов — %SCOOP%\shims (иначе %USERPROFILE%\scoop\shims)
//! и глобальные %SCOOP_GLOBAL%\shims (иначе %ProgramData%\scoop\shims). По
//! записи на `<имя>.exe`: target — сам шим (запуск корректен, аргументы
//! пробрасываются), иконный источник — «реальный» exe из соседнего
//! `<имя>.shim` (строка `path = "..."`, у него нормальная иконка); .shim нет/
//! бит — источник сам шим (WARN). GUI-scoop-приложения, у которых ярлык в
//! Start Menu есть, дедупятся в пользу ярлыка: ключ шима — разрешённый
//! «реальный» exe, совпал с целью .lnk — шим пропускается.
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

/// Каталоги сканирования по умолчанию: Start Menu (ProgramData + APPDATA),
/// рабочий стол пользователя, общий рабочий стол (Public Desktop) и scoop-шимы
/// (в конце — при дедупе ярлык Start Menu побеждает шим).
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
    // Scoop-шимы — последними (см. scan_full_report: lnk-обход раньше шимов).
    roots.extend(scoop_shim_dirs());
    roots.into_iter().filter(|p| p.is_dir()).collect()
}

/// Каталоги scoop-шимов: пользовательский `%SCOOP%\shims` (иначе
/// `%USERPROFILE%\scoop\shims`) плюс глобальный `%SCOOP_GLOBAL%\shims`
/// (иначе `%ProgramData%\scoop\shims`); несуществующие отброшены.
pub fn scoop_shim_dirs() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .ok()
        .or_else(|| dirs::home_dir().map(|p| p.to_string_lossy().into_owned()));
    scoop_shim_dirs_from(
        std::env::var("SCOOP").ok(),
        std::env::var("SCOOP_GLOBAL").ok(),
        home,
        std::env::var("ProgramData").ok(),
    )
    .into_iter()
    .filter(|p| p.is_dir())
    .collect()
}

/// Чистая сборка списка каталогов шимов из явных значений (без env и без
/// проверки существования) — маппинг тестируется без мутации env, что в
/// параллельных тестах недопустимо.
fn scoop_shim_dirs_from(
    scoop: Option<String>,
    scoop_global: Option<String>,
    home: Option<String>,
    program_data: Option<String>,
) -> Vec<PathBuf> {
    let mut shim_dirs = Vec::new();
    // Пользовательский корень: SCOOP перекрывает %USERPROFILE%\scoop.
    match scoop {
        Some(s) => shim_dirs.push(PathBuf::from(s).join("shims")),
        None => {
            if let Some(h) = home {
                shim_dirs.push(PathBuf::from(h).join(r"scoop\shims"));
            }
        }
    }
    // Глобальный корень: SCOOP_GLOBAL перекрывает %ProgramData%\scoop.
    match scoop_global {
        Some(g) => shim_dirs.push(PathBuf::from(g).join("shims")),
        None => {
            if let Some(pd) = program_data {
                shim_dirs.push(PathBuf::from(pd).join(r"scoop\shims"));
            }
        }
    }
    shim_dirs
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
///
/// Корни из `scoop_shim_dirs()` (кладёт туда `default_app_dirs`) сканируются
/// как scoop-шимы, остальные — как каталоги ярлыков; шимы всегда ПОСЛЕ
/// .lnk-обхода (см. `scan_full_report`). tmp-корни тестов с реальной машиной
/// не совпадают — детерминизм сохранён.
pub fn scan_apps_report(roots: &[PathBuf]) -> ScanReport {
    let scoop = scoop_shim_dirs();
    let is_shim_root = |p: &Path| scoop.iter().any(|s| s == p);
    let shim_roots: Vec<PathBuf> = roots.iter().filter(|p| is_shim_root(p)).cloned().collect();
    let lnk_roots: Vec<PathBuf> = roots.iter().filter(|p| !is_shim_root(p)).cloned().collect();
    scan_full_report(&lnk_roots, &shim_roots)
}

/// Полный скан с ЯВНЫМИ корнями: .lnk-каталоги + scoop-шимы (для тестов и
/// диагностики без мутации env; прод-путь — `scan_apps_report(&default_app_dirs())`).
pub fn scan_full_report(lnk_roots: &[PathBuf], shim_roots: &[PathBuf]) -> ScanReport {
    let mut report = ScanReport::default();
    let mut seen_targets: HashSet<String> = HashSet::new();
    for root in lnk_roots {
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
    // Scoop-шимы — после .lnk-обхода. Ключ дедупа — разрешённый «реальный» exe
    // (он же иконный источник; без .shim — сам шим): GUI-scoop-приложение с
    // ярлыком Start Menu на тот же exe уже в списке — шим пропускается (lnk
    // побеждает, у него нормальные имя/иконка); шимы между корнями схлопываются
    // по тому же ключу (первый корень выигрывает, как .lnk выше).
    let mut scoop_added = 0usize;
    let mut scoop_skipped = 0usize;
    for dir in shim_roots {
        for app in scan_scoop_shims(dir) {
            let key = app
                .icon_source
                .clone()
                .unwrap_or_else(|| app.target.clone())
                .to_lowercase();
            if seen_targets.insert(key) {
                report.apps.push(app);
                scoop_added += 1;
            } else {
                scoop_skipped += 1;
            }
        }
    }
    if !shim_roots.is_empty() {
        logging::info(&format!(
            "apps: scoop shims added={scoop_added} dedup_skipped={scoop_skipped}"
        ));
    }
    report
}

/// Разобрать каталог scoop-шимов (без рекурсии): по записи на каждый
/// `<имя>.exe`. target — путь шима (запуск шима корректен); иконный источник —
/// «реальный» exe из соседнего `<имя>.shim`, при отсутствии/бите — сам шим.
pub fn scan_scoop_shims(dir: &Path) -> Vec<AppEntry> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            logging::warn(&format!("apps: недоступен каталог шимов {}: {e}", dir.display()));
            return Vec::new();
        }
    };
    let mut exes: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false)
        })
        .collect();
    exes.sort(); // порядок read_dir не гарантирован — детерминизм для тестов
    exes.iter()
        .filter_map(|exe| {
            let title = exe.file_stem()?.to_string_lossy().into_owned();
            let shim = exe.with_extension("shim"); // <имя>.shim рядом
            let icon_source = match shim_path_line(&shim) {
                Some(real) => Some(real),
                None => {
                    logging::warn(&format!(
                        "apps: scoop: {} без строки path — иконка из самого шима",
                        shim.display()
                    ));
                    Some(exe.to_string_lossy().into_owned())
                }
            };
            Some(AppEntry {
                title,
                target: exe.to_string_lossy().into_owned(),
                args: None,
                icon_source,
            })
        })
        .collect()
}

/// Строка `path = "..."` из .shim → путь «реального» exe (кавычки снимаются);
/// нет файла/не читается/нет пути — None.
fn shim_path_line(shim: &Path) -> Option<String> {
    let text = std::fs::read_to_string(shim).ok()?;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        if !key.trim().eq_ignore_ascii_case("path") {
            continue;
        }
        let value = value.trim();
        let unquoted = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            &value[1..value.len() - 1]
        } else {
            value
        };
        if unquoted.is_empty() {
            return None;
        }
        return Some(unquoted.to_string());
    }
    None
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
    /// без Start Menu. На машине со scoop сюда входят и шимы (после .lnk).
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

    /// Fake-scoop: shim.exe + .shim с path = "…real.exe" (в кавычках, как
    /// пишет scoop; второй — без кавычек) → title=stem, target=шим,
    /// иконный источник = «реальный» exe; запуск — шимом (LaunchApp).
    #[test]
    fn scoop_shim_parses_to_entry_with_real_exe_icon() {
        let dir = unique_temp_dir("apps-scoop");
        let real_dir = dir.join("apps").join("bat").join("current");
        fs::create_dir_all(&real_dir).unwrap();
        let real = real_dir.join("bat.exe");
        fs::write(&real, b"MZ").unwrap();
        fs::write(dir.join("bat.exe"), b"MZ").unwrap();
        fs::write(dir.join("bat.shim"), format!("path = \"{}\"", real.display())).unwrap();
        // второй шим: путь без кавычек + лишние строки (env = …)
        fs::write(dir.join("raw.exe"), b"MZ").unwrap();
        fs::write(
            dir.join("raw.shim"),
            format!("env = x@y\npath = {}\n", real.display()),
        )
        .unwrap();

        let apps = scan_scoop_shims(&dir);
        assert_eq!(apps.len(), 2, "по записи на каждый .exe, без рекурсии");
        let bat = apps.iter().find(|a| a.title == "bat").expect("запись bat");
        assert_eq!(bat.target, dir.join("bat.exe").to_string_lossy().as_ref());
        assert_eq!(bat.icon_source.as_deref(), Some(real.to_string_lossy().as_ref()));
        assert_eq!(bat.args, None);
        let raw = apps.iter().find(|a| a.title == "raw").expect("запись raw");
        assert_eq!(raw.icon_source.as_deref(), Some(real.to_string_lossy().as_ref()));

        // Провайдер: запуск шимом, id — по пути шима (как у .lnk — по target).
        let provider = AppsProvider::new(apps);
        let items = provider.query("bat");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].action,
            ItemAction::LaunchApp {
                path: dir.join("bat.exe").to_string_lossy().into_owned(),
                args: None,
            }
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Нет .shim или битый (без строки path) → иконный источник = сам шим.
    #[test]
    fn scoop_shim_without_or_broken_shim_file_falls_back_to_shim() {
        let dir = unique_temp_dir("apps-scoop-fallback");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("plain.exe"), b"MZ").unwrap(); // .shim нет
        fs::write(dir.join("broken.exe"), b"MZ").unwrap();
        fs::write(dir.join("broken.shim"), "env = x\n").unwrap(); // нет path

        let apps = scan_scoop_shims(&dir);
        assert_eq!(apps.len(), 2);
        for app in &apps {
            let self_path = dir.join(format!("{}.exe", app.title));
            assert_eq!(
                app.icon_source.as_deref(),
                Some(self_path.to_string_lossy().as_ref()),
                "икона из самого шима: {}",
                app.title
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }

    /// ДЕДУП: .lnk на «реальный» exe + шим на тот же exe → шим пропущен (lnk
    /// побеждает, остаётся имя/иконка ярлыка); шим-алиас из второго корня на
    /// тот же exe тоже схлопнут; уникальный шим остаётся.
    #[test]
    fn scoop_shim_dedup_lnk_wins_and_between_roots() {
        let dir = unique_temp_dir("apps-scoop-dedup");
        let programs = dir.join("programs");
        let shims1 = dir.join("shims1");
        let shims2 = dir.join("shims2");
        for d in [&programs, &shims1, &shims2] {
            fs::create_dir_all(d).unwrap();
        }
        let real = dir.join("apps").join("Element").join("current").join("Element.exe");
        let real_s = real.to_string_lossy().into_owned();
        // Ярлык Start Menu (GUI-scoop-приложение): target — реальный exe.
        write_lnk(&programs, "Element.lnk", &real_s, "", "C:\\i\\lnk.dll");
        // Шим на тот же exe — должен быть пропущен.
        fs::write(shims1.join("element.exe"), b"MZ").unwrap();
        fs::write(shims1.join("element.shim"), format!("path = \"{}\"", real_s)).unwrap();
        // Шим-алиас из второго корня на тот же exe — тоже схлопнут.
        fs::write(shims2.join("elem2.exe"), b"MZ").unwrap();
        fs::write(shims2.join("elem2.shim"), format!("path = \"{}\"", real_s)).unwrap();
        // Уникальный шим — остаётся.
        let bat_real = dir.join("apps").join("bat").join("current").join("bat.exe");
        fs::write(shims2.join("bat.exe"), b"MZ").unwrap();
        fs::write(shims2.join("bat.shim"), format!("path = \"{}\"", bat_real.display())).unwrap();

        let report = scan_full_report(&[programs], &[shims1, shims2]);
        assert_eq!(report.broken, 0);
        assert_eq!(report.apps.len(), 2, "Element(lnk) + bat(шим); дубли схлопнуты");
        assert_eq!(report.apps[0].title, "Element", "lnk побеждает");
        assert_eq!(report.apps[0].icon_source.as_deref(), Some("C:\\i\\lnk.dll"));
        assert_eq!(report.apps[1].title, "bat");

        let provider = AppsProvider::new(report.apps);
        let items = provider.query("element");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, format!("apps:{}", real_s.to_lowercase()));

        let _ = fs::remove_dir_all(&dir);
    }

    /// Маршрутизация scan_apps_report: каталог, не входящий в scoop_shim_dirs
    /// (tmp-дерево), обрабатывается как .lnk-корень — .exe-шимы там игнорируются.
    #[test]
    fn exe_in_non_scoop_root_is_ignored() {
        let dir = unique_temp_dir("apps-scoop-routing");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("app.exe"), b"MZ").unwrap();
        fs::write(dir.join("app.shim"), "path = \"C:\\x\\real.exe\"").unwrap();
        write_lnk(&dir, "App.lnk", "app.exe", "", "");

        let report = scan_apps_report(&[dir.clone()]);
        assert_eq!(report.apps.len(), 1, "tmp-корень — не scoop: только .lnk");
        assert_eq!(report.apps[0].title, "App");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Маппинг SCOOP/SCOOP_GLOBAL/USERPROFILE/ProgramData → каталоги шимов
    /// (чистая функция — без мутации env, параллельные тесты в безопасности).
    #[test]
    fn scoop_shim_dirs_env_mapping() {
        // SCOOP задан → он перекрывает USERPROFILE; глобальный — ProgramData.
        assert_eq!(
            scoop_shim_dirs_from(
                Some(r"C:\scoop".into()),
                None,
                Some(r"C:\Users\u".into()),
                Some(r"C:\ProgramData".into()),
            ),
            vec![
                PathBuf::from(r"C:\scoop\shims"),
                PathBuf::from(r"C:\ProgramData\scoop\shims"),
            ]
        );
        // SCOOP не задан → USERPROFILE\scoop; SCOOP_GLOBAL задан → он.
        assert_eq!(
            scoop_shim_dirs_from(
                None,
                Some(r"D:\scoop-global".into()),
                Some(r"C:\Users\u".into()),
                None,
            ),
            vec![
                PathBuf::from(r"C:\Users\u\scoop\shims"),
                PathBuf::from(r"D:\scoop-global\shims"),
            ]
        );
        // Ничего не задано и переменных нет — пусто.
        assert!(scoop_shim_dirs_from(None, None, None, None).is_empty());
    }
}
