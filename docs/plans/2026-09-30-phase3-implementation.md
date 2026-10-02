# План реализации Фазы 3 «Клипборд-история + сниппеты» — Iskra

> **Дата:** 2026-09-30
> **Основание:** [docs/specs/2026-09-17-windows-raycast-clone-spec.md](../specs/2026-09-17-windows-raycast-clone-spec.md)
> (§4.2 решения 6/7, §4.3 схема `clipboard_entries`/`snippets`),
> [docs/specs/2026-09-17-windows-raycast-clone-plan.md](../specs/2026-09-17-windows-raycast-clone-plan.md) (Фаза 3),
> [docs/plans/2026-09-30-phase2-implementation.md](2026-09-30-phase2-implementation.md) (поиск принят, Checkpoint 2 — go).
> **Из фазы 2 перенесено (в бэклог, НЕ в скоуп фазы 3):** вотчер ФС, UWP/shell:AppsFolder,
> персистентность recents, мульти-монитор/150%.
> **Бюджет:** 2 прогона воркера. `spikes/` не трогаем.

## Goal

Киллер-фича №1: клипборд-история, переживающая перезагрузку, со вставкой в активное
окно; сниппеты с CRUD в настройках и вставкой из поиска. Критерии: 1000 записей без
деградации; история переживает перезагрузку; вставка работает в терминал, браузер,
Office и UWP; общий RAM ≤ 80 МБ; p95 поиска не деградировал.

---

## 1. Архитектурные решения

| # | Вопрос | Решение | План Б |
|---|--------|---------|--------|
| D1 | Слушатель клипборда | Отдельный поток с **message-only окном** (`CreateWindowExW` HWND_MESSAGE) + `AddClipboardFormatListener` → `WM_CLIPBOARDUPDATE`; без подкласса окна Tauri (нет квирков с их wndproc); спайк доказал паттерн поток+окно | Подкласс HWND главного окна (SetWindowSubclass) |
| D2 | Форматы MVP | `CF_UNICODETEXT` (текст), `CF_DIB`/`CF_DIBV5`→PNG (изображения), `CF_HDROP` (файлы → список путей). Остальное (CF_HTML и пр.) — игнор + лог | CF_HTML в фазу 4 по потребности |
| D3 | Хранение | Существующая `Db` (core, rusqlite + refinery): миграция **V3** — таблицы `clipboard_entries` и `snippets` по §4.3 спеки; WAL уже включён; лимиты 1000 записей / 200 МБ, вытеснение LRU (used_at→created_at) среди **непиннед**; поиск по превью — `LIKE` c `normalize_name` (lowercase+ё→е; FTS5 не нужен на 1000 записях) | FTS5-таблица поверх preview, если LIKE медленнее 5 мс на 1000 |
| D4 | Изображения | Истина — PNG-файл в `%APPDATA%\iskra\clipboard\{id}.png` (колонка `image_path`), в `content` — ничего; превью-thumbnail 128px рядом (`{id}_thumb.png`) для списка; изображения > 20 МБ — skip + лог | BLOB в базе + команда отдачи байтов |
| D5 | Дедуп событий | Хэш контента (FNV-1a 64): подряд идущий идентичный контент НЕ создаёт новую запись (Excel/Office дёргают клипборд многократно); повторное копирование существующего → запись поднимается наверх (used_at + used_count) | — |
| D6 | Вставка в активное окно | Цепочка: запомнить `GetForegroundWindow` → скрыть лончер → положить контент в клипборд (sys) → `SetForegroundWindow(prev)` → пауза ~80 мс → `SendInput` Ctrl+V. **UIPI:** вставка в elevated-окна из не-elevated Iskra невозможна — задокументировать как ограничение | `WM_PASTE` точечно известным классам (notepad, браузеры) |
| D7 | Исключения и настройки | Расширение `Settings` (serde default — обратная совместимость существующего settings.json): `clipboard_enabled: bool` (default true), `clipboard_excluded_apps: Vec<String>` (process names, lowercase), лимиты — консты кода (не настройки, MVP). Владелец клипборда: `GetClipboardOwner`→pid→process name через `iskra-sys` | Исключения по заголовку окна |
| D8 | Вход в клипборд | Третий экран лончера (App: launcher \| settings \| **clipboard**): кнопка-иконка в поисковой строке + команда «клипборд» в поиске (word-match). Внутри: список, поиск по превью, pin (кнопка/PgUp?), Del — удалить, Enter/клик — вставить и скрыть лончер | Хоткей на клипборд (например Alt+V) — по желанию пользователя после фазы |
| D9 | Сниппеты в поиске | `SnippetsProvider` (SearchProvider, priority 85): матчит name/keywords/body → `ItemAction::CopyText` + авто-вставка (та же цепочка D6). CRUD — новая секция в SettingsView | — |

