//! Запуск приложений, файлов и URI через ShellExecuteW (шаг 3 Фазы 2, D1).
//!
//! Один вход для всех действий лончера: exe (`notepad.exe`), документы
//! (`отчёт.docx`), URI (`ms-settings:display`, `https://...`) — обработчик
//! выбирается ассоциацией Windows. ShellExecuteW — асинхронный по своей природе
//! (возвращает код успеха, не ждёт завершения), поэтому `launch*` не блокирует.
//!
//! НАХОДКА шага 3: на Windows 11 упакованные приложения (Notepad и др.) после
//! запуска НЕ открываются повторно через `OpenProcess(pid)` — returned
//! ERROR_INVALID_PARAMETER даже при живом процессе. Единственный надёжный
//! способ завершить/дождаться такой процесс — хэндл от `SEE_MASK_NOCLOSEPROCESS`
//! (`LaunchedProcess::terminate`), поэтому он и есть канонический API.

use std::io;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{GetProcessId, TerminateProcess};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Запустить `file` (exe/файл/URI) с обработчиком по умолчанию.
///
/// Примеры: `"notepad.exe"`, `"ms-settings:display"`, `"https://example.com"`.
pub fn launch(file: &str) -> io::Result<()> {
    launch_params(file, "")
}

/// Запустить `file` с аргументами `params` (например exe + ключи командной строки).
pub fn launch_params(file: &str, params: &str) -> io::Result<()> {
    let file_w = wide(file);
    let params_w = wide(params);
    let hinst = unsafe {
        ShellExecuteW(
            None,
            PCWSTR::null(), // глагол по умолчанию ("open")
            PCWSTR(file_w.as_ptr()),
            PCWSTR(params_w.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Документация: успех = возвращённое значение > 32, иначе SE_ERR_*-код.
    let code = hinst.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(se_err("ShellExecuteW", code))
    }
}

/// Запустить `file` и получить дескриптор процесса: PID + terminate().
/// Для целей без процесса (URI/документы, открытые чужим обработчиком) вернёт
/// ошибку — используйте [`launch`]. Хэндл закрывается в Drop.
pub fn launch_process(file: &str, params: &str) -> io::Result<LaunchedProcess> {
    let file_w = wide(file);
    let params_w = wide(params);
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file_w.as_ptr()),
        lpParameters: PCWSTR(params_w.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut sei).map_err(|e| err("ShellExecuteExW", e))?;
        if sei.hProcess.is_invalid() {
            // SEE_MASK_NOCLOSEPROCESS не даёт хэндла для целей без процесса
            // (URI, ассоциированные документы) — вызывающему нужен launch().
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "ShellExecuteExW: цель без процесса (URI/документ), PID недоступен",
            ));
        }
        Ok(LaunchedProcess {
            pid: GetProcessId(sei.hProcess),
            handle: sei.hProcess,
        })
    }
}

/// Запущенный процесс: PID + хэндл от ShellExecuteExW (закрывается в Drop).
///
/// [`terminate`](Self::terminate) — единственный надёжный способ завершить
/// упакованное (MSIX) приложение: повторный OpenProcess по его PID даёт
/// ERROR_INVALID_PARAMETER (см. шапку модуля).
pub struct LaunchedProcess {
    pid: u32,
    handle: HANDLE,
}

impl LaunchedProcess {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Завершить процесс (код выхода 0). Не ждёт выхода — используйте
    /// WaitForSingleObject при необходимости (шаг 4).
    pub fn terminate(&self) -> io::Result<()> {
        unsafe { TerminateProcess(self.handle, 0).map_err(|e| err("TerminateProcess", e)) }
    }

    /// Хэндл процесса (для ожидания/статусов, без передачи владения).
    pub fn handle(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for LaunchedProcess {
    fn drop(&mut self) {
        unsafe { let _ = CloseHandle(self.handle); }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn err(op: &str, e: windows::core::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{op} failed: {e}"))
}

/// SE_ERR_*-коды ShellExecuteW (значения ≤ 32).
fn se_err(op: &str, code: isize) -> io::Error {
    let what = match code {
        0 => "out of memory or resources",
        2 => "file not found (SE_ERR_FNF)",
        3 => "path not found (SE_ERR_PNF)",
        5 => "access denied (SE_ERR_ACCESSDENIED)",
        6 => "security error (SE_ERR_ACCESSDENIED legacy)",
        8 => "not enough memory (SE_ERR_OOM)",
        10 => "bad format (ERROR_BAD_FORMAT)",
        11 => "invalid exe (SE_ERR_BADFORMAT)",
        26 => "sharing violation (SE_ERR_SHARE)",
        27 => "incomplete association (SE_ERR_ASSOCINCOMPLETE)",
        28 => "DDE timeout (SE_ERR_DDETIMEOUT)",
        29 => "DDE failed (SE_ERR_DDEFAIL)",
        30 => "DDE busy (SE_ERR_DDEBUSY)",
        31 => "no association for the extension (SE_ERR_NOASSOC)",
        32 => "DLL not found (SE_ERR_DLLNOTFOUND)",
        _ => "unknown error code",
    };
    io::Error::new(io::ErrorKind::Other, format!("{op} failed: {what} (code {code})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Done-критерий шага 3: ShellExecuteW открывает notepad.exe.
    /// Тест сам завершает запущенный им процесс — по хэндлу от ShellExecuteExW
    /// (повторный OpenProcess по PID упакованного Notepad невозможен, шапка модуля).
    #[test]
    fn launch_opens_notepad_and_kills_it() {
        let proc = launch_process("notepad.exe", "").expect("notepad.exe должен запуститься");
        assert!(proc.pid() != 0, "PID запущенного notepad.exe должен быть ненулевым");

        // Дать упакованному Notepad (Win11) завершить активацию: terminate
        // в окне активации может быть отклонён с ACCESS_DENIED.
        std::thread::sleep(std::time::Duration::from_millis(2000));

        // Ретраи: TerminateProcess по хэндлу от ShellExecuteExW — единственный
        // надёжный способ для упакованных приложений (шапка модуля).
        let mut last = None;
        for _ in 0..10 {
            match proc.terminate() {
                Ok(()) => {
                    last = None;
                    break;
                }
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
        if let Some(e) = last {
            panic!("запущенный notepad.exe (pid {}) должен завершаться: {e}", proc.pid());
        }
    }

    /// Несуществующая цель → Err (без диалога: SEE_MASK_FLAG_NO_UI).
    #[test]
    fn launch_missing_file_is_err() {
        let err = launch(r"C:\definitely_missing_iskra_test_xyz.exe")
            .expect_err("несуществующий файл должен давать ошибку");
        assert!(err.to_string().contains("file not found"), "неожиданная ошибка: {err}");
    }

    /// PID-вариант тоже репортит ошибку для несуществующей цели.
    #[test]
    fn launch_process_missing_file_is_err() {
        assert!(
            launch_process(r"C:\definitely_missing_iskra_test_xyz.exe", "").is_err(),
            "несуществующий файл должен давать ошибку"
        );
    }
}
