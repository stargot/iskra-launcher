# План реализации UI-фич «Размер окна + Сниппеты отдельным табом» — Iskra

> **Дата:** 2026-10-02
> **Основание:** [docs/plans/2026-09-30-phase3-implementation.md](2026-09-30-phase3-implementation.md) (образец, D7/D9 — настройки и сниппеты),
> решение пользователя: окно ≥ 2x (режимы normal/2x/fullscreen), сниппеты — 4-й экран, подсветка только в превью без CodeMirror.
> **Бюджет:** 2 прогона воркера. `spikes/` не трогаем. RAM ≤ 80 МБ (private WS) — критерий сохраняется.

## Goal

Две UI-фичи лончера: (1) настройка размера окна с режимами «обычный / 2× / на весь экран», применяемая сразу и на старте, с продуманным hide-on-blur при fullscreen; (2) вынос CRUD сниппетов из настроек на четвёртый экран с моноширинным редактором и лёгкой самописной подсветкой синтаксиса в превью, без изменения БД/поиска.

---

## 1. Архитектурные решения

| # | Вопрос | Решение | План Б |
|---|--------|---------|--------|
| D1 | Модель настройки | Enum `WindowMode { Normal, Double, Fullscreen }` в `settings.rs`, serde lowercase (`"normal" \| "double" \| "fullscreen"`), поле `window_mode` в `Settings` + `window_mode: Option<WindowMode>` в `SettingsPatch`. У `Settings` уже есть контейнерный `#[serde(default)]` → старый settings.json читается (дефолт `Normal` = сегодняшние 720×480). Имя в JSON: `windowMode` | Числовой scale-фактор — отклонён (менее расширяем) |
| D2 | Применение размера | `window::apply_mode(win, mode)` в `app/src/window.rs`: Normal → `set_fullscreen(false)` + `set_size` 720×480 (логические константы `BASE_W/BASE_H` рядом с tauri.conf.json, коммент-контракт); Double → `set_fullscreen(false)` + `set_size` 1440×960 с клампом в work area монитора; Fullscreen → `set_fullscreen(true)`. Вызовы: `main.rs` setup (после mica, до хоткеев) и `update_settings` (после успешного save; ошибка set_size → лог, не Err) | `ASSUMPTION:` tauri `set_size` работает программно при `resizable: false` (SetWindowPos не зависит от WS_THICKFRAME). Проверяется запуском на шаге 1; если нет — обёртка `set_resizable(true) → set_size → set_resizable(false)` |
| D3 | Кламп 2× на малых мониторах | Физический размер = логический × `win.scale_factor()`, максимум — `rcWork` монитора окна через `iskra_sys::monitor::monitor_rect_from_hwnd(win.hwnd())`. Центрирование: `show_window` уже центрирует по `outer_size` при каждом показе; после `set_size` на живом окне — перецентрировать той же формулой по монитору окна | Без клампа (окно уйдёт за экран на 1366×768) — недопустимо |
| D4 | Hide-on-blur при fullscreen | **Сохраняется во всех режимах без исключений.** Обоснование: окно `alwaysOnTop`; fullscreen без hide-on-blur после alt-tab накроет чужое non-topmost приложение и заблокирует рабочий стол до явного закрытия. Hide-on-blur возвращает управление автоматически; состояние UI (экран, черновики) переживает hide (webview жив), повторный хоткей восстанавливает fullscreen (`set_fullscreen` персистентен) | «Pin-режим» (fullscreen: hide-on-blur off + alwaysOnTop off) — по фидбеку после приёмки, отдельной задачей |
| D5 | RAM-бюджет | Тот же WebView2-процесс, растёт лишь поверхность композитинга. Замер `spikes/ram-acceptance.ps1` в режимах 2× и fullscreen на приёмке — критерий ≤ 80 МБ остаётся в чек-листе | Если fullscreen поднимет WS выше бюджета — дефолт Normal, fullscreen как opt-in (так и есть), зафиксировать цифры |
| D6 | Вход на экран сниппетов | Четвёртый экран `"snippets"` в `App.tsx` (Screen union, рендер по образцу clipboard). Вход: (а) кнопка 📝 в searchbar между 📋 и ⚙ (`ResultList` Props + `onOpenSnippets`); (б) точное совпадение запроса «сниппеты»/«snippets» — тот же `useEffect`-паттерн, что «клипборд» (ResultList ~стр. 205). Core не трогаем (nav-событие из трея не нужно) | nav://snippets из трея — по потребности позже |
| D7 | Перенос CRUD | Новый `ui/src/components/SnippetsView.tsx` (образец — ClipboardView: topbar + onBack, Esc обрабатывает App). Мастер-список (локальный фильтр по имени/keywords, кнопка «Новый», удаление) + панель редактора: имя, keywords, моно-`textarea` (rows 10–14, resize: vertical), Сохранить (dirty), живое превью с подсветкой под редактором (debounce ~150 мс). Из `SettingsView.tsx` секция «Сниппеты», `SnippetRow`, `addSnippet`, `reloadSnippets` удаляются целиком; функции `snippets*` в `ipc/client.ts` остаются (их подхватывает SnippetsView); команды `snippets_*` в `commands.rs` — без изменений | Оставить в настройках ссылку-кнопку «Сниппеты →» на новый экран (дёшево, улучшает discoverability) |
| D8 | Подсветка | **Самописный** `ui/src/lib/highlight.ts`, без зависимостей: однопроходный regex-токенизатор (строки `'.."`, `…`, `..`, комментарии `// /* */ #`, числа, общий набор ключевых слов C-подобных языков ~60 слов) + JSON-режим (ключи/значения/true-false-null) с авто-детектом: trimmed body начинается с `{`/`[` → JSON, иначе generic. Рендер — React `<span class="tok-*">`, никакого `dangerouslySetInnerHTML`. Лимит: тело > 100 000 символов → превью plain (CPU/RAM). Оценка: ~150–250 строк TS, бандл 245 → ~250 КБ, RAM не задевает. Схему БД не меняем (язык не выбирается — миграция V4 не нужна) | prismjs (core + 3 языка ≈ +25–30 КБ min) — если качество не устроит на приёмке |
| D9 | Цвета подсветки | 4–5 переменных `--code-keyword/--code-string/--code-comment/--code-number/--code-key` в обоих блоках `[data-theme]` (dark+light) в `theme.css`, приглушённые, контраст AA | Хардкод в классах tok-* — не делать |
| D10 | Общий поиск | `SnippetsProvider` (crates/iskra-core/src/snippets.rs) **не трогаем** — сниппеты остаются в поиске, вставка через `run_item` как была | — |