## 2. Раскладка (новое/изменяемое)

```
crates/iskra-core/
├── migrations/V3__clipboard.sql     # clipboard_entries, snippets (§4.3 спеки + image_path)
└── src/
    ├── clipboard.rs                 # репозиторий: add(text/image/files), list(query), delete,
    │                                #   pin, clear_unpinned?, evict (лимиты), normalize-поиск
    ├── snippets.rs                  # репозиторий CRUD + SnippetsProvider (D9)
    ├── settings.rs                  # + clipboard_enabled, clipboard_excluded_apps (serde default)
    └── ipc.rs                       # + ClipboardEntry, ClipboardKind, события clipboard://updated,
                                      #   команды clipboard_list/paste/delete/pin, snippets_*, IndexStatus нет
crates/iskra-sys/src/
├── clipboard.rs                     # message-window поток (D1), чтение форматов (D2),
│                                    #   set text/image(files) для вставки, GetClipboardOwner→process name
└── sendinput.rs                     # Ctrl+V (KEYEVENTF), Unicode-ввод (запас для сниппетов без клипборда?)
app/src/
├── clipboard_service.rs             # поток listener → дедуп (D5) → Db → clipboard://updated;
│                                    #   paste(id): foreground save/restore + SendInput (D6);
│                                    #   исключения (D7), лимиты/вытеснение
├── commands.rs                      # + clipboard_*, snippets_* команды
└── main.rs                          # запуск ClipboardService (если clipboard_enabled)
ui/src/
├── App.tsx                          # + экран clipboard
├── components/ClipboardView.tsx     # список/pin/del/поиск/превью (convertFileSrc на {id}_thumb.png)
├── components/SettingsView.tsx      # + секция клипборда (вкл/выкл, исключения) + CRUD сниппетов
├── ipc/types.ts, client.ts          # зеркало
└── theme.css                        # стили клипборд-списка, превью, чипы файлов
```

## 3. Шаги реализации

**Бюджет: 2 прогона воркера.** Прогон 1 = шаги 0–2, прогон 2 = шаги 3–6.

### Шаг 0. `[S]` Пре-чек (внутри шага 1)
Проверить сборкой: features windows для DataExchange/Ole (clipboard), UI_Input (SendInput);
refinery V3 накатывается на копию живой index.db (скопировать %APPDATA%\iskra\index.db в tmp).
- Done when: тест миграции на копии реальной БД зелёный.

### Шаг 1. `[P]` Core: БД-слой клипборда + сниппеты
Файлы: `migrations/V3__clipboard.sql`, `src/clipboard.rs`, `src/snippets.rs`, `settings.rs`, `ipc.rs`.
- Схема: `clipboard_entries(id PK, kind CHECK(text|image|files), content TEXT/список, image_path TEXT NULL,
  preview TEXT, pinned INT DEFAULT 0, source_app TEXT, content_hash TEXT, created_at INT, used_at INT,
  used_count INT DEFAULT 0)`; `snippets(id PK, name, body, keywords, created_at)` — §4.3 спеки
  (content BLOB заменён на text/path-вариант — зафиксировать как правку спеки в итогах фазы).
- Репозиторий: add (с вытеснением по лимитам), list(query?)+normalize, delete, pin, записи top-N;
  snippets CRUD. `SnippetsProvider` (D9).
