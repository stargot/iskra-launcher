//! Ввод клавиш через SendInput (шаг 2 Фазы 3): Ctrl+V — завершающее звено
//! цепочки вставки (D6), юникод-ввод — запас для сниппетов без клипборда (D9).
//! Единственное место workspace, посылающее ввод, кроме keys.rs (ADR 7).
//!
//! Ограничение (риск 2, UIPI): в elevated-окна из не-elevated Iskra нажатия
//! не дойдут — это задокументированное ограничение, не баг.
//!
//! Обычные тесты НЕ шлют реальный ввод (чтобы не печатать в окно пользователя):
//! проверяются только построители INPUT-массивов и пустой ввод. Реальные
//! нажатия — в #[ignore]-тестах для ручной приёмки.

use std::io;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VK_CONTROL, VK_V, VIRTUAL_KEY,
};

/// Послать Ctrl+V в активное окно (VK_CONTROL down → V down → V up → VK_CONTROL up
/// одним атомарным вызовом SendInput). Возвращает ошибку, если система приняла
/// не все события (может быть при блокировке ввода UIPI/фокус-украдкой).
pub fn send_ctrl_v() -> io::Result<()> {
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
        mk(VK_V, false),
        mk(VK_V, true),
        mk(VK_CONTROL, true),
    ];
    send_all(&seq)
}

/// Напечатать текст как юникод-события (KEYEVENTF_UNICODE, по одному на UTF-16
/// кодовую единицу; суррогатные пары — двумя событиями). Запасной путь вставки
/// сниппетов, когда клипборд трогать нельзя.
pub fn send_text(text: &str) -> io::Result<()> {
    send_all(&unicode_inputs(text))
}

/// Одним вызовом SendInput: частично применённая последовательность нажатий
/// (например Ctrl зажат без V) хуже полностью неприменённой.
fn send_all(seq: &[INPUT]) -> io::Result<()> {
    let sent = unsafe { SendInput(seq, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == seq.len() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("SendInput принял {sent} из {} событий", seq.len()),
        ))
    }
}

/// UTF-16 кодовая единица → INPUT с KEYEVENTF_UNICODE (вниз) или
/// KEYEVENTF_UNICODE|KEYEVENTF_KEYUP (вверх). wVk обязан быть 0.
fn unicode_inputs(text: &str) -> Vec<INPUT> {
    text.encode_utf16()
        .flat_map(|unit| {
            let mk = |up: bool| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: unit,
                        dwFlags: if up {
                            KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                        } else {
                            KEYEVENTF_UNICODE
                        },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            [mk(false), mk(true)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Построитель юникод-событий: по паре down/up на каждую UTF-16 единицу
    /// (эмодзи = суррогатная пара = 4 события), флаги и wScan корректны.
    #[test]
    fn unicode_inputs_match_utf16_units() {
        let text = "aé🚀";
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(units.len(), 4, "'a', 'é', + суррогатная пара");

        let inputs = unicode_inputs(text);
        assert_eq!(inputs.len(), units.len() * 2, "каждая единица = down+up");

        for (i, unit) in units.iter().enumerate() {
            // SAFETY: чтение полей union INPUT — только ki (мы заполняли ki).
            let (down, up) = unsafe { (&inputs[i * 2].Anonymous.ki, &inputs[i * 2 + 1].Anonymous.ki) };
            assert_eq!(down.wScan, *unit, "wScan вниз = кодовая единица");
            assert_eq!(up.wScan, *unit, "wScan вверх = кодовая единица");
            assert_eq!(down.wVk.0, 0, "wVk обязан быть 0 для KEYEVENTF_UNICODE");
            assert_eq!(down.dwFlags, KEYEVENTF_UNICODE);
            assert_eq!(up.dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
        }
    }

    /// Пустая строка — 0 событий, SendInput принимает тривиально (безопасный
    /// тест: реального ввода нет).
    #[test]
    fn send_text_empty_is_ok() {
        send_text("").expect("пустой ввод должен проходить без ошибок");
    }

    /// РУЧНАЯ приёмка цепочки вставки (D6): фокус в текстовое поле, затем
    /// `cargo test -p iskra-sys send_ctrl_v_manual -- --ignored --nocapture`
    /// — через 3 c в поле должна появиться вставка из клипборда.
    #[test]
    #[ignore]
    fn send_ctrl_v_manual() {
        eprintln!("3 секунды на фокус в текстовое поле…");
        std::thread::sleep(std::time::Duration::from_secs(3));
        send_ctrl_v().expect("SendInput Ctrl+V");
    }

    /// РУЧНАЯ приёмка юникод-ввода: та же процедура, печатает строку.
    #[test]
    #[ignore]
    fn send_text_manual() {
        eprintln!("3 секунды на фокус в текстовое поле…");
        std::thread::sleep(std::time::Duration::from_secs(3));
        send_text("iskra send_text ✓").expect("SendInput юникод-текст");
    }
}
