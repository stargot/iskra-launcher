//! Bench-режимы (порты из spikes/tauri-app + шаг 6 Фазы 2):
//! - `--bench N` — латентность показа окна (хоткей → position+show+focus),
//!   микросекунды в logs/latency.log; по завершении приложение завершается (код 0);
//! - `--bench-search N` — микробенчмарк ядра поиска: синтетический корпус 10 000
//!   записей in-memory, N запросов из фиксированного списка (RU/EN, обрывки),
//!   микросекунды на запрос → logs/latency-search.log, p95 (nearest-rank) — в
//!   stdout и runtime.log. Tauri/окно не запускаются — только ядро iskra-core.

use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use iskra_core::logging;
use iskra_core::search::aggregator::RESULT_LIMIT;
use iskra_core::search::{
    Aggregator, CalcProvider, ItemAction, NoUsage, SearchItem, SearchProvider, SettingsProvider,
    SystemProvider, WebProvider,
};

/// Прогрев перед первым нажатием: даём webview загрузиться.
const WARMUP_MS: u64 = 700;
/// Пауза между итерациями (окно успевает скрыться).
const ITER_PAUSE_MS: u64 = 60;
/// Таймаут ожидания обработки нажатия.
const ITER_TIMEOUT_SECS: u64 = 3;

/// Разобрать `--bench N` из аргументов командной строки.
pub fn parse_bench_arg() -> Option<usize> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--bench")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<usize>().ok())
}

/// Разобрать `--bench-search N` из аргументов командной строки.
pub fn parse_bench_search_arg() -> Option<usize> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--bench-search")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<usize>().ok())
}

/// Запустить bench-поток (вызывается из setup после manage состояния).
pub fn spawn(app: AppHandle, n: usize, rx: Receiver<u64>) {
    std::thread::spawn(move || run(app, n, rx));
}

fn run(app: AppHandle, n: usize, rx: Receiver<u64>) {
    std::thread::sleep(Duration::from_millis(WARMUP_MS));
    logging::info(&format!("bench start n={n}"));
    let win = app.get_webview_window("main");
    for i in 0..n {
        iskra_sys::keys::send_ctrl_alt_f9();
        match rx.recv_timeout(Duration::from_secs(ITER_TIMEOUT_SECS)) {
            Ok(delta_us) => logging::info(&format!("bench iter {i}: {delta_us} µs")),
            Err(err) => {
                logging::warn(&format!("bench iter {i}: recv err {err}"));
                break;
            }
        }
        // Прячем окно: каждая итерация мерит путь hidden → shown (реальный use-case).
        if let Some(w) = &win {
            let _ = w.hide();
        }
        std::thread::sleep(Duration::from_millis(ITER_PAUSE_MS));
    }
    logging::info("bench done");
    app.exit(0);
}

// --- `--bench-search N`: микробенчмарк ядра поиска (шаг 6) ---

/// Размер синтетического корпуса (критерий фазы: p95 < 20 мс на 10 000 записей).
pub const BENCH_CORPUS: usize = 10_000;

/// Фиксированный список запросов: RU/EN, целые слова и обрывки (цикл по N прогонов).
const BENCH_QUERIES: [&str; 10] = [
    "док", "отчет", "хром", "замет", "бюдж 2024", "report", "meet", "photo 2025", "инв",
    "презентация",
];

/// Синтетический корпус: детерминированные RU/EN-заголовки с номерами и расширениями.
fn corpus(n: usize) -> Vec<String> {
    let ru = [
        "доклад",
        "отчет",
        "заметки",
        "презентация",
        "бюджет",
        "инструкция",
        "договор",
        "хром",
        "фото",
        "музыка",
    ];
    let en = [
        "report", "meeting", "photo", "invoice", "notes", "budget", "manual", "chrome",
        "download", "setup",
    ];
    let ext = ["pdf", "docx", "txt", "xlsx", "png", "mp3", "zip"];
    (0..n)
        .map(|i| {
            // i/2 — чтобы каждое слово обеих групп реально встречалось в корпусе
            let word = if i % 2 == 0 { ru[(i / 2) % ru.len()] } else { en[(i / 2) % en.len()] };
            format!("{word}-{i}-2024.{}", ext[i % ext.len()])
        })
        .collect()
}

/// Провайдер поверх корпуса: fuzzy-скор по заголовкам (как файловый провайдер,
/// но без БД — мерим именно конвейер поиска).
struct CorpusProvider {
    titles: Vec<String>,
    take: usize,
}

impl CorpusProvider {
    fn new(titles: Vec<String>) -> Self {
        CorpusProvider { titles, take: 20 }
    }
}

