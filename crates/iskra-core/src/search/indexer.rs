//! Индексатор файлов (план Ф2, шаг 2): обход пользовательских папок,
//! mtime-инкремент, прогресс через mpsc-канал (`index://progress` в шаге 4).
//!
//! Границы обхода (§2/§3 плана): Desktop/Documents/Downloads/Pictures/Videos/
//! Music из %USERPROFILE%; глубина ≤ 5 от корня; исключённые каталоги
//! .git | node_modules | AppData (регистронезависимо); лимит 50 000 файлов
//! (при достижении — truncation-флаг, сверка удалённых в таком прогоне
//! НЕ выполняется, чтобы не снести «недошитые» записи).
//!
//! Полный рескан (`run_full`) — первый запуск; далее `run_incremental`
//! (на старте и раз в 30 мин, D9): пишет только изменившиеся по mtime файлы
//! и удаляет из индекса исчезнувшие (триггеры V2 чистят FTS автоматически).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use super::db::{ApplyMode, Db, FileRecord};
use crate::logging;

/// Максимальная глубина обхода от корневой папки (корень = 0).
pub const MAX_DEPTH: usize = 5;

/// Лимит файлов в индексе (§2 плана).
pub const MAX_FILES: usize = 50_000;

/// Периодичность сообщений прогресса (в файлах).
pub const PROGRESS_EVERY: usize = 256;

/// Фаза индексации (зеркалится в ipc-событии index://progress на шаге 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexPhase {
    /// Обход ФС: `done` — найдено файлов, `total` ещё неизвестен (0).
    Scanning,
    /// Запись в БД: `done`/`total` известны.
    Indexing,
    /// Сверка удалённых файлов.
    Cleaning,
    /// Прогон завершён.
    Done,
}

/// Сообщение прогресса индексации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexProgress {
    pub phase: IndexPhase,
    pub done: u64,
    pub total: u64,
}

/// Итог одного прогона индексатора (для логов и тестов).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexStats {
    /// Найдено файлов при обходе.
    pub discovered: usize,
    /// Записано в БД (вставлено + обновлено).
    pub indexed: usize,
    /// Пропущено без изменений (только инкрементальный прогон).
    pub unchanged: usize,
    /// Удалено исчезнувших записей.
    pub removed: usize,
    /// Достигнут лимит MAX_FILES — обход оборван.
    pub truncated: bool,
}

/// Индексатор: обходит корни и применяет результат к БД.
pub struct Indexer {
    db: Arc<Db>,
    roots: Vec<PathBuf>,
    max_files: usize,
}

impl Indexer {
    /// Индексатор с корнями по умолчанию (папки пользователя).
    pub fn new(db: Arc<Db>) -> Self {
        Indexer { db, roots: default_roots(), max_files: MAX_FILES }
    }

    /// Индексатор с заданными корнями (тесты, будущие плагины).
    pub fn with_roots(db: Arc<Db>, roots: Vec<PathBuf>) -> Self {
        Indexer { db, roots, max_files: MAX_FILES }
    }

    /// Переопределить лимит файлов (тесты).
    pub fn with_max_files(mut self, max_files: usize) -> Self {
        self.max_files = max_files;
        self
    }

    /// Полный рескан: пишет все найденные файлы безусловно.
    pub fn run_full(&self, progress: Option<&Sender<IndexProgress>>) -> super::db::Result<IndexStats> {
        self.run(ApplyMode::Full, progress)
    }

    /// Инкрементальный прогон: только изменения mtime + сверка удалённых.
    pub fn run_incremental(
        &self,
        progress: Option<&Sender<IndexProgress>>,
    ) -> super::db::Result<IndexStats> {
        self.run(ApplyMode::Incremental, progress)
    }

    fn run(
        &self,
        mode: ApplyMode,
        progress: Option<&Sender<IndexProgress>>,
    ) -> super::db::Result<IndexStats> {
        let send = |phase: IndexPhase, done: u64, total: u64| {
            if let Some(tx) = progress {
                let _ = tx.send(IndexProgress { phase, done, total }); // приёмник мог отпасть
            }
        };

        send(IndexPhase::Scanning, 0, 0);
        let (files, truncated) = scan(&self.roots, self.max_files, progress);
        let discovered = files.len();

        send(IndexPhase::Indexing, 0, discovered as u64);
        let prog = |done: u64, total: u64| send(IndexPhase::Indexing, done, total);
        let stats = self.db.apply_files(&files, mode, &prog)?;

        // Сверка удалённых: только если обход не оборван лимитом, иначе
        // «недошитые» записи ложно сочлись бы удалёнными.
        let mut removed = 0usize;
        if !truncated {
            send(IndexPhase::Cleaning, 0, 0);
            let visited: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
            for path in self.db.list_paths()? {
                if !visited.contains(path.as_str()) && self.db.delete_file(&path)? {
                    removed += 1;
                }
            }
        }

        send(IndexPhase::Done, discovered as u64, discovered as u64);
        Ok(IndexStats {
            discovered,
            indexed: stats.applied,
            unchanged: stats.unchanged,
            removed,
            truncated,
        })
    }
}