- Настройки: новые поля с serde default; roundtrip старого settings.json без полей — тест.
- IPC: типы + события + 3–4 roundtrip-теста.
- Done when: `cargo test -p iskra-core` зелёный; тесты: вытеснение 1000+10, pin переживает вытеснение,
  дедуп-подъём (повторный add существующего hash → одна запись, used_at новый), поиск «прив»→«Привет мир»,
  V3 на копии живой БД.

### Шаг 2. `[P]` Sys: клипборд + SendInput
Файлы: `sys/clipboard.rs`, `sys/sendinput.rs`.
- Listener-поток (D1): RegisterClass+HWND_MESSAGE+AddClipboardFormatListener; callback в канал.
- Чтение: OpenClipboard с retry (5×20 мс — занят другим процессом), GetClipboardData по форматам D2,
  CF_DIB→PNG (image crate), CF_HDROP→Vec<path>; GetClipboardOwner→pid→process name (D7).
- Запись: set_text / set_image(png bytes) / set_files для вставки.
- SendInput: Ctrl+V (VK_CONTROL+V, KEYEVENTF_KEYUP последовательность).
- Тесты (свои процессы): set_text→чтение roundtrip; listener: положить текст → канал получил событие;
  CF_HDROP: set/чтение tmp-файлов; DIB 32bpp→PNG.
- Done when: `cargo test -p iskra-sys` зелёный; в ignore-тестах —SendInput (реальную вставку — ручная приёмка).

### Шаг 3. `[S]` App: ClipboardService + IPC-команды (нужны 1–2)
Файлы: `app/clipboard_service.rs`, `commands.rs`, `main.rs`.
- Сервис: listener-канал → формат/дедуп/исключения (D7) → add в Db → `clipboard://updated`;
  paste(id): D6 целиком (foreground save→скрыть→set→restore→SendInput); ленивое обслуживание лимитов.
- Команды: clipboard_list(query)->Vec<ClipboardEntry>, clipboard_paste(id), clipboard_delete(id),
  clipboard_pin(id, pinned), snippets_*(CRUD); settings: clipboard_enabled применяется на лету
  (listener живёт, но игнорирует события).
- Done when: `build.cmd` зелёный; smoke: копирую текст в notepad → в логе запись+событие; рестарт —
  история на месте; копирование 3× подряд одного → одна запись с used_count=3.

### Шаг 4. `[P]` UI: экран клипборда (нужен шаг 3)
Файлы: `ClipboardView.tsx`, `App.tsx`, `types/client`, `theme.css`.
- Список: текст (2 строки, ellipsis), изображение (thumbnail, клик — полный в detail? MVP: только thumb+размер),
  файлы (чипы имён); поиск по превью (debounce 50 мс); pin/del; Enter → paste; подпись source_app + время.
- Вход: иконка в searchbar (рядом с ⚙) + поиск «клипборд» → экран.
- Esc на экране клипборда → назад в launcher (не скрывать лончер).
- Done when: `npm run build` зелёный; ручной чек §5 (пп. 1–4).

### Шаг 5. `[P]` UI: сниппеты + настройки клипборда (параллельно 4, после шага 3)
Файлы: `SettingsView.tsx` (+секции), мелочи в `ResultList/ClipboardView` при нужде.
- Сниппеты CRUD (имя/тело/keywords), тест вставки из поиска.
- Настройки клипборда: toggle мониторинга (применяется сразу), список исключений (текстом, по одному
  process name на строку), сохраняется через существующий update_settings.
- Done when: `npm run build` зелёный; сниппет «адрес» вставляется из поиска; исключение notepad
  реально блокирует запись.

### Шаг 6. `[S]` Приёмочные замеры (release, после 4–5)
- Scripted: 1000 записей (генератор через внутреннюю команду/тестовый хук), лимит 200 МБ с изображениями,
  RAM ≤ 80 МБ, p95 поиска (bench-search) не хуже 9 мс.
- Done when: цифры зафиксированы в отчёте приёмки.

## 4. Риски и план Б

