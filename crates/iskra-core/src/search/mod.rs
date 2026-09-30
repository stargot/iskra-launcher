//! Ядро поиска Iskra (план Ф2, §2): fuzzy-скоринг, трейт провайдера, агрегатор,
//! ранжирование, калькулятор, настройки Windows, web-fallback, БД-слой (db.rs),
//! индексатор файлов (indexer.rs) и приложения из .lnk (apps.rs) — шаг 2.
//!
//! Границы крейта не нарушаются: никаких Tauri/WinAPI/unsafe — исполнение
//! действий в iskra-sys (шаг 3), склейка в app (шаг 4).
//!
//! Здесь же — смоук-тесты шага 0 (пре-чек зависимостей сборкой под MSVC):
//! rusqlite bundled + FTS5 (D2), refinery поверх rusqlite (D3), image/PNG (D7).

pub mod aggregator;
pub mod apps;
pub mod calc;
pub mod db;
pub mod fuzzy;
pub mod indexer;
pub mod provider;
pub mod ranking;
pub mod settings_win;
pub mod system;
pub mod types;
pub mod web;

pub use aggregator::{Aggregator, NoUsage, UsageStore};
pub use apps::{AppEntry, AppsProvider, ScanReport, default_app_dirs, scan_apps, scan_apps_report};
pub use calc::{CalcAnswer, CalcProvider};
pub use db::{normalize_name, ApplyMode, ApplyStats, Db, DbError, FileRecord, FilesProvider};
pub use fuzzy::{score, MAX_SCORE, MIN_MEANINGFUL};
pub use indexer::{IndexPhase, IndexProgress, IndexStats, Indexer};
pub use provider::SearchProvider;
pub use ranking::total_score;
pub use settings_win::SettingsProvider;
pub use system::SystemProvider;
pub use types::{ItemAction, SearchItem, SystemCommand};
pub use web::WebProvider;

#[cfg(test)]
mod step0 {
    //! Шаг 0: зависимости собраны и совместимы под vcvars64 (MSVC).

    // Миграции лежат в crates/iskra-core/migrations (относительно CARGO_MANIFEST_DIR);
    // макрос генерирует mod migrations с функцией runner() -> Runner.
    mod embedded {
        use refinery::embed_migrations;
        embed_migrations!("migrations");
    }

    /// D2 + D3: bundled-сборка sqlite содержит FTS5, refinery прогоняет
    /// embedded-миграцию на rusqlite-соединении, unicode61 режет кириллицу
    /// на токены, префикс-поиск «док*» находит «доклад.pdf» (основа теста шага 2).
    #[test]
    fn rusqlite_bundled_fts5_and_refinery_work() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");

        let report = embedded::migrations::runner().run(&mut conn).expect("refinery: миграции применились");
        // Шаг 2 добавил V2__search_index.sql — проверяем «применились все», число не фиксируем.
        assert!(
            !report.applied_migrations().is_empty(),
            "V1__baseline и последующие миграции применены ровно один раз"
        );
        // повторный прогон — идемпотентен (базовое свойство, нужное шагу 2)
        let again = embedded::migrations::runner().run(&mut conn).expect("повторный прогон");
        assert_eq!(again.applied_migrations().len(), 0, "повторно — ничего не применяется");

        conn.execute_batch(
            "CREATE VIRTUAL TABLE smoke_fts USING fts5(name, tokenize='unicode61');
             INSERT INTO smoke_fts(name) VALUES ('доклад.pdf'), ('отчет.docx');",
        )
        .expect("FTS5 должен быть собран в bundled sqlite (D2)");

        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM smoke_fts WHERE smoke_fts MATCH 'док*'",
                [],
                |r| r.get(0),
            )
            .expect("prefix-запрос");
        assert_eq!(n, 1, "FTS5: «док*» находит «доклад.pdf», но не «отчет.docx»");
    }

    /// D7: image кодирует PNG (линковка и работа под MSVC; реальное
    /// использование иконок — iskra-sys/icons.rs в шаге 3).
    #[test]
    fn image_png_encode_works() {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255u8, 0, 0, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).expect("PNG-кодирование");
        let bytes = buf.into_inner();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG-сигнатура");
        assert!(bytes.len() > 20, "не пустой файл: {} байт", bytes.len());
    }
}
