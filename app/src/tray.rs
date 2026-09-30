//! Трей: иконка + меню «Показать / Настройки / Автозапуск ✓ / Выход».
//! Галочка автозапуска синхронизируется пересборкой меню (план, риск 3:
//! простой и надёжный вариант вместо слежения за CheckMenuItem).

use std::sync::OnceLock;

use tauri::menu::{CheckMenuItem, Menu, MenuBuilder, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Wry};

use iskra_core::logging;
use iskra_core::EVENT_NAV_SETTINGS;

use crate::{commands, window};

static TRAY: OnceLock<TrayIcon<Wry>> = OnceLock::new();

const TRAY_ID: &str = "iskra-tray";
const ID_SHOW: &str = "show";
const ID_SETTINGS: &str = "settings";
const ID_AUTOSTART: &str = "autostart";
const ID_QUIT: &str = "quit";

/// Создать иконку и меню трея. Ошибка не роняет приложение — логгируется в setup.
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let checked = iskra_sys::autostart::is_enabled();
    let menu = build_menu(app, checked)?;
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::include_image!("icons/32x32.png"))
        .tooltip("Iskra")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            ID_SHOW => window::show(app, window::ShowReason::User),
            ID_SETTINGS => {
                window::show(app, window::ShowReason::User);
                if let Err(err) = app.emit(EVENT_NAV_SETTINGS, ()) {
                    logging::warn(&format!("tray: emit nav://settings FAILED: {err}"));
                }
            }
            ID_AUTOSTART => {
                commands::toggle_autostart(app);
            }
            ID_QUIT => {
                logging::info("quit from tray");
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    let _ = TRAY.set(tray);
    logging::info("tray: created");
    Ok(())
}

/// Синхронизировать галочку автозапуска с фактическим состоянием
/// (пересборка меню — дёшево, план риск 3).
pub fn sync_autostart(app: &AppHandle, checked: bool) {
    let Some(tray) = TRAY.get() else { return };
    match build_menu(app, checked) {
        Ok(menu) => {
            if let Err(err) = tray.set_menu(Some(menu)) {
                logging::warn(&format!("tray: set_menu FAILED: {err}"));
            }
        }
        Err(err) => logging::warn(&format!("tray: rebuild menu FAILED: {err}")),
    }
}

fn build_menu(app: &AppHandle, autostart_checked: bool) -> tauri::Result<Menu<Wry>> {
    let show = MenuItem::with_id(app, ID_SHOW, "Показать", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, ID_SETTINGS, "Настройки", true, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(
        app,
        ID_AUTOSTART,
        "Автозапуск",
        true,
        autostart_checked,
        None::<&str>,
    )?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "Выход", true, None::<&str>)?;
    MenuBuilder::new(app)
        .items(&[&show, &settings_item, &autostart, &sep, &quit])
        .build()
}