| # | Риск | План Б |
|---|------|--------|
| 1 | OpenClipboard конкуренция (другой процесс держит) | retry 5×20 мс; пропустить событие с логом — потеря одной записи не критична |
| 2 | UIPI: вставка в elevated-окна блокирована | задокументировать; точечный WM_PASTE для известных классов; (полноценное решение — запуск Iskra elevated — вне скоупа) |
| 3 | CF_DIB-кворки (16bpp, палитра, negative height) | поддержка 32/24bpp top-down/bottom-up; остальное — skip+лог (текст и файлы — основа) |
| 4 | Фокус-пляска при paste (restore + SendInput рано/поздно) | пауза 80 мс + повторная проверка GetForegroundWindow == prev; при несовпадении — повторrestore; ручная проверка на 4 типах окон |
| 5 | Дублирование записей (Office шлёт несколько WM_CLIPBOARDUPDATE) | дедуп по content_hash (D5) — тест обязателен |
| 6 | Изображения съедают RAM/диск | лимит 20 МБ/изображение, thumbnails, общий лимит 200 МБ с вытеснением; RAM-замер в шаге 6 |
| 7 | V3-миграция на живой БД пользователя | тест на копии реальной index.db (шаг 0); refinery идемпотентен |
| 8 | Списки 1000 записей в UI без пагинации | MVP: показываем топ-100 по used_at + поиск; виртуализация — только если скролл дёргается (риск 5 фазы 2 не проявился) |
| 9 | Клипборд-фича в корпоративных средах (DLP-плагины блокируют чтение) | события пропускаются с логом, фича деградирует до ручного сниппетов — не падаем |

## 5. Верификация (чек-лист приёмки)

Сборка: `build.cmd` → `target/release/iskra.exe`.

- [x] **1000 записей**: scripted-наполнение (seed 1000/1000), RAM ≤ 80 МБ (private WS):
      тёплый старт 78,3 МБ — PASS (первый на машине пуск 83,2 МБ — миграция V3 +
      первый рескан индекса, далее стабильно 78,3±0,02) — script (2026-10-02);
- [ ] **Перезагрузка**: после reboot история клипборда и pin на месте — manual (user);
- [ ] **Вставка работает**: терминал, браузер, Office/LibreOffice, UWP-приложение — manual (user);
- [x] **Pin**: закреплённая запись не вытесняется при переполнении лимита —
      юнит-тест «pin переживает вытеснение» зелёный — script;
- [x] **Лимиты**: переполнение 1000/200 МБ → вытесняются старейшие непиннед —
      юнит-тест «вытеснение 1000+10» зелёный — script;
- [ ] **Исключения**: notepad в excluded → копирование из него не записывается — manual;
- [ ] **Вкл/выкл мониторинга**: toggle работает сразу и переживает рестарт — manual;
- [ ] **Сниппеты**: CRUD в настройках, вставка из поиска — manual;
- [x] **p95 поиска не деградировал**: bench-search 200, корпус 10k: p95 = 8,5 мс
      (min 6,45 / median 7,64) ≤ 9 мс — PASS (2026-10-02, машина №2) — script;
- [ ] **Мульти-монитор/150%** — перенос, если появится железо.

**Примечание (2026-10-02, машина №2 — `C:\SFT_Storage\Projects\Iskra`):** авто-часть прогнана после
переноса проекта: тесты core 93 + sys 23 зелёные; адаптации: `build.cmd` → BuildTools в
`Program Files (x86)`; `ram-acceptance.ps1` → путь через `$PSScriptRoot`; тест
`launch_opens_notepad_and_kills_it` → `launch_opens_process_and_kills_it` — жертва `System32\cmd.exe /k`:
упакованный Notepad активируется через стаб, стаб сразу выходит и хэндл от ShellExecuteExW оказывается
хэндлом мёртвого процесса (TerminateProcess → ACCESS_DENIED); вдобавок GNU timeout из Git в PATH
затеняет `System32\timeout.exe`. Ручная часть чек-листа — за пользователем.

## 6. Распределение по прогонам воркера

- **Прогон 1:** шаги 0–2 (1 и 2 параллельно после фиксации схемы V3 в шаге 0–1).
- **Прогон 2:** шаг 3 → параллельно 4 и 5 → шаг 6 + чек-лист; чекбоксы — вручную после приёмки.