## 2. Files to Modify / New Files (раскладка)

```
crates/iskra-core/src/settings.rs   # + WindowMode enum, поле window_mode, патч, тесты (прогон 1)
app/src/window.rs                   # + BASE_W/BASE_H, apply_mode(win, mode), recenter (прогон 1)
app/src/main.rs                     # + вызов apply_mode в setup (прогон 1)
app/src/commands.rs                 # + применение window_mode в update_settings (прогон 1)
ui/src/ipc/types.ts                 # + windowMode: "normal"|"double"|"fullscreen" (оба прогона зеркалят)
ui/src/components/SettingsView.tsx  # + card «Размер окна» (сегменты); − секция «Сниппеты», SnippetRow (прогон 2)
ui/src/components/SnippetsView.tsx  # НОВЫЙ: CRUD + редактор + превью (прогон 2)
ui/src/lib/highlight.ts             # НОВЫЙ: самописный токенизатор + React-рендер токенов (прогон 2)
ui/src/App.tsx                      # + экран "snippets" (прогон 2)
ui/src/components/ResultList.tsx    # + кнопка 📝 + команда «сниппеты»/«snippets» (прогон 2)
ui/src/theme.css                    # + карточка размера, стили экрана сниппетов, .snippet-editor/.snippet-preview,
                                    #   tok-* + --code-* в обеих темах (оба прогона)
```
Не трогаем: `crates/iskra-core/src/snippets.rs`, `commands.rs` (snippets_*), миграции, `ipc/client.ts` (кроме ничего — функции уже есть).