impl SearchProvider for CorpusProvider {
    fn name(&self) -> &str {
        "files"
    }

    fn priority(&self) -> i64 {
        iskra_core::search::ranking::PRIORITY_FILES
    }

    fn query(&self, q: &str) -> Vec<SearchItem> {
        if q.trim().is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(i64, usize)> = self
            .titles
            .iter()
            .enumerate()
            .map(|(i, t)| (iskra_core::search::fuzzy::score(q, t), i))
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(self.take)
            .map(|(s, i)| SearchItem {
                id: format!("files:{}", i),
                provider: "files".to_string(),
                title: self.titles[i].clone(),
                subtitle: None,
                icon_path: None,
                score: s,
                action: ItemAction::OpenPath { path: self.titles[i].clone() },
            })
            .collect()
    }
}

/// Прогон bench-поиска: корпус 10k, N запросов, лог µs, p95 nearest-rank в stdout.
/// Вызывается из main() ДО старта Tauri; вызывающий завершает процесс.
pub fn run_bench_search(n: usize) {
    let titles = corpus(BENCH_CORPUS);
    let files = CorpusProvider::new(titles);
    let agg = Aggregator::new(vec![
        Arc::new(files),
        Arc::new(CalcProvider),
        Arc::new(SettingsProvider),
        Arc::new(SystemProvider),
        Arc::new(WebProvider::default()),
    ]);

    // Прогрев: первая итерация греет аллокации/кэш — в замер не идёт.
    let _warm = agg.query(BENCH_QUERIES[0], &NoUsage);

    logging::reset("latency-search.log");
    let mut latencies_us: Vec<u64> = Vec::with_capacity(n);
    for i in 0..n {
        let q = BENCH_QUERIES[i % BENCH_QUERIES.len()];
        let t0 = Instant::now();
        let items = agg.query(q, &NoUsage);
        let us = t0.elapsed().as_micros() as u64;
        // Выдача не выбрасывается: препятствуем выкидыванию запроса оптимизатором.
        if items.len() > RESULT_LIMIT {
            logging::warn("bench-search: неожиданно большая выдача");
        }
        latencies_us.push(us);
        logging::append("latency-search.log", &format!("{us}"));
    }

    let min = latencies_us.iter().min().copied().unwrap_or(0);
    // nearest-rank ожидает отсортированный вектор (см. сигнатуру) — до фикса p95/median
    // в stdout брались из хронологического порядка и занижали хвост.
    let mut sorted_us = latencies_us.clone();
    sorted_us.sort_unstable();
    let p95 = percentile_nearest_rank(&sorted_us, 95);
    let median = percentile_nearest_rank(&sorted_us, 50);
    let summary = format!(
        "bench-search: n={n} corpus={BENCH_CORPUS} min={min}us median={median}us p95={p95}us (nearest-rank)"
    );
    println!("{summary}");
    logging::info(&summary);
}

/// Percentile по nearest-rank: rank = ceil(p/100 * N), элемент с номером rank.
fn percentile_nearest_rank(sorted: &[u64], p: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p * sorted.len() as u64) + 99) / 100; // ceil(p/100 * N) без float
    let rank = rank.clamp(1, sorted.len() as u64);
    sorted[(rank - 1) as usize]
}

#[cfg(test)]
mod bench_search_tests {
    use super::*;

    /// Корпус: ровно BENCH_CORPUS записей, все уникальные, RU/EN микс.
    #[test]
    fn corpus_size_and_uniqueness() {
        let c = corpus(BENCH_CORPUS);
        assert_eq!(c.len(), BENCH_CORPUS);
        let mut sorted = c.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), BENCH_CORPUS, "заголовки уникальны");
        assert!(c.iter().any(|t| t.contains("доклад")));
        assert!(c.iter().any(|t| t.contains("report")));
    }

    /// Nearest-rank на простых примерах (в т.ч. границы).
    #[test]
    fn nearest_rank_percentile() {
        assert_eq!(percentile_nearest_rank(&[], 95), 0);
        assert_eq!(percentile_nearest_rank(&[5], 95), 5);
        // 1..=100: p95 → rank 95 → значение 95
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile_nearest_rank(&v, 95), 95);
        assert_eq!(percentile_nearest_rank(&v, 50), 50);
        assert_eq!(percentile_nearest_rank(&v, 100), 100);
        // 1..=99: p95 → ceil(94.05)=95 → 95
        let v: Vec<u64> = (1..=99).collect();
        assert_eq!(percentile_nearest_rank(&v, 95), 95);
    }
}
