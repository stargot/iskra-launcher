//! Запись текста в буфер обмена (CF_UNICODETEXT) — сторона app.
//!
//! iskra-sys по условиям шага 4 не расширяется, а `ItemAction::CopyText` по плану
//! исполняется «на стороне app» (см. док-комментарий search/types.rs) — поэтому
//! минимальный WinAPI-путь живёт здесь: OpenClipboard → EmptyClipboard →
//! SetClipboardData(GlobalAlloc(GMEM_MOVEABLE, UTF-16 + NUL)).
//!
//! Владение выделенным блоком после успешного SetClipboardData переходит системе —
//! освобождать его (GlobalFree) нельзя. CloseClipboard — во всех путях.

use std::io;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

use iskra_core::logging;

/// Стиль ошибки единый с iskra-sys (io::ErrorKind::Other + контекст операции).
fn err(op: &str, e: windows::core::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{op}: {e}"))
}

/// Скопировать `text` в буфер обмена как Unicode-текст.
pub fn copy_text(text: &str) -> io::Result<()> {
    unsafe {
        OpenClipboard(None).map_err(|e| err("OpenClipboard", e))?;
        let result = (|| {
            EmptyClipboard().map_err(|e| err("EmptyClipboard", e))?;

            let mut utf16: Vec<u16> = text.encode_utf16().collect();
            utf16.push(0); // NUL-терминатор
            let bytes = utf16.len() * 2;

            let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| err("GlobalAlloc", e))?;
            let dst = GlobalLock(hglobal);
            if dst.is_null() {
                let _ = GlobalUnlock(hglobal);
                return Err(io::Error::new(io::ErrorKind::Other, "GlobalLock вернул null"));
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr().cast::<u8>(), dst.cast::<u8>(), bytes);
            let _ = GlobalUnlock(hglobal);

            // HANDLE и HGLOBAL — один и тот же базовый указатель; после успешного
            // SetClipboardData блоком владеет система (освобождать нельзя).
            SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hglobal.0)))
                .map(|_| ())
                .map_err(|e| err("SetClipboardData", e))
        })();
        if let Err(e) = CloseClipboard() {
            logging::warn(&format!("clipboard: CloseClipboard: {e}"));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    /// Автотест НЕ трогает реальный буфер обмена пользователя (побочный эффект).
    /// Проверяем компиляцию и константу формата; запись — ручная приёмка (шаг 6).
    #[test]
    fn cf_unicode_text_is_13() {
        assert_eq!(windows::Win32::System::Ole::CF_UNICODETEXT.0, 13);
    }
}
