# Iskra

Лончер в духе Raycast для Windows 11. Tauri 2 (Rust) + TypeScript/React, плагины out-of-process.

- Спека: `docs/specs/2026-09-17-windows-raycast-clone-spec.md`
- План: `docs/specs/2026-09-17-windows-raycast-clone-plan.md`

> Рабочий каталог `wrc` — историческое имя (Windows Raycast Clone), продукт — **Iskra**.

## Сборка

Требуется Windows: Rust (MSVC), Node ≥ 20.19, VS2022 + Windows SDK 10.0.26100 (vcvars64).

- `build.cmd` — всё сразу: vcvars64 → сборка UI, если нет `ui/dist` (npm install + npm run build) → `cargo build --release`. Артефакт: `target/release/iskra.exe`.
- UI отдельно: `npm --prefix ui install`; `npm --prefix ui run build` (прод) или `npm --prefix ui run dev` (dev-сервер :5173 для `cargo run` из `app/`).
- Тесты: `cargo test -p iskra-core -p iskra-sys` (из среды vcvars64).
- Cargo-крейты: `app` (бинарник `iskra`, Tauri-шелл) + `crates/iskra-core` (чистая логика) + `crates/iskra-sys` (обёртки WinAPI).

## Поиск (Фаза 2)

По запросу — приложения (Start Menu + рабочий стол), файлы из пользовательских папок
(индекс SQLite/FTS5, рескан на старте и раз в 30 мин), настройки Windows (`ms-settings:`),
калькулятор/конвертер единиц, системные команды (блокировка/сон/перезагрузка/… — только
при запросе от 2 символов) и fallback «искать в интернете». Пустой запрос — недавние/частые
(usage считается по запускам; снимок переживает рестарт).

Асинхронность (D8): быстрые провайдеры отвечают синхронно (< 20 мс), файловые результаты
приходят событием `search://updated` с `queryId` — UI отбрасывает устаревшие. Иконки (D7)
извлекаются лениво в кэш `%APPDATA%\iskra\icons\{hash}.png` и раздаются через `assetProtocol`
(`convertFileSrc`). Прогресс индекса — событие `index://progress` (статусбар UI).

Клавиатура: ↑↓ — выбор, Enter — запуск, Esc — скрыть окно.

## Бенчмарк поиска

`iskra.exe --bench-search 200` (release, из `build.cmd`-окружения) — микробенчмарк ядра
без Tauri/окна: синтетический корпус 10 000 записей in-memory, 200 запросов из фиксированного
списка (RU/EN, целые слова и обрывки). Микросекунды на запрос пишутся в
`%APPDATA%\iskra\logs\latency-search.log` (по одному числу в строке), итог (min/median/p95 по
nearest-rank) печатается в stdout и `runtime.log`; приложение завершается с кодом 0.

Критерий фазы: p95 < 20 000 µs на корпусе 10k (замер 2026-09-30: p95 ≈ 8 100 µs).
Percentile считается nearest-rank: `rank = ceil(p/100 · N)`. Приёмочные замеры (RAM,
скролл 60 FPS, запуск приложений) — по чек-листу §5 плана фазы.
