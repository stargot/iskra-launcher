# План реализации Фазы 1 «Скелет лончера» — Iskra

> **Дата:** 2026-09-29
> **Основание:** [docs/specs/2026-09-17-windows-raycast-clone-plan.md](../specs/2026-09-17-windows-raycast-clone-plan.md) (Фаза 1),
> [docs/specs/2026-09-17-windows-raycast-clone-spec.md](../specs/2026-09-17-windows-raycast-clone-spec.md) (§4.1, решения 2/6/7/8),
> [spikes/tauri-app/RESULTS.md](../../spikes/tauri-app/RESULTS.md) (проверенные паттерны фазы 0).
> **Проверенное окружение:** Tauri 2.11.5, tauri-plugin-global-shortcut 2.3.2, window-vibrancy 0.8.0,
> windows 0.62.2 (Cargo.lock спайка); rustc 1.92.0, Node 24.15; Windows SDK 10.0.26100; сборка через vcvars64.
> **Бюджет:** 1–2 прогона воркера-агента. `spikes/` не трогаем — паттерны копируем адаптируя, не импортируем.

## Goal
Создать в корне репозитория новый cargo workspace + UI-пакет: резидентное приложение `iskra.exe`
(Tauri 2), живущее в трее, показывающее пустой список по переназначаемому глобальному хоткею на
активном мониторе, скрывающееся по blur, с настройками (тема/хоткей/автозапуск) и зафиксированным
IPC-контрактом — с выполнением критерия: p95 показа < 100 мс, RAM idle ≤ 80 МБ.

---

## 1. Раскладка репозитория

```
D:/Projects/Iskra/
├── Cargo.toml                      # workspace: members, workspace.dependencies, [profile.release]
├── Cargo.lock
├── build.cmd                       # vcvars64 → npm build (если нужен) → cargo build --release
├── .gitignore                      # + /target/, ui/node_modules/, ui/dist/, app/gen/, *.log
├── README.md                       # секция «Сборка» (5–10 строк)
├── crates/
│   ├── iskra-core/                 # чистая логика, без Tauri и WinAPI
│   │   ├── Cargo.toml              # serde, serde_json, dirs
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── settings.rs         # Settings/SettingsPatch, defaults, load/save (атомарно),
│   │       │                       #   путь %APPDATA%\iskra\settings.json
│   │       ├── ipc.rs              # КОНТРАКТ: типы команд и событий (serde, camelCase) + roundtrip-тест
│   │       └── logging.rs          # файловый лог runtime.log → %APPDATA%\iskra\logs (порт из спайка)
│   └── iskra-sys/                  # безопасные обёртки windows-rs (ADR 7)
│       ├── Cargo.toml              # windows 0.62.2: Win32_Foundation, Win32_UI_WindowsAndMessaging,
│       │                           #   Win32_Graphics_Gdi, Win32_System_Registry
│       └── src/
│           ├── lib.rs
│           ├── monitor.rs          # foreground→cursor→primary цепочка: физический rect монитора
│           ├── autostart.rs        # set/remove/is_enabled: HKCU\...\Run, значение "Iskra" → путь exe
│           └── fullscreen.rs       # is_foreground_fullscreen() — эвристика (rect == monitor rect)
├── app/                            # бинарник iskra.exe (tauri-шелл)
│   ├── Cargo.toml                  # name = "iskra"; tauri = "2.11" (features: tray-icon, image-png),
│   │                               #   tauri-plugin-global-shortcut "2", window-vibrancy 0.8.0,
│   │                               #   iskra-core, iskra-sys; edition = "2021" (!)
│   ├── build.rs                    # tauri_build::build() — как в спайке
│   ├── tauri.conf.json             # окно main: 720×480, decorations/visible=false, transparent,
│   │                               #   skipTaskbar, alwaysOnTop; devUrl :5173; frontendDist ../ui/dist
│   ├── capabilities/default.json   # core:default (как в спайке)
│   ├── icons/                      # КОПИЯ иконок из spikes/tauri-app/icons (icon.ico, 32x32.png,
│   │                               #   128x128.png) — spikes/ не модифицируем
│   └── src/
│       ├── main.rs                 # GPU-off через set_var (cfg!(debug_assertions)), builder, setup
│       ├── window.rs               # позиционирование «центр-верх активного монитора» (физические px),
│       │                           #   show/hide/toggle, mica, hide-on-blur (порт из спайка)
│       ├── hotkey.rs               # сервис: register/remap/конфликт→фолбэк→событие (паттерн спайка)
│       ├── tray.rs                 # TrayIconBuilder + меню: Показать/Настройки/Автозапуск✓/Выход
│       ├── commands.rs             # #[tauri::command]: get_settings, update_settings, get_runtime_info
│       └── bench.rs                # --bench N: SendInput Ctrl+Alt+F9 → latency.log (порт из спайка)
├── ui/                             # React + TS + Vite (решение 2)
│   ├── package.json                # react ^19, react-dom ^19, @tauri-apps/api ^2.7;
│   │                               #   dev: vite ^6.3, @vitejs/plugin-react ^4.6, typescript ~5.8,
│   │                               #   @tauri-apps/cli ^2, @types/react(-dom) ^19
│   ├── tsconfig.json
│   ├── vite.config.ts              # react(); server.port 5173 strictPort; clearScreen false
│   ├── index.html
│   └── src/
│       ├── main.tsx
│       ├── App.tsx                 # 2 «экрана»: список | настройки (простой state, без роутера)
│       ├── theme.css               # CSS-переменные, [data-theme="dark"|"light"], mica-совместимый фон
│       ├── ipc/
│       │   ├── types.ts            # ЗЕРКАЛО crates/iskra-core/src/ipc.rs (источник истины — Rust)
│       │   └── client.ts           # типизированные обёртки invoke()/listen()
│       └── components/
│           ├── ResultList.tsx      # пустой список-заглушка (скелетон-строки)
│           └── SettingsView.tsx    # тема, хоткей (input+Сохранить), автозапуск (checkbox)
├── docs/                           # не трогаем (кроме чекбоксов фазы 1 — вручную после приёмки)
└── spikes/                         # НЕ ТРОГАЕМ — только читаем/копируем
```

