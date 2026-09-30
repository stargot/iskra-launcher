//! iskra-sys — безопасные обёртки windows-rs (ADR 7): единственное место в workspace
//! с `unsafe`/WinAPI. Без Tauri. Реализации — шаг 2 Фазы 1
//! (docs/plans/2026-09-29-phase1-implementation.md).

pub mod autostart;
pub mod fullscreen;
pub mod keys;
pub mod monitor;
