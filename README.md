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