Принципы: `iskra-core` — без зависимостей от Tauri/WinAPI (тестируется быстро, переиспользуется
фазой 2); `iskra-sys` — единственное место с `unsafe`/windows-rs; `app` — только склейка.
Спайк не импортируем — паттерны (wndproc, фолбэк-логика, bench, build.cmd) копируем адаптируя.

---

## 2. Шаги реализации

**Бюджет: 2 прогона воркера.** Прогон 1 = шаги 0–3, прогон 2 = шаги 4–6.

### Шаг 0. `[S]` Пре-чек окружения (внутри шага 1)
Проверить: `vcvars64.bat` по пути из спайка, `C:\Program Files (x86)\Windows Kits\10\Lib` существует,
`node -v` ≥ 20.19, `git status` чист.
- Done when: все 4 проверки выполнены командами, зафиксированы в отчёте.

### Шаг 1. `[S]` Скелет workspace + app + ui — собирается и запускается
Файлы: корневой `Cargo.toml` (workspace, resolver = "2", workspace.dependencies: serde 1, serde_json 1,
windows 0.62.2, tauri = "2.11"), `.gitignore`, `build.cmd` (вызывает vcvars64 → `npm --prefix ui run build`
если нет `ui/dist` → `cargo build --release`), `app/` целиком (порт main.rs из спайка в урезанном виде:
builder + frameless-окно из conf + mica + hide-on-blur + временная прямая регистрация Alt+Space для
smoke-теста + файловый лог), `ui/` каркас (Vite+React, App пишет «Iskra», пустой список).
`frontendDist: "../ui/dist"`, поэтому порядок сборки: npm → cargo. **Edition 2021** во всех крейтах
(чтобы `std::env::set_var` был safe — см. риск 6).
- Зависимости: tauri "2.11" (в lock спайка 2.11.5), tauri-build "2", tauri-plugin-global-shortcut "2",
  window-vibrancy "=0.8.0", npm-версии из дерева выше.
