//! Активный монитор: цепочка foreground → cursor → primary (риск 5 плана).
//! Все координаты — физические пиксели, DPI-корректно по построению
//! (rcMonitor из GetMonitorInfoW всегда в физических px).

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY, HMONITOR,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow};

/// Прямоугольник монитора в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Rect монитора, где сейчас foreground-окно. Цепочка:
/// 1. `GetForegroundWindow` → `MonitorFromWindow(NEAREST)`;
/// 2. foreground NULL/недоступен → `GetCursorPos` → `MonitorFromPoint(NEAREST)`;
/// 3. и это не вышло → primary-монитор; `None` только если нет вообще никакого монитора.
pub fn foreground_monitor_rect() -> Option<Rect> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if !hwnd.0.is_null() {
            if let Some(rect) = monitor_rect_from_hwnd(hwnd) {
                return Some(rect);
            }
        }
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_ok() {
            let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            if let Some(rect) = rect_from_hmonitor(hmon) {
                return Some(rect);
            }
        }
        rect_from_hmonitor(MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY))
    }
}

/// Rect монитора, ближайшего к окну (физические px). NULL hwnd → None.
pub fn monitor_rect_from_hwnd(hwnd: HWND) -> Option<Rect> {
    if hwnd.0.is_null() {
        return None;
    }
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        rect_from_hmonitor(hmon)
    }
}

fn rect_from_hmonitor(hmon: HMONITOR) -> Option<Rect> {
    if hmon.0.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        rcMonitor: RECT::default(),
        rcWork: RECT::default(),
        dwFlags: 0,
    };
    unsafe {
        if GetMonitorInfoW(hmon, &mut info).as_bool() {
            Some(Rect {
                x: info.rcMonitor.left,
                y: info.rcMonitor.top,
                width: info.rcMonitor.right - info.rcMonitor.left,
                height: info.rcMonitor.bottom - info.rcMonitor.top,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// На рабочем столе foreground/cursor/primary всегда доступны → Some с валидным rect.
    /// (критерий Done шага 2: «тест monitor возвращает Some на рабочем столе»)
    #[test]
    fn foreground_monitor_returns_some_on_desktop() {
        let rect = foreground_monitor_rect().expect("монитор должен найтись на рабочем столе");
        assert!(rect.width > 0, "ширина монитора должна быть > 0, got {rect:?}");
        assert!(rect.height > 0, "высота монитора должна быть > 0, got {rect:?}");
    }
}
