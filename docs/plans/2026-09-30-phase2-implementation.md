# План реализации Фазы 2 «Ядро поиска» — Iskra

> **Дата:** 2026-09-30
> **Основание:** [docs/specs/2026-09-17-windows-raycast-clone-spec.md](../specs/2026-09-17-windows-raycast-clone-spec.md)
> (§4.2 решения 5/6, §4.3 модель данных), [docs/specs/2026-09-17-windows-raycast-clone-plan.md](../specs/2026-09-17-windows-raycast-clone-plan.md) (Фаза 2),
> [docs/plans/2026-09-29-phase1-implementation.md](2026-09-29-phase1-implementation.md) (скелет, принят 2026-09-30).
> **Из фазы 1 перенесено:** мульти-монитор/150% — N/A на текущем железе (проверить, если появится).
> **Бюджет:** 2 прогона воркера. `spikes/` не трогаем.

## Goal

Лончером можно пользоваться ежедневно без плагинов: по запросу — приложения, файлы из
папок пользователя, настройки Windows (ms-settings:), калькулятор/конвертер, системные
команды, fallback «искать в интернете»; по пустому запросу — недавние/частые.
Критерии: поиск < 20 мс p95 на 10 000 записей; приложения запускаются; файлы находятся
по обрывку имени; скролл списка 60 FPS; RAM остаётся ≤ 80 МБ после индексации.

---

## 1. Архитектурные решения (в развитие спеки)

| # | Вопрос | Решение | План Б |
|---|--------|---------|--------|
| D1 | Где живёт поиск | `iskra-core`: fuzzy-движок, trait `SearchProvider`, агрегатор, ранжирование, БД-слой (rusqlite), индексатор (std::fs). `iskra-sys`: .lnk-RESOLVE цели, иконки, запуск, питание. `app`: только склейка (сервис + команды) | — |
| D2 | SQLite | `rusqlite` с feature `bundled` (без системной sqlite3), FTS5 поверх `file_index`. Проверить в шаге 0, что bundled-сборка содержит FTS5 (`CREATE VIRTUAL TABLE ... USING fts5`) | флаг `SQLITE_ENABLE_FTS5` через `bundled` уже включён в libsqlite3-sys; если нет — `--cfg` feature или hand-CRT миграции отката |
| D3 | Миграции | `refinery` (embedded, feature rusqlite) — по решению 6 спеки; БД `%APPDATA%\iskra\index.db`, WAL | hand-rolled `PRAGMA user_version` (для 5 таблиц refinery может оказаться избыточен — решение фиксируем в шаге 2 отчётом) |
| D4 | Парсинг .lnk | крейт `parselnk` (pure Rust, без COM) в `iskra-core` | COM `IShellLink` в `iskra-sys` (нужен STA-поток) |
| D5 | Калькулятор | `meval` для выражений + собственный парсер конвертации единиц (регулярка `N unit in|to unit`) поверх таблицы коэффициентов (длина, масса, температура, объём, скорость, данные, время). Валюты — НЕТ (без сети, по спеке) | hand-rolled shunting-yard вместо meval |
| D6 | UWP/Store-приложения | MVP — только .lnk из Start Menu (ProgramData + APPDATA) и рабочий стол. `shell:AppsFolder` через COM — в фазу 3 | — |
| D7 | Иконки в UI | извлечение HICON → PNG (`image` 0.25) в `%APPDATA%\iskra\icons\{hash}.png`; раздача через `assetProtocol` (scope `$APPDATA/iskra/icons/*`) + `convertFileSrc` | custom protocol handler в Rust |
| D8 | Асинхронность | `search(q)` возвращает быстрые провайдеры сразу (< 20 мс); файловые/медленные результаты — событием `search://updated` с `queryId` (UI отбрасывает устаревшие) | всё через события |
| D9 | Реал-тайм вотчер ФС (notify) | НЕТ в фазе 2: рескан по mtime на старте + интервал 30 мин. Вотчер — фаза 3+ | — |

Формула ранжирования (одна точка — `ranking.rs`, веса тюнятся там):
`score = fuzzy(0..1000) + provider_priority + min(used_count,50)*2 + recency_boost`,
приоритеты: apps 100, calc 90 (при матче), settings 80, system 70, files 50, web-fallback 10.