## 3. Шаги реализации

**Бюджет: 2 прогона воркера.** Прогон 1 = шаги 1–3 (фича «окно»), прогон 2 = шаги 4–6 (фича «сниппеты»). Внутри прогона шаги последовательны (`S`): общий файл `settings.rs`/`SettingsView.tsx`/`theme.css` между прогонами исключает параллельность.

### Прогон 1 — размер окна

**Шаг 1.** `[S]` Core: `WindowMode`.
Файл: `crates/iskra-core/src/settings.rs`.
- Enum `WindowMode` (serde lowercase, Default = Normal), поле `window_mode` в `Settings` и `Option<WindowMode>` в `SettingsPatch` + ветка в `patched()`.
- Тесты по образцу `old_settings_json_without_clipboard_fields_roundtrips`: (а) дефолт `normal`; (б) старый JSON без `windowMode` читается, дефолт проставляется, save/load — полный круг, в JSON `"windowMode": "normal"`; (в) патч `"double"` применяется точечно.
- Done when: `cargo test -p iskra-core` зелёный.

**Шаг 2.** `[S]` App: применение режима.
Файлы: `app/src/window.rs`, `app/src/main.rs`, `app/src/commands.rs`.
- `window.rs`: константы `BASE_W=720, BASE_H=480` (коммент «синхронно с tauri.conf.json»), `apply_mode(win, mode)` с клампом (D3) и recenter по монитору окна; в `main.rs` setup после mica — `apply_mode(&win, &settings.window_mode)`; в `update_settings` после `next.save()` — если патч менял режим, применить к окну (ошибка → лог, команда не падает).
- Проверка `ASSUMPTION` D2 запуском: если `set_size` игнорируется при `resizable: false` — включить план Б (set_resizable-обёртка).
- Done when: `build.cmd` зелёный; ручной пуск: `windowMode: "double"` в settings.json до старта → окно 2×; смена в UI → применяется сразу, переживает рестарт.

**Шаг 3.** `[S]` UI: выбор режима.
Файлы: `ui/src/ipc/types.ts`, `ui/src/components/SettingsView.tsx`, `ui/src/theme.css`.
- `types.ts`: `windowMode` в `Settings`/`SettingsPatch` (зеркало D1). `SettingsView`: card «Размер окна» с `.segmented` (Обычный / 2× / На весь экран), клик → `updateSettings({ windowMode })` сразу (по образцу темы), активный сегмент из `settings.windowMode`.
- `theme.css`: если нужно, минимум (сегменты уже есть); fullscreen-раскладка — flex-каркас уже растягивается, контент капать не требуется.
- Done when: `npm --prefix ui run build` зелёный; ручной чек пп. 1–4 чек-листа §5.

### Прогон 2 — сниппеты отдельным табом + подсветка

**Шаг 4.** `[S]` UI: хайлайтер.
Файл: `ui/src/lib/highlight.ts` (новый).
- Токенизатор D8 (generic + JSON-автодетект, лимит 100 КБ), экспорт `highlight(body): Token[]` (или готовые React-узлы `Highlighted({ body })`), без new dependency.
- Done when: `npm --prefix ui run build` (tsc) зелёный; в консоли Vite — размер бандла вырос ≤ +10 КБ.

