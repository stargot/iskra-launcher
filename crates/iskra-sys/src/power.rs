//! Питание и сессия (шаг 3 Фазы 2): блокировка станции, выключение мониторов,
//! сон, перезагрузка/выключение (+AdjustTokenPrivileges — риск 4), очистка корзины.
//!
//! Все функции выполняют РЕАЛЬНОЕ действие: автотесты их не вызывают
//! (только компиляция/сигнатуры); реальные вызовы — ручная приёмка и
//! действия пользователя из UI (риск 9: подтверждение Enter).

use std::io;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LPARAM, WPARAM};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::{
    ExitWindowsEx, LockWorkStation, EWX_REBOOT, EWX_SHUTDOWN, SHTDN_REASON_FLAG_PLANNED,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SC_MONITORPOWER, SendMessageW, WM_SYSCOMMAND,
};

/// Заблокировать рабочую станцию (Win+L).
pub fn lock() -> io::Result<()> {
    unsafe { LockWorkStation().map_err(|e| err("LockWorkStation", e)) }
}

/// Выключить мониторы: `SendMessage(HWND_BROADCAST, WM_SYSCOMMAND,
/// SC_MONITORPOWER, 2)`. Включение обратно — движение мыши/клавиатура.
pub fn monitor_off() -> io::Result<()> {
    // HWND_BROADCAST может блокироваться на зависшем окне верхнего уровня —
    // допустимо по плану (вызов из UI-потока пользователя, а не из сервисного).
    unsafe {
        SendMessageW(
            HWND_BROADCAST,
            WM_SYSCOMMAND,
            Some(WPARAM(SC_MONITORPOWER as usize)),
            Some(LPARAM(2)), // 2 = off, 1 = low power, -1 = on
        );
    }
    Ok(())
}

/// Сон (suspend): `SetSuspendState(hibernate=false, force=false, disableWake=false)`
/// через PowrProf. Hibernate не используем (спека: «сон»).
pub fn sleep() -> io::Result<()> {
    let ok = unsafe { SetSuspendState(false, false, false) };
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("SetSuspendState failed: win32 error {}", io::Error::last_os_error()),
        ))
    }
}

/// Перезагрузка: ExitWindowsEx(EWX_REBOOT) с предварительным включением
/// привилегии SeShutdownPrivilege (риск 4: без AdjPriv молча не срабатывает).
pub fn restart() -> io::Result<()> {
    enable_shutdown_privilege()?;
    exit_windows(EWX_REBOOT, "ExitWindowsEx(EWX_REBOOT)")
}

/// Выключение компьютера: ExitWindowsEx(EWX_SHUTDOWN) + привилегия.
pub fn shutdown() -> io::Result<()> {
    enable_shutdown_privilege()?;
    exit_windows(EWX_SHUTDOWN, "ExitWindowsEx(EWX_SHUTDOWN)")
}

/// Включить привилегию SeShutdownPrivilege у токена текущего процесса.
/// Сама по себе безопасна (реальная перезагрузка не начинается) — тестируется.
pub fn enable_shutdown_privilege() -> io::Result<()> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .map_err(|e| err("OpenProcessToken", e))?;

        let mut tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: Default::default(),
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let lookup = LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut tp.Privileges[0].Luid);
        if lookup.is_err() {
            let _ = CloseHandle(token);
            return lookup.map_err(|e| err("LookupPrivilegeValueW", e));
        }

        let adjust = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        let _ = CloseHandle(token);
        adjust.map_err(|e| err("AdjustTokenPrivileges", e))
    }
}

/// Очистить корзину (все диски при `root=None`). `confirm=false` — без диалогов.
pub fn empty_recycle_bin(confirm: bool) -> io::Result<()> {
    let flags = if confirm {
        0
    } else {
        SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND
    };
    unsafe {
        SHEmptyRecycleBinW(None, PCWSTR::null(), flags).map_err(|e| err("SHEmptyRecycleBinW", e))
    }
}

fn exit_windows(flags: windows::Win32::System::Shutdown::EXIT_WINDOWS_FLAGS, op: &str) -> io::Result<()> {
    unsafe {
        ExitWindowsEx(flags, SHTDN_REASON_FLAG_PLANNED).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("{op} failed: {e}"))
        })
    }
}

fn err(op: &str, e: windows::core::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{op} failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Сигнатуры: функции существуют и типизированы, НО не вызываются —
    /// реальные lock/sleep/restart/monitor-off в тестах запрещены (план шага 3).
    #[test]
    fn signatures_compile_without_calling() {
        let _lock: fn() -> io::Result<()> = lock;
        let _monitor_off: fn() -> io::Result<()> = monitor_off;
        let _sleep: fn() -> io::Result<()> = sleep;
        let _restart: fn() -> io::Result<()> = restart;
        let _shutdown: fn() -> io::Result<()> = shutdown;
        let _empty: fn(bool) -> io::Result<()> = empty_recycle_bin;
    }

    /// Риск 4: AdjPriv SeShutdownPrivilege можно проверять безопасно —
    /// включение привилегии само по себе ничего не перезагружает.
    #[test]
    fn enable_shutdown_privilege_succeeds() {
        enable_shutdown_privilege().expect("SeShutdownPrivilege должна включаться у обычного пользователя");
    }

    // --- ручная приёмка (план: «остальные — ручная приёмка»); cargo test их не запускает ---

    #[test]
    #[ignore = "ручная приёмка: реально блокирует станцию"]
    fn manual_lock() {
        lock().unwrap();
    }

    #[test]
    #[ignore = "ручная приёмка: реально гасит мониторы"]
    fn manual_monitor_off() {
        monitor_off().unwrap();
    }

    #[test]
    #[ignore = "ручная приёмка: реально усыпляет машину"]
    fn manual_sleep() {
        sleep().unwrap();
    }

    #[test]
    #[ignore = "ручная приёмка: РЕАЛЬНАЯ перезагрузка"]
    fn manual_restart() {
        restart().unwrap();
    }

    #[test]
    #[ignore = "ручная приёмка: РЕАЛЬНОЕ выключение"]
    fn manual_shutdown() {
        shutdown().unwrap();
    }

    #[test]
    #[ignore = "ручная приёмка: очистка корзины (безопасно только на пустой)"]
    fn manual_empty_recycle_bin() {
        empty_recycle_bin(false).unwrap();
    }
}