## 2. Раскладка репозитория (новое/изменяемое)

```
crates/iskra-core/src/
├── search/
│   ├── mod.rs            # re-exports
│   ├── fuzzy.rs          # fzf-like score: boundary/camelCase/подстрока, корпус-тесты
│   ├── provider.rs       # trait SearchProvider { name, priority, query(&self, q) -> Vec<SearchItem> }
│   ├── types.rs          # SearchItem { id, provider, title, subtitle?, icon_path?, score, action }, ItemAction
│   ├── aggregator.rs     # merge+sort+лимит 50, id-реестр для run_item, пустой запрос → recents
│   ├── ranking.rs        # формула (см. §1), единственная точка весов
│   ├── calc.rs           # meval + конвертер единиц (D5)
│   ├── settings_win.rs   # топ-50 ms-settings: URI (title RU/EN keywords)
│   ├── web.rs            # fallback: URL браузера по умолчанию (Action::WebSearch)
│   ├── apps.rs           # обход Start Menu (.lnk), parselnk → title/target/args/icon index (D4/D6)
│   ├── db.rs             # rusqlite bundled + refinery: file_index, file_index_fts(FTS5), usage
│   └── indexer.rs        # walker (Desktop/Documents/Downloads/Pictures/Videos/Music,
│                         #   глубина ≤ 5, exclude .git|node_modules|AppData|*, лимит 50k),
│                         #   mtime-инкремент, прогресс через канал
└── ipc.rs                # + SearchItem/SearchResponse/SearchError, run_item, события
                          #   search://updated {queryId, items}, index://progress {phase, done, total}
crates/iskra-sys/src/
├── shell.rs              # ShellExecuteW: launch app/файл/URI (ms-settings:, https:)
├── icons.rs              # SHGetFileInfoW/IShellItemImageFactory → HBITMAP → PNG (image) в кэш
└── power.rs              # LockWorkStation, SetSuspendState (sleep), ExitWindowsEx(+привилегия),
                          #   SC_MONITORPOWER, SHEmptyRecycleBin
app/src/
├── search_service.rs     # поток БД (одно соединение), агрегатор, queryId, usage++, события
├── commands.rs           # + search, run_item, get_index_status, reindex
├── bench.rs              # + --bench-search N: корпус 10k, p95 запросов → latency-search.log
└── main.rs               # инициализация SearchService + фоновый индекс-воркер
ui/src/
├── components/ResultList.tsx   # РЕАЛЬНЫЙ список: input, клавиатура ↑↓ Enter Esc, иконки
│                               #   (convertFileSrc), debounce 50 мс, merge search://updated
├── ipc/types.ts, client.ts     # зеркало ipc.rs
└── theme.css                   # стили результатов (hover/selected, иконки 20×20)
```

Принципы сохраняются: core без Tauri/WinAPI (тестируется быстро), sys — весь unsafe,
app — склейка. `tauri.conf.json`: + assetProtocol scope (D7).

## 3. Шаги реализации

**Бюджет: 2 прогона воркера.** Прогон 1 = шаги 0–3, прогон 2 = шаги 4–6.

### Шаг 0. `[S]` Пре-чек зависимостей (внутри шага 1)
Проверить сборкой: `rusqlite` (bundled) FTS5 доступен, `refinery`+rusqlite совместимы,
`parselnk`, `meval`, `image` собираются под vcvars64; зафиксировать выбранные версии в отчёте.
- Done when: тест `CREATE VIRTUAL TABLE ... USING fts5` зелёный; версии в отчёте.

### Шаг 1. `[P]` Ядро поиска: fuzzy + trait + агрегатор + calc/settings/web
Файлы: `search/{fuzzy,provider,types,aggregator,ranking,calc,settings_win,web}.rs`, тесты.
- Fuzzy: score 0..1000 (точное совпадение > границы слов > camelCase > подстрока > подпоследовательность),
  корпус-тесты ≥ 20 пар «запрос/ожидаемый порядок» (RU и EN, включая «хрм»→«Хром»).
- Calc: выражения (`2+2*2`, `sqrt(2)`, `sin(pi/2)`), конвертер (`10 km in mi`, `72 f to c`);
  юнит-тесты; запрос-детект: начинается с цифры/скобки и валиден — иначе провайдер молчит.
