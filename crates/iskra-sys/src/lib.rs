//! iskra-sys — безопасные обёртки windows-rs (ADR 7): единственное место в workspace
//! с `unsafe`/WinAPI. Без Tauri. Реализации — шаг 2 Фазы 1
//! (docs/plans/2026-09-29-phase1-implementation.md) и шаг 3 Фазы 2
//! (docs/plans/2026-09-30-phase2-implementation.md): shell/иконки/питание.

pub mod autostart;
pub mod fullscreen;
pub mod icons;
pub mod keys;
pub mod monitor;
pub mod power;
pub mod shell;

#[cfg(test)]
mod tests {
    use crate::icons;

    /// Roundtrip шага 3 (крейт-уровень): извлечение иконки → PNG-байты →
    /// декодирование обратно. notepad.exe есть на любой Windows.
    #[test]
    fn icon_extract_png_roundtrip() {
        let _guard = icons::ICON_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = icons::extract_png_bytes(r"C:\Windows\System32\notepad.exe", 32)
            .expect("иконка notepad.exe должна извлекаться");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "должна быть PNG-сигнатура");
        let img = image::load_from_memory(&bytes).expect("PNG должен декодироваться обратно");
        assert!(
            img.width() > 0 && img.height() > 0,
            "декодированный PNG должен быть непустым"
        );
    }
}