- Done when: `build.cmd` отрабатывает → существует `target/release/iskra.exe`; запуск → Alt+Space
  показывает окно (центр-верх основного монитора, как в спайке), клик мимо — скрывает;
  `npm run build` в ui/ зелёный.

### Шаг 2. `[P]` Крейт `iskra-sys` — монитор, автозапуск, fullscreen
Файлы: `crates/iskra-sys/*` (см. дерево).
- `monitor::foreground_monitor_rect() -> Option<Rect>`: `GetForegroundWindow` →
  `MonitorFromWindow(MONITOR_DEFAULTTONEAREST)` → `GetMonitorInfoW.rcMonitor` (физические px);
  при NULL foreground — `GetCursorPos` → `MonitorFromPoint`; последнее — нет (берём primary).
- `autostart::{set, remove, is_enabled}`: `RegCreateKeyExW/RegSetValueExW/RegQueryValueExW/RegDeleteValueW`
  на `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, имя значения `"Iskra"`,
  данные = `std::env::current_exe()`.
- `fullscreen::is_foreground_fullscreen()`: rect окна == rect монитора && стили без WS_CAPTION.
- Зависимости: windows = "0.62.2" (features спайка + Win32_Graphics_Gdi, Win32_System_Registry).
- Done when: `cargo test -p iskra-sys` зелёный — тест автозапуска пишет/читает/удаляет значение
  `IskraPhase1Test` в реальном HKCU Run (с очисткой); тест monitor возвращает Some на рабочем столе.

### Шаг 3. `[P]` Крейт `iskra-core` — настройки + IPC-контракт (параллельно шагу 2)
Файлы: `crates/iskra-core/src/{settings,ipc,logging}.rs`.
- `Settings { theme: Theme (Dark|Light), hotkey: String ("Alt+Space"), autostart: bool }`,
  `SettingsPatch` — все поля Option. Defaults: dark / "Alt+Space" / false.
  Путь: `dirs::config_dir()\iskra\settings.json`; сохранение атомарно (tmp + rename);
  загрузка при старте, битый JSON → defaults + лог.
- `ipc.rs` — контракт, camelCase (serde rename_all):
  - Команды (UI→core): `get_settings() -> Settings`;
    `update_settings(patch: SettingsPatch) -> Result<Settings, SettingsError>` (InvalidHotkey | HotkeyBusy | Io);
    `get_runtime_info() -> RuntimeInfo { active_hotkey: Option<String>, fallback_active: bool }`.
  - События (core→UI): `settings://changed` → `Settings`; `hotkey://changed` → `{ hotkey, from_fallback }`;
    `nav://settings` → null (от трея).
- Зависимости: serde 1 (derive), serde_json 1, dirs "6".
- Done when: `cargo test -p iskra-core` зелёный (serde roundtrip Settings/SettingsPatch/событий;
  save/load в temp-каталоге).

### Шаг 4. `[S]` App-сервисы: хоткей, окно, трей, команды (нужны шаги 2+3)
Файлы: `app/src/{main,window,hotkey,tray,commands}.rs`.
- `main.rs`: **первой строкой main** —
  `if !cfg!(debug_assertions) { std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--disable-gpu --disable-gpu-compositing"); }`
  (механизм замерен в спайке: 64–67 МБ, аддитивно к аргументам); builder + setup: загрузить Settings,
  mica (результат в лог), инициализация сервисов, регистрация команд.
- `hotkey.rs`: `HotkeyService` на плагине global-shortcut — register из настроек; при ошибке →
  фолбэк Ctrl+Alt+Space (паттерн спайка, приложение не падает); `remap(new)` = unregister старого →
  register нового → при ошибке откат на старый/фолбэк + `Result::Err(HotkeyBusy)`;
  toggle: если окно видимо — hide, иначе show; события `hotkey://changed`.
  Bench-хоткей Ctrl+Alt+F9 — только при `--bench`.
- `window.rs`: показ = позиция от `iskra_sys::foreground_monitor_rect()` (до set_focus!) →
  `set_position(PhysicalPosition)` → `show()` → `set_focus()` (порядок спайка);
  hide-on-blur через `on_window_event(Focused(false))` (порт); mica; физические px —
  DPI-корректно по построению.