- Settings: статический список топ-50 ms-settings: URI с RU/EN keywords.
- Aggregator: merge по `ranking.rs`, dedupe по (provider,title), лимит 50, `query("")` →
  recents из usage-таблицы.
- Done when: `cargo test -p iskra-core` зелёный, включая корпус и bench-тест
  «10 000 записей, p95 < 20 мс» (`#[ignore]`-тест в release: `cargo test -r -- --ignored`).

### Шаг 2. `[P]` БД + индексатор (нужен шаг 1 — типы)
Файлы: `search/{db,indexer,apps}.rs`.
- Схема (решение 6, §4.3 спеки): `file_index(path PK, name, ext, mtime, size)` +
  `file_index_fts` (FTS5, content = name, external content), `usage(id PK, used_count, used_at)`.
- Индексатор: старт — полный скан в фоне (канал прогресса), далее mtime-инкремент на старте
  и раз в 30 мин; excludes и лимиты из §2.
- Apps: обход двух Start Menu + Desktop, parselnk → (title, target, args, icon_path).
- Done when: тесты db (миграции на temp-файле, roundtrip, FTS5-поиск по обрывку «док»→«доклад.pdf»)
  и indexer (tmp-дерево, инкремент: второй проход индексирует 0) зелёные.

### Шаг 3. `[P]` iskra-sys: shell, иконки, питание
Файлы: `sys/{shell,icons,power}.rs`, тесты где безопасно.
- Shell: запуск exe/файла/URI через ShellExecuteW (включая `ms-settings:` и `https:`).
- Icons: HICON/HBITMAP → PNG → кэш-файл `{hash}.png`; тест: иконка notepad.exe извлекается в tmp.
- Power: lock, monitor-off, sleep, restart/shutdown (+AdjPriv), empty bin.
  Автотесты — только lock-БЕЗ-вызова (проверка сигнатур) и empty bin на пустой корзине опционально;
  остальные — ручная приёмка.
- Done when: `cargo test -p iskra-sys` зелёный; иконка извлечена, ShellExecute открывает notepad (тест).

### Шаг 4. `[S]` App-сервисы + IPC (нужны 1–3)
Файлы: `search_service.rs`, `commands.rs`, `ipc.rs` + зеркало `ui/src/ipc/*`, `main.rs`, `tauri.conf.json`.
- Контракт: `search(q) -> SearchResponse`, `run_item(id) -> Result<(), SearchError>`
  (usage++ и выполнение Action), `get_index_status() -> IndexStatus`, событие `search://updated`,
  `index://progress` (camelCase, roundtrip-тесты в ipc.rs).
- SearchService: БД-поток (одно соединение, WAL), queryId-генерация, «медленные» результаты —
  эмитом события; Action-исполнение через sys.
- `tauri.conf.json`: assetProtocol scope `$APPDATA/iskra/icons/*` (D7).
- Done when: `build.cmd` зелёный; вручную: Alt+Space → ввод «хрм» → Хром в топе; Enter запускает.

### Шаг 5. `[P]` UI: реальный список (нужен шаг 4)
Файлы: `ResultList.tsx`, `types.ts`, `client.ts`, `theme.css`.
- Input с debounce 50 мс; пустой запрос → recents; клавиатура: ↑↓ Enter Esc (Esc — скрыть окно
  через новое событие/команду — минорное расширение контракта, согласовать в шаге 4);
  иконки через convertFileSrc; merge `search://updated` по queryId; статусбар — прогресс индекса.
- Скролл 60 FPS: обычный рендер до ~50 строк; виртуализация НЕ нужна (лимит 50) — риск 5.
- Done when: `npm run build` зелёный; ручной чек §5.

### Шаг 6. `[S]` Bench + приёмка (после 4–5, release)
Файлы: `bench.rs` (+`--bench-search N`), README-секция.
- Done when: p95 поиска < 20 мс на 10k (скрипт, nearest-rank); RAM ≤ 80 МБ после индексации
  (методика RESULTS.md); чек-лист §5 закрыт.

## 4. Риски и план Б