/// Корневые папки индексации: шесть пользовательских каталогов из %USERPROFILE%.
pub fn default_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    ["Desktop", "Documents", "Downloads", "Pictures", "Videos", "Music"]
        .iter()
        .map(|d| home.join(d))
        .filter(|p| p.is_dir())
        .collect()
}

/// Каталоги, которые никогда не обходим (§2 плана), регистронезависимо.
fn is_excluded_dir(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), ".git" | "node_modules" | "appdata")
}

/// Обход корней: файлы с метаданными; `truncated` — достигнут лимит.
/// Ошибки чтения (права/гонки) — skip+лог, не падение.
fn scan(
    roots: &[PathBuf],
    max_files: usize,
    progress: Option<&Sender<IndexProgress>>,
) -> (Vec<FileRecord>, bool) {
    let mut files: Vec<FileRecord> = Vec::new();
    let mut truncated = false;
    // Стек (каталог, глубина): корень = 0, спуск пока глубина ребёнка ≤ MAX_DEPTH.
    let mut stack: Vec<(PathBuf, usize)> =
        roots.iter().filter(|r| r.is_dir()).map(|r| (r.clone(), 0)).collect();

    'outer: while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                logging::warn(&format!("indexer: недоступен каталог {}: {e}", dir.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue }; // гонки/права — пропустить
            let path = entry.path();
            if meta.is_dir() {
                let name = entry.file_name();
                if is_excluded_dir(&name.to_string_lossy()) {
                    continue;
                }
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            if files.len() >= max_files {
                truncated = true;
                logging::warn(&format!(
                    "indexer: лимит {max_files} файлов достигнут, обход оборван"
                ));
                break 'outer;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().into_owned());
            files.push(FileRecord {
                path: path.to_string_lossy().into_owned(),
                name: entry.file_name().to_string_lossy().into_owned(),
                ext,
                mtime,
                size: meta.len() as i64,
            });
            if files.len() % PROGRESS_EVERY == 0 {
                if let Some(tx) = progress {
                    let _ = tx.send(IndexProgress {
                        phase: IndexPhase::Scanning,
                        done: files.len() as u64,
                        total: 0,
                    });
                }
            }
        }
    }
    (files, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    use crate::search::db::unique_temp_dir;

    /// Дерево из 3 файлов: доклад.pdf, заметки.txt, sub/Отчёт ёлка.txt.
    fn make_tree(root: &std::path::Path) {
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("доклад.pdf"), b"pdf").unwrap();
        fs::write(root.join("заметки.txt"), b"txt").unwrap();
        fs::write(root.join("sub").join("Отчёт ёлка.txt"), b"txt").unwrap();
    }

    /// Полный скан пишет всё; инкрементальный второй проход — 0 изменений.
    #[test]
    fn full_scan_then_incremental_indexes_zero() {
        let dir = unique_temp_dir("indexer-incr");
        make_tree(&dir);

        let db = Arc::new(Db::open_in_memory().unwrap());
        let indexer = Indexer::with_roots(db.clone(), vec![dir.clone()]);

        let stats = indexer.run_full(None).unwrap();
        assert_eq!(
            (stats.discovered, stats.indexed, stats.unchanged, stats.removed, stats.truncated),
            (3, 3, 0, 0, false)
        );
        assert_eq!(db.count_files().unwrap(), 3);

        // инкремент: ничего не менялось → 0 записей, 3 пропущено
        let stats = indexer.run_incremental(None).unwrap();
        assert_eq!(
            (stats.discovered, stats.indexed, stats.unchanged, stats.removed),
            (3, 0, 3, 0),
            "второй проход индексирует 0"
        );
        assert_eq!(db.count_files().unwrap(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    /// Инкремент подхватывает изменение mtime и удаление файла.
    #[test]
    fn incremental_picks_mtime_change_and_removal() {
        let dir = unique_temp_dir("indexer-mtime");
        make_tree(&dir);

        let db = Arc::new(Db::open_in_memory().unwrap());
        let indexer = Indexer::with_roots(db.clone(), vec![dir.clone()]);
        indexer.run_full(None).unwrap();

        // изменили содержимое и выставили явный старый mtime (детерминизм
        // без sleep: реальный mtime файла — «сейчас», ставим 2009 год)
        let changed = dir.join("доклад.pdf");
        let f = fs::OpenOptions::new().append(true).open(&changed).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_234_567_890))
            .unwrap();
        drop(f);

        // удалили один файл
        fs::remove_file(dir.join("заметки.txt")).unwrap();

        let stats = indexer.run_incremental(None).unwrap();
        assert_eq!(stats.indexed, 1, "изменённый файл переписан");
        assert_eq!(stats.unchanged, 1, "sub/Отчёт ёлка.txt не тронут");
        assert_eq!(stats.removed, 1, "удалённый файл вычищен из индекса");
        assert_eq!(db.count_files().unwrap(), 2);
        assert!(db.search_files("док", 10).unwrap().len() == 1);
        assert!(db.search_files("заметки", 10).unwrap().is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    /// Глубина ≤ 5, excludes, лимит файлов (truncation запрещает сверку удалённых).
    #[test]
    fn depth_excludes_and_limit() {
        let dir = unique_temp_dir("indexer-limits");
        // глубина 5 — индексируется: root/a/b/c/d/e (e на глубине 5)
        let deep = dir.join("a").join("b").join("c").join("d").join("e");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("на-глубине-5.txt"), b"x").unwrap();
        // глубина 6 — НЕТ
        let too_deep = deep.join("f");
        fs::create_dir_all(&too_deep).unwrap();
        fs::write(too_deep.join("слишком-глубоко.txt"), b"x").unwrap();
        // excludes
        fs::create_dir_all(dir.join("node_modules")).unwrap();
        fs::write(dir.join("node_modules").join("pkg.js"), b"x").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git").join("config"), b"x").unwrap();
        fs::write(dir.join(".gitignore"), b"x").unwrap(); // ФАЙЛ с таким именем — индексируется

        let db = Arc::new(Db::open_in_memory().unwrap());
        let indexer = Indexer::with_roots(db.clone(), vec![dir.clone()]);
        let stats = indexer.run_full(None).unwrap();
        assert_eq!(stats.indexed, 2, "на-глубине-5.txt + .gitignore");
        assert!(db.search_files("слишком", 10).unwrap().is_empty(), "глубина 6 не берётся");
        assert!(db.search_files("pkg", 10).unwrap().is_empty(), "node_modules исключён");
        assert!(db.search_files("config", 10).unwrap().is_empty(), ".git исключён");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Лимит файлов: truncation-флаг, обход оборван, сверка удалённых не гоняется
    /// (иначе «недошитые» записи ложно удалялись бы).
    #[test]
    fn file_limit_truncates_and_skips_removal_check() {
        let dir = unique_temp_dir("indexer-limit");
        make_tree(&dir); // 3 файла

        let db = Arc::new(Db::open_in_memory().unwrap());
        let indexer = Indexer::with_roots(db.clone(), vec![dir.clone()]).with_max_files(2);
        let stats = indexer.run_full(None).unwrap();
        assert!(stats.truncated);
        assert_eq!(stats.indexed, 2);
        assert_eq!(db.count_files().unwrap(), 2);

        // повторный полный прогон при обрыве НЕ удаляет существующие записи
        let stats = indexer.run_full(None).unwrap();
        assert!(stats.truncated);
        assert_eq!(stats.removed, 0);
        assert_eq!(db.count_files().unwrap(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    /// Прогресс приходит через mpsc: Scanning → Indexing → ... → Done.
    #[test]
    fn progress_via_mpsc_channel() {
        let dir = unique_temp_dir("indexer-progress");
        make_tree(&dir);

        let db = Arc::new(Db::open_in_memory().unwrap());
        let indexer = Indexer::with_roots(db, vec![dir.clone()]);
        let (tx, rx) = std::sync::mpsc::channel();
        indexer.run_full(Some(&tx)).unwrap();
        drop(tx); // закрываем: recv() вернёт Err после исчерпания

        let mut phases = Vec::new();
        while let Ok(p) = rx.recv() {
            assert!(p.done <= p.total.max(1), "done не обгоняет total: {p:?}");
            phases.push(p.phase);
        }
        assert_eq!(phases.first(), Some(&IndexPhase::Scanning));
        assert_eq!(phases.last(), Some(&IndexPhase::Done));
        assert!(phases.contains(&IndexPhase::Indexing));

        let _ = fs::remove_dir_all(&dir);
    }

    /// Кириллица/ё в именах файлов доходят до FTS-поиска (риск 8, сквозняк).
    #[test]
    fn cyrillic_names_searchable_after_indexing() {
        let dir = unique_temp_dir("indexer-cyr");
        make_tree(&dir);

        let db = Arc::new(Db::open_in_memory().unwrap());
        let indexer = Indexer::with_roots(db.clone(), vec![dir.clone()]);
        indexer.run_full(None).unwrap();

        assert_eq!(db.search_files("док", 10).unwrap().len(), 1, "«док» → «доклад.pdf»");
        assert_eq!(db.search_files("ЁЛК", 10).unwrap().len(), 1, "ё/регистр в запросе");
        assert_eq!(db.search_files("Замет", 10).unwrap().len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