- `tray.rs`: `TrayIconBuilder` + `include_image!("icons/32x32.png")`, меню: «Показать», «Настройки»
  (show + emit `nav://settings`), `CheckMenuItem` «Автозапуск» (переключение → `iskra_sys::autostart`,
  синхронизация галочки), sep, «Выход».
- `commands.rs`: тонкие обёртки над core-settings; `update_settings` применяет hotkey-remap
  и autostart, шлёт `settings://changed`.
- Done when: `build.cmd` зелёный; ручная проверка: Alt+Space → окно на мониторе, где фокус;
  смена хоткея правкой settings.json + рестарт работает;
  `reg query HKCU\...\Run /v Iskra` после включения автозапуска из трея показывает путь к exe;
  выключение — удаляет; трей-меню полностью функционален.

### Шаг 5. `[P]` UI: типизированный IPC + экран настроек + тема (после шага 3, параллельно шагу 4)
Файлы: `ui/src/ipc/{types,client}.ts`, `components/{ResultList,SettingsView}.tsx`, `theme.css`, `App.tsx`.
- `types.ts` — дословное зеркало `ipc.rs` (комментарий «при изменении Rust-типов править здесь»);
  `client.ts` — `invoke`/`listen` с типами.
- `SettingsView`: селект dark/light (мгновенно применяет `data-theme` на `<html>` + `update_settings`),
  input хоткея + «Сохранить» (ошибка HotkeyBusy → инлайн-сообщение + показать фактический хоткей
  из `get_runtime_info`), checkbox автозапуска (начальное состояние из Settings).
- `ResultList`: пустой список-скелетон (5 приглушённых строк) — заготовка фазы 2.
- Done when: `npm run build` (tsc + vite) зелёный; в запущенном приложении: тема переключается
  мгновенно и сохраняется после рестарта, смена хоткея на свободный (напр. Ctrl+Alt+K) работает сразу,
  на занятый — ошибка и откат, чекбокс автозапуска синхронизирован с реестром.

### Шаг 6. `[S]` Bench + замеры приёмки (после шагов 4–5, только release)
Файлы: `app/src/bench.rs` (порт `--bench N` из спайка: SendInput Ctrl+Alt+F9, hidden→shown,
микросекунды в `latency.log`), README-секция «Сборка и замеры».
- Done when: `iskra.exe --bench 50` на релизной сборке даёт p95 (nearest-rank) **< 100 мс**;
  RAM private WS (PowerShell-методика из RESULTS.md: дерево iskra.exe + msedgewebview2, 10 с,
  `WorkingSetPrivate`) **≤ 80 МБ**.

---

## 3. Риски и план Б