**Шаг 5.** `[S]` UI: экран сниппетов + перенос CRUD.
Файлы: `ui/src/components/SnippetsView.tsx` (новый), `App.tsx`, `ResultList.tsx`, `SettingsView.tsx`, `ui/src/theme.css`.
- `SnippetsView` по D7 (master-detail, моно-редактор, превью с подсветкой, inline notice/error как в SettingsView); `App.tsx`: `Screen` += `"snippets"`, рендер + `onBack` (Esc уже работает); `ResultList`: Props `onOpenSnippets`, кнопка 📝 между 📋 и ⚙, команда «сниппеты»/«snippets»; `SettingsView`: удалить секцию «Сниппеты»/`SnippetRow`/связанные state (клиентские функции остаются).
- `theme.css`: `.snippets`, `.snippet-list`, `.snippet-editor` (моно: Consolas/Cascadia Mono), `.snippet-preview`, `.tok-*`, `--code-*` в обеих темах; удалить неиспользуемое из `.snippet-row` по остаточному принципу.
- Done when: `npm --prefix ui run build` зелёный; `cargo test -p iskra-core` по-прежнему зелёный (core не менялся); сниппет ищется в общем поиске и вставляется (D10 не сломан).

**Шаг 6.** `[S]` Приёмочные замеры (после 5).
- `build.cmd` → сборка; `spikes/ram-acceptance.ps1` в режимах Normal / 2× / fullscreen (частный WS ≤ 80 МБ); цифры — в отчёт приёмки.
- Done when: цифры RAM зафиксированы; чек-лист §5 заполнен пользователем.

## 4. Риски и план Б

| # | Риск | План Б |
|---|------|--------|
| 1 | `set_size` игнорируется при `resizable: false` (ASSUMPTION D2) | Обёртка `set_resizable(true) → set_size → set_resizable(false)`; проверка на шаге 2 запуском |
| 2 | Fullscreen + `transparent: true` + mica — визуальные квирки (чёрный фон, артефакты) | Визуальный чек на шаге 2; при артефактах — перед fullscreen отключать прозрачность/vibrancy или ограничиться «2×+кламп» как максимумом |
| 3 | 2× не влезает в work area (1366×768, 150% DPI) | Кламп по rcWork (D3) — обязателен; тест вручную на малом разрешении в чек-листе |
| 4 | Fullscreen без hide-on-blur блокирует стол (alwaysOnTop) | Решение D4: hide-on-blur сохранён везде; pin-режим — только по фидбеку |
| 5 | RAM-бюджет ≤ 80 МБ в 2×/fullscreen (сейчас 78,3 тёплый) | Замер на шаге 6; полный экран — opt-in режим; при превышении — зафиксировать и решить с пользователем |
| 6 | Подсветка на больших телах — CPU/латентность ввода | Лимит 100 КБ → plain (D8); debounce превью 150 мс; подсветка только в превью, не в textarea |
| 7 | Регулярный-токенизатор: катастрофический бэкtracking | Однопроходный сканер без вложенных квантификаторов; тела сниппетов пользовательские, лимит п.6 страхует |
| 8 | Поломка CRUD при переносе (настройки → таб) | Команды/клиент не менялись; ручной чек CRUD в §5; опционально кнопка-ссылка в настройках (D7 план Б) |
| 9 | Старый settings.json (совместимость) | serde default + тест шага 1 (roundtrip по образцу существующего) |

## 5. Верификация

Авто (каждый прогон): `cargo test -p iskra-core` ✓, `npm --prefix ui run build` ✓, `build.cmd` ✓.

Чек-лист ручной приёмки (пользователь):
- [ ] **Режимы на старте**: выставить каждый из трёх `windowMode` в settings.json → перезапуск → размер верный (Normal 720×480, 2× ≈ 1440×960, fullscreen);
- [ ] **Смена на лету**: переключение сегмента в настройках применяет размер немедленно, без рестарта;
- [ ] **Кламп**: на малом окне/виртуалке 1366×768 режим 2× не вылезает за экран;
- [ ] **Hide-on-blur в fullscreen**: клик мимо/alt-tab → лончер скрылся; повторный Alt+Space → fullscreen восстановлен;
- [ ] **Переживает рестарт**: выбранный режим сохранён в settings.json и применён после перезапуска;
- [ ] **RAM**: 2× и fullscreen — private WS ≤ 80 МБ (шаг 6, script);
- [ ] **Вход на таб**: кнопка 📝 открывает экран; запрос «сниппеты» в поиске открывает экран; Esc → назад в лончер;
- [ ] **CRUD**: создать/переименовать/изменить тело/удалить — сохраняется и переживает рестарт;
- [ ] **Подсветка**: JSON-сниппет — ключи/строки/числа раскрашены в обеих темах; текстовый — generic (строки/комментарии/ключевые слова); редактор остаётся моно-без подсветки; превью живое при вводе;
- [ ] **Общий поиск не сломан**: сниппет находится по имени/keywords, Enter копирует и вставляет;
- [ ] **Настройки**: секция сниппетов исчезла, остальные карточки работают.