| # | Риск | План Б |
|---|------|--------|
| 1 | rusqlite bundled без FTS5 под vcvars | включить FTS5-флаг сборки; крайний случай — LIKE-режим с триграммами (медленнее, но работает) |
| 2 | .lnk-кворки (ADS, UNC, кириллица) | parselnk не берёт → COM IShellLink; отдельные битые .lnk — skip+лог, не падение |
| 3 | Extract иконок: HBITMAP→PNG квирки (альфа-канал) | `IShellItemImageFactory::GetImage` (32-bit BGRA premultiplied) + `image` PNG; проверка визуально на 3 приложениях в шаге 3 |
| 4 | ExitWindowsEx без привилегии молча не срабатывает | AdjPriv SE_SHUTDOWN_NAME в power.rs; ручная проверка в приёмке (по желанию пользователя) |
| 5 | Скролл/перерисовка < 60 FPS при обновлениях от событий | memo-компоненты, батчинг merge по rAF; крайний случай — виртуализация (react-window) |
| 6 | Рост RAM от in-memory индекса приложений/настроек | измерение в шаге 6; ожидаемо +2–5 МБ на ~10k записей; если больше — перенос file-провайдера целиком в FTS5 (без in-memory дубля) |
| 7 | refinery тянет тяжёлые зависимости (syn и пр. — только build-time) | hand-rolled user_version миграции (5 таблиц — дёшево) |
| 8 | Кириллица в FTS5 (token unicode61) — поиск по «док» находит «доклад» | да; но регистр/ё — нормализовать в name при записи; тест в шаге 2 |
| 9 | Сон/перезагрузка из лончера опасны при ложном Enter | системные команды требуют ≥ 2 символов запроса («lock», «сон») и подтверждения Enter — без автозапуска по одному символу |

## 5. Верификация (чек-лист приёмки против критерия фазы)

Сборка: `build.cmd` → `target/release/iskra.exe` (release 40 с, exe 13,6 МБ).

- [x] **Поиск < 20 мс p95 на 10k записей**: `iskra.exe --bench-search 200` (release),
      nearest-rank по `latency-search.log`; фактически p95 = 8,68 мс (nearest-rank,
      n=200, корпус 10k) — PASS, запас 2,3× (после фикса сортировки bench-вывода
      повтор на n=100: 7,15 мс);
- [x] **Приложения запускаются**: запуск из поиска — подтверждено пользователем (2026-09-30);
- [x] **Файлы по обрывку имени**: RU- и EN-фрагменты из папок пользователя —
      подтверждено пользователем;
- [x] **Пустой запрос**: recents по пустому запросу — подтверждено пользователем;
- [x] **Калькулятор/конвертер**: юнит-тесты зелёные + manual — подтверждено пользователем;
- [x] **ms-settings**: URI открываются — подтверждено пользователем;
- [x] **Системные команды**: lock и monitor-off — подтверждено пользователем;
- [x] **Fallback web**: обрывок без результатов → «искать в интернете» → браузер —
      подтверждено пользователем;
- [x] **Скролл 60 FPS**: плавный скролл списка без дёрганий — подтверждено пользователем;
- [x] **RAM ≤ 80 МБ** после полной индексации (private WS, методика RESULTS.md):
      70,1 МБ — PASS;
- [x] **Индекс-прогресс** виден в статусбаре — работает; рестарт — инкрементально:
      indexed=0 за 13 мс — PASS;
- [x] Мульти-монитор/150% (перенос из фазы 1) — N/A на текущем железе (единственный
      монитор 1920×1080@100%), перенесено в фазу 3.

**Примечание (2026-09-30):** авто-часть прогнана скриптом (bench, RAM, инкремент-рестарт,
логи: ERROR в runtime.log — 0), ручные проверки закрыты пользователем 2026-09-30.
Релиз собирать только `build.cmd` (прямой `cargo build` без `--features custom-protocol`
даёт dev-режим с devUrl → ERR_CONNECTION_REFUSED в окне); при замене трей-иконки
нужен `touch app/src/tray.rs` (cargo не отслеживает PNG внутри `include_image!`).

## 6. Распределение по прогонам воркера

- **Прогон 1:** шаг 0–1, параллельно 2 и 3 (контракт типов из шага 1 — вход для 2).
- **Прогон 2:** шаг 4 → (параллельно 5) → шаг 6 + чек-лист; чекбоксы — вручную после приёмки.