| # | Риск | План Б |
|---|------|--------|
| 1 | **GPU-off из кода**: `set_var` в main работает, только если WebView2 читает env при создании окна, а не при старте процесса. В спайке env ставился снаружи до запуска — **ASSUMPTION**, что перенос внутрь main() эквивалентен (Builder создаёт окно позже). | Задавать `additional_browser_args` в `WebviewWindowBuilder` (окно строить из Rust, не из conf), продублировав дефолтные флаги wry (`--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`). План В: второй конфиг `tauri.release.conf.json` + `--config` при релизной сборке. Проверка шага 1/4: замер RAM на релизной сборке. |
| 2 | **Mica + transparent + GPU-off**: софтверный рендер может дать чёрный фон/артефакты (визуально в спайке mica не проверялась). | Первый же визуальный чек шага 4. Если чёрный: отказаться от vibrancy в релизе, непрозрачный тёмный фон через CSS (`theme.css`) — RAM-критерий не страдает (флаги остаются). |
| 3 | **Трей API Tauri 2** (`tauri::tray`, `tauri::menu`, фича `tray-icon`): стабильное ядро 2.x, но точечные переименования между минорками (`menu_on_left_click`). | Если CheckMenuItem-синхронизация капризна — пересобирать меню при изменении состояния (дёшево). Крайний случай: текстовый пункт «Автозапуск: вкл/выкл». |
| 4 | **Remap хоткея через плагин** (unregister→register в рантайме) — не проверялся в спайке, возможны квирки плагина. | Сырой `RegisterHotKey` + поток с WM_HOTKEY в `iskra-sys` (raw-Win32 в спайке доказан subclassing'ом wndproc); интерфейс `HotkeyService` не меняется. |
| 5 | **Активный монитор**: `GetForegroundWindow` в момент хоткея может вернуть NULL/наше окно; многообразие DPI-скейлов. | Цепочка foreground→cursor→primary заложена с самого начала (шаг 2). Проверить на 2 мониторах с разным масштабом (100%/150%). |
| 6 | `std::env::set_var` в **edition 2024 — unsafe**. | Зафиксировать `edition = "2021"` во всех крейтах (спайк на 2021). |
| 7 | **RAM ≤ 80 МБ с React** вместо статического HTML (было 64–67). | Запас ~13 МБ; UI минималистичный (только react/react-dom). Если не влезаем — профилировать дерево webview, убрать лишний чанк; `--process-per-site` в спайке эффекта не дал — не использовать. |
| 8 | **Порядок сборки**: `cargo build` падает без `ui/dist` (frontendDist). | `build.cmd` оркеструет: npm install/build → cargo; проверка наличия `ui/dist` внутри скрипта. |
| 9 | Полный экран **exclusive-fullscreen** (игры) поверх не перекрывается — ограничение композитора Windows. | Критерий проверяем на F11-fullscreen браузера/плеера (всегда поверх через alwaysOnTop). Эксклюзивный режим — задокументировать как известное ограничение, фазу не блокирует. |

---

## 4. Файлы для изменения / новые

- Изменяемые: `.gitignore`, `README.md` (секция сборки).
- Всё остальное — новые файлы: см. дерево в §1 (корень: `Cargo.toml`, `build.cmd`;
  `crates/iskra-core/*`, `crates/iskra-sys/*`, `app/*`, `ui/*`).
  `spikes/` и `docs/specs/` воркером не изменяются (чекбоксы фазы 1 в плане — вручную после приёмки).

## 5. Верификация (чек-лист приёмки против критерия фазы)

Сборка: `build.cmd` → `target/release/iskra.exe` (dev: `npm --prefix ui run dev` + `cargo run` с devUrl).

- [x] **`Alt+Space` → окно p95 < 100 мс**: `iskra.exe --bench 50` (release), nearest-rank по
      `latency.log`; фактически p95 = 11,56 мс (nearest-rank, rank 48 из 50) —
      PASS, запас ~8,7×.
- [ ] **Скрытие при blur**: клик по другому окну / Alt+Tab → окно скрыто; повторный хоткей
      показывает (toggle).
- [x] **RAM idle ≤ 80 МБ**: private WS дерева процессов через 10 с простоя на релизной сборке
      (GPU-off активен; чёрный фон — не признак, замер обязателен); фактически
      70,0–70,5 МБ (70,04 / 70,27 / 70,45 МБ, 3 выборки) — PASS.
- [ ] **Автозапуск работает**: включение из трея/настроек →
      `reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v Iskra` = путь exe;
      выключение удаляет значение.
- [ ] **Трей работает**: иконка видна; Показать/Настройки/Автозапуск✓/Выход — все пункты
      функциональны, галочка синхронна с реестром.
- [ ] Доп. из задач фазы: переназначение хоткея из настроек (свободный — работает, занятый —
      ошибка + откат + показ фактического); окно появляется на **активном** мониторе (фокус на
      втором мониторе → окно там же, центр-верх); F11-fullscreen браузера — лончер поверх;
      при 150% масштабе размер/позиция корректны.

**Примечание (2026-09-30):** ручные проверки (blur-hide, трей-меню, автозапуск
из трея, активный монитор, remap хоткея из UI, F11-fullscreen) не выполнялись
и остаются открытыми.

## 6. Распределение по прогонам воркера

- **Прогон 1:** шаги 0–1 (скелет, собирается), затем параллельно 2 и 3.
- **Прогон 2:** шаг 4 → (параллельно 5) → шаг 6 + чек-лист.
