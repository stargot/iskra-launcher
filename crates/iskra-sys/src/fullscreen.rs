//! Детект fullscreen у foreground-окна: rect окна == rect монитора && нет WS_CAPTION.
//! Замечание (риск 9 плана): exclusive-fullscreen (игры) композитором Windows
//! перекрывается не всегда — критерий фазы проверяется на F11-fullscreen браузера/плеера.

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GWL_STYLE, WS_CAPTION,
};

use super::monitor;

/// True, если foreground-окно развёрнуто на весь монитор без рамки окна
/// (эвристика из плана шага 2: rect == monitor rect && стили без WS_CAPTION).
pub fn is_foreground_fullscreen() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let Some(mon) = monitor::monitor_rect_from_hwnd(hwnd) else {
            return false;
        };

        let mut rc = RECT::default();
        if GetWindowRect(hwnd, &mut rc).is_err() {
            return false;
        }
        let covers_monitor = rc.left == mon.x
            && rc.top == mon.y
            && (rc.right - rc.left) == mon.width
            && (rc.bottom - rc.top) == mon.height;
        if !covers_monitor {
            return false;
        }

        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        style & WS_CAPTION.0 as isize == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// На рабочем столе (IDE/терминал в окне) fullscreen почти наверняка нет — но
    /// функция обязана просто возвращать bool без паники. Позитивный кейс
    /// (F11-браузер) проверяется вручную на приёмке фазы.
    #[test]
    fn returns_bool_without_panicking() {
        let _ = is_foreground_fullscreen();
    }
}
