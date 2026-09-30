//! Автозапуск: значение в `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! Прод-имя значения — "Iskra"; тесты работают с "IskraPhase1Test" (критерий Done шага 2).

use std::io;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};

const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
/// Имя значения автозапуска Iskra (план шага 2).
const VALUE_NAME: &str = "Iskra";

/// Включить автозапуск: значение "Iskra" = путь к текущему exe (в кавычках —
/// безопасно для путей с пробелами). Повторный вызов перезаписывает.
pub fn set() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    set_value(VALUE_NAME, &exe.to_string_lossy())
}

/// Выключить автозапуск (идемпотентно: нет значения/ключа — уже Ok).
pub fn remove() -> io::Result<()> {
    remove_value(VALUE_NAME)
}

/// Включён ли автозапуск (есть ли REG_SZ-значение "Iskra" в Run).
pub fn is_enabled() -> bool {
    is_enabled_value(VALUE_NAME)
}

// --- параметризованные версии (для тестов и шага 4) ---

/// Записать значение `name` = `command` (REG_SZ) в HKCU Run.
pub fn set_value(name: &str, command: &str) -> io::Result<()> {
    let quoted = format!("\"{command}\"");
    let data_utf16: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
    let mut data: Vec<u8> = Vec::with_capacity(data_utf16.len() * 2);
    for w in &data_utf16 {
        data.extend_from_slice(&w.to_le_bytes());
    }

    let subkey = wide(RUN_SUBKEY);
    let vname = wide(name);
    unsafe {
        let mut hkey = HKEY::default();
        let code = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        );
        if code != ERROR_SUCCESS {
            return Err(io_from_win32("RegCreateKeyExW", code));
        }
        let code = RegSetValueExW(
            hkey,
            PCWSTR(vname.as_ptr()),
            None,
            REG_SZ,
            Some(data.as_slice()),
        );
        let close = RegCloseKey(hkey);
        if code != ERROR_SUCCESS {
            return Err(io_from_win32("RegSetValueExW", code));
        }
        if close != ERROR_SUCCESS {
            return Err(io_from_win32("RegCloseKey", close));
        }
    }
    Ok(())
}

/// Удалить значение `name` из HKCU Run. Нет значения/ключа → Ok (идемпотентно).
pub fn remove_value(name: &str) -> io::Result<()> {
    let subkey = wide(RUN_SUBKEY);
    let vname = wide(name);
    unsafe {
        let mut hkey = HKEY::default();
        let code = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if code != ERROR_SUCCESS {
            return Err(io_from_win32("RegOpenKeyExW", code));
        }
        let code = RegDeleteValueW(hkey, PCWSTR(vname.as_ptr()));
        let _ = RegCloseKey(hkey);
        match code {
            ERROR_SUCCESS => Ok(()),
            ERROR_FILE_NOT_FOUND => Ok(()),
            c => Err(io_from_win32("RegDeleteValueW", c)),
        }
    }
}

/// Есть ли REG_SZ-значение `name` в HKCU Run.
pub fn is_enabled_value(name: &str) -> bool {
    matches!(get_value(name), Some(_))
}

/// Прочитать REG_SZ-значение `name` из HKCU Run (None — нет значения/не строка).
pub fn get_value(name: &str) -> Option<String> {
    let subkey = wide(RUN_SUBKEY);
    let vname = wide(name);
    unsafe {
        let mut hkey = HKEY::default();
        let code = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &mut hkey,
        );
        if code != ERROR_SUCCESS {
            return None;
        }
        let mut vtype = REG_SZ;
        let mut size: u32 = 0;
        let code = RegQueryValueExW(
            hkey,
            PCWSTR(vname.as_ptr()),
            None,
            Some(&mut vtype),
            None,
            Some(&mut size),
        );
        if code != ERROR_SUCCESS || vtype != REG_SZ || size == 0 {
            let _ = RegCloseKey(hkey);
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let code = RegQueryValueExW(
            hkey,
            PCWSTR(vname.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        );
        let _ = RegCloseKey(hkey);
        if code != ERROR_SUCCESS {
            return None;
        }
        let units: Vec<u16> = buf[..size as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Some(
            String::from_utf16_lossy(&units)
                .trim_end_matches('\0')
                .to_string(),
        )
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn io_from_win32(op: &str, code: WIN32_ERROR) -> io::Error {
    io::Error::new(
        io::ErrorKind::Other,
        format!("{op} failed: win32 error {}", code.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Имя тестового значения из критерия Done шага 2 (план).
    const TEST_NAME: &str = "IskraPhase1Test";

    /// Пишет/читает/удаляет РЕАЛЬНОЕ значение в HKCU Run (с очисткой до и после).
    /// После прогона теста значение удаляется — автозапуск пользователя не затрагивается.
    #[test]
    fn set_query_remove_in_real_hkcu_run() {
        // очистка после возможного прошлого падения теста
        let _ = remove_value(TEST_NAME);
        assert!(!is_enabled_value(TEST_NAME), "тестовое значение не должно существовать до теста");

        let fake_path = r"C:\Program Files\Iskra\iskra.exe";
        set_value(TEST_NAME, fake_path).expect("set_value должен писать в HKCU Run");
        assert!(
            is_enabled_value(TEST_NAME),
            "после set_value значение должно читаться"
        );

        let stored = get_value(TEST_NAME).expect("get_value после set_value");
        assert_eq!(stored, format!("\"{fake_path}\""), "значение должно быть путём в кавычках");

        remove_value(TEST_NAME).expect("remove_value должен удалять значение");
        assert!(!is_enabled_value(TEST_NAME), "после remove_value значения быть не должно");

        // повторный remove — идемпотентен (критерий: «с очисткой»)
        remove_value(TEST_NAME).expect("повторный remove_value должен быть Ok");
    }
}