## 6. Распределение по прогонам воркера

- **Прогон 1:** шаги 1→2→3 (фича «размер окна») + пп. 1–6 чек-листа.
- **Прогон 2:** шаги 4→5→6 (фича «сниппеты-таб») + пп. 7–11 чек-листа + RAM-замер.

---

## Краткая сводка плана

**Шаги:** (1) core: enum `WindowMode` в settings + serde-default-тесты → (2) app: `window::apply_mode` (кламп+recenter), вызов в setup и `update_settings` → (3) UI: сегментированный выбор в настройках → (4) UI: самописный хайлайтер `highlight.ts` → (5) UI: экран `SnippetsView` (перенос CRUD, кнопка 📝 + команда «сниппеты», чистка SettingsView) → (6) приёмка: сборка + RAM в трёх режимах.

**Файлы:** `crates/iskra-core/src/settings.rs`, `app/src/window.rs`, `app/src/main.rs`, `app/src/commands.rs`, `ui/src/ipc/types.ts`, `ui/src/components/{SettingsView,App→App.tsx,ResultList}.tsx`, `ui/src/theme.css`; новые: `ui/src/components/SnippetsView.tsx`, `ui/src/lib/highlight.ts`. Не трогаются: `snippets.rs`, `SnippetsProvider`, миграции, IPC-команды.

**Итог работы:** изучены окно/настройки/IPC/UI/сниппеты проекта, принятые решения зафиксированы (D1–D10: serde-default enum, apply_mode с клампом по work area, hide-on-blur сохраняется в fullscreen — обоснование alwaysOnTop; самописный хайлайтер без зависимостей с лимитом 100 КБ и без миграции БД), план на 2 прогона воркера с Done-критериями, рисками и ручным чек-листом составлен. Единственное отмеченное `ASSUMPTION` — программный `set_size` при `resizable: false` — проверяется воркером на шаге 2, план Б вписан. Файл плана не создавал (планищик read-only) — документ выше готов к записи в `docs/plans/2026-10-02-ui-window-and-snippets.md` дословно.

---

## Итоги (2026-10-02, машина №2)

- Прогон 1 «размер окна»: коммит `6b24bba`. Reviewer: FIX REQUIRED (кламп rcMonitor → rcWork) — исправлено; warning по логированию set_fullscreen(false) — закрыт. Core-тесты 95 passed.
- Прогон 2 «сниппеты-таб»: коммит `0486868`. Reviewer: APPROVE WITH COMMENTS; два warning (потеря dirty-черновика, удаление без подтверждения) — закрыты (confirm при уходе, двухклик-удаление). Бандл 245 → 251,74 КБ, тесты 95 passed.
- RAM приёмка (spikes/ram-acceptance.ps1, private WS дерева, 3 сэмпла): normal **78,01/78,08/78,16 МБ — PASS (≤80)**; double 85,90/85,95/85,95 (+6); fullscreen 91,24/91,24/91,25 (+11). Превышение в 2×/fullscreen — рост поверхности композитинга WebView2, режимы opt-in, дефолт normal в бюджете (риск 5 плана сбылся, решение зафиксировано здесь).
- Авто-верификация: cargo test -p iskra-core ✓, npm run build ✓ (40 модулей), build.cmd ✓.
- Ручные пункты чек-листа §5 (пп. 1–11) — за пользователем.
