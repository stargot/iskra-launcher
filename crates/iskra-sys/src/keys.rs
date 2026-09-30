//! Отправка клавиш для bench-режима: Ctrl+Alt+F9 через SendInput — реальные
//! системные события, полный путь RegisterHotKey (порт из spikes/tauri-app).
//! Единственное место workspace, посылающее ввод; держим в iskra-sys (ADR 7).

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VK_CONTROL, VK_F9, VK_MENU, VIRTUAL_KEY,
};

/// Послать Ctrl+Alt+F9 (bench-хоткей) как реальные системные нажатия
/// (down F9 → up F9 → отпускание модификаторов, как в спайке).
pub fn send_ctrl_alt_f9() {
    let mk = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let seq = [
        mk(VK_CONTROL, false),
        mk(VK_MENU, false),
        mk(VK_F9, false),
        mk(VK_F9, true),
        mk(VK_MENU, true),
        mk(VK_CONTROL, true),
    ];
    unsafe {
        SendInput(&seq, std::mem::size_of::<INPUT>() as i32);
    }
}
