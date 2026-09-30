//! Извлечение иконок в PNG-кэш (шаг 3 Фазы 2, D7, риск 3).
//!
//! Путь: `IShellItemImageFactory::GetImage` (план Б риска 3 — надёжнее
//! SHGetFileInfoW: работает и для exe, и для .lnk, и для документов) →
//! HBITMAP 32bpp BGRA premultiplied → un-premultiply → RGBA → PNG
//! (image 0.25) → файл кэша `{hash}.png`, hash — FNV-1a 64 от имени источника.
//!
//! Каталог кэша — параметр (прод: `%APPDATA%\iskra\icons`; тесты: %TMP%).
//! Требует COM-инициализации вызывающего потока — модуль делает сам
//! (CoInitializeEx STA, с учётом RPC_E_CHANGED_MODE и CoUninitialize).

use std::io;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, SIZE};
use windows::Win32::Graphics::Gdi::{
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits,
    GetObjectW, HBITMAP, HGDIOBJ, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, IBindCtx,
};
use windows::Win32::UI::Shell::{
    SHCreateItemFromParsingName, IShellItemImageFactory, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};

/// Извлечь иконку `source` (exe/файл/.lnk/папка) размером `size`×`size` и
/// вернуть PNG-байты.
pub fn extract_png_bytes(source: &str, size: i32) -> io::Result<Vec<u8>> {
    if size <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "размер иконки должен быть > 0",
        ));
    }
    let source_w = wide(source);

    // COM на вызывающем потоке. S_OK/S_FALSE → балансируем CoUninitialize;
    // RPC_E_CHANGED_MODE (поток уже MTA) → работаем без uninit (Shell API это позволяют).
    let mut needs_uninit = false;
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        if hr.is_ok() {
            needs_uninit = true;
        } else if hr != RPC_E_CHANGED_MODE {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("CoInitializeEx failed: {hr}"),
            ));
        }
    }

    let result = (|| {
        // Квирка шелла: пока кэш иконок строится, GetImage может вернуть
        // E_PENDING (0x8000000A) даже для SIIGBF_ICONONLY — лечится повтором.
        const E_PENDING: windows::core::HRESULT = windows::core::HRESULT(0x8000_000Au32 as _);
        let mut hbm = None;
        for attempt in 0..5 {
            let factory: IShellItemImageFactory =
                match unsafe { SHCreateItemFromParsingName(PCWSTR(source_w.as_ptr()), None::<&IBindCtx>) }
                {
                    Err(e) => return Err(err("SHCreateItemFromParsingName", e)),
                    Ok(f) => f,
                };
            match unsafe {
                factory.GetImage(
                    SIZE { cx: size, cy: size },
                    SIIGBF_BIGGERSIZEOK | SIIGBF_ICONONLY,
                )
            } {
                Ok(bm) => {
                    hbm = Some(bm);
                    break;
                }
                Err(e) if e.code() == E_PENDING && attempt < 4 => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(e) => return Err(err("IShellItemImageFactory::GetImage", e)),
            }
        }
        let hbm = hbm.ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "GetImage: E_PENDING после 5 попыток")
        })?;
        hbitmap_to_png(hbm)
    })();

    if needs_uninit {
        unsafe { CoUninitialize() };
    }
    result
}

/// Извлечь иконку и записать PNG в кэш `cache_dir`: `{dir}/{hash}.png`.
/// Каталог создаётся при необходимости; возвращает полный путь к файлу.
pub fn extract_to_cache(source: &str, size: i32, cache_dir: &str) -> io::Result<String> {
    std::fs::create_dir_all(cache_dir)?;
    let path = std::path::Path::new(cache_dir).join(cache_file_name(source));
    let png = extract_png_bytes(source, size)?;
    std::fs::write(&path, &png)?;
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "путь кэша не UTF-8"))
}

/// Имя файла кэша для источника: `{hash}.png` (D7).
pub fn cache_file_name(source: &str) -> String {
    format!("{}.png", source_hash(source))
}

/// FNV-1a 64-bit от имени источника, hex (простого хэша достаточно — план D7).
pub fn source_hash(source: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in source.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// HBITMAP (32bpp BGRA premultiplied) → PNG-байты. Владеет `hbm`: удаляет его.
fn hbitmap_to_png(hbm: HBITMAP) -> io::Result<Vec<u8>> {
    let cleanup = |hdc: windows::Win32::Graphics::Gdi::HDC| unsafe {
        let _ = DeleteDC(hdc);
        let _ = DeleteObject(HGDIOBJ::from(hbm));
    };
    unsafe {
        let hdc = CreateCompatibleDC(None);
        if hdc.is_invalid() {
            cleanup(hdc);
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "CreateCompatibleDC failed",
            ));
        }

        // Реальные размеры — через GetObjectW (BIGGERSIZEOK позволяет шеллу
        // вернуть больше запрошенного).
        let mut bmp = BITMAP::default();
        if GetObjectW(
            HGDIOBJ::from(hbm),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bmp as *mut BITMAP as *mut core::ffi::c_void),
        ) == 0
        {
            cleanup(hdc);
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "GetObjectW (размеры HBITMAP) failed",
            ));
        }
        let (w, h) = (bmp.bmWidth, bmp.bmHeight);
        if w <= 0 || h <= 0 {
            cleanup(hdc);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("HBITMAP пустой ({w}x{h})"),
            ));
        }

        // 32bpp запрашиваем всегда: GDI сам конвертирует любой формат битов;
        // biHeight < 0 → top-down (строки сверху вниз, как ждёт image).
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let mut buf = vec![0u8; w as usize * h as usize * 4];
        if GetDIBits(
            hdc,
            hbm,
            0,
            h as u32,
            Some(buf.as_mut_ptr().cast()),
            &mut bmi,
            DIB_RGB_COLORS,
        ) == 0
        {
            cleanup(hdc);
            return Err(io::Error::new(io::ErrorKind::Other, "GetDIBits (пиксели) failed"));
        }
        cleanup(hdc);

        bgra_premultiplied_to_rgba_png(buf, w as u32, h as u32)
    }
}

/// BGRA premultiplied → RGBA straight → PNG-байты.
///
/// Квирка (риск 3): иконки без альфа-канала приходят с a=0 во всех пикселях —
/// считаем их непрозрачными, иначе получили бы чёрный квадрат.
fn bgra_premultiplied_to_rgba_png(mut bgra: Vec<u8>, w: u32, h: u32) -> io::Result<Vec<u8>> {
    let has_alpha = bgra.chunks_exact(4).any(|px| px[3] != 0);
    for px in bgra.chunks_exact_mut(4) {
        let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
        let rgba = if !has_alpha {
            [r, g, b, 255]
        } else if a == 0 {
            [0, 0, 0, 0]
        } else {
            // un-premultiply: c' = round(c * 255 / a)
            let un = |c: u8| ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8;
            [un(r), un(g), un(b), a]
        };
        px.copy_from_slice(&rgba);
    }

    let img = image::RgbaImage::from_raw(w, h, bgra).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "буфер не совпал с размерами")
    })?;
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("PNG encode failed: {e}")))?;
    Ok(png.into_inner())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn err(op: &str, e: windows::core::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{op} failed: {e}"))
}

/// Сериализует иконко-тесты (общий кэш иконок шелла не любит параллельный доступ
/// из одного процесса — источник флейков E_PENDING/GetImage).
#[cfg(test)]
pub(crate) static ICON_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// Done-критерий шага 3: иконка notepad.exe извлекается в tmp как валидный PNG.
    #[test]
    fn extract_notepad_icon_to_tmp_png() {
        let _guard = ICON_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("iskra-icons-test-{}", std::process::id()));
        let dir_s = dir.to_str().expect("tmp-путь должен быть UTF-8");

        let path =
            extract_to_cache(r"C:\Windows\System32\notepad.exe", 32, dir_s).expect("извлечение иконки");

        let bytes = std::fs::read(&path).expect("PNG-файл кэша должен читаться");
        assert!(bytes.len() > 100, "PNG не должен быть пустым, got {} байт", bytes.len());
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "файл должен начинаться с PNG-сигнатуры"
        );

        // Roundtrip: декодируется обратно, размер в разумных пределах (BIGGERSIZEOK).
        let img = image::load_from_memory(&bytes).expect("PNG должен декодироваться");
        assert!(
            img.width() >= 16 && img.width() <= 256 && img.height() >= 16 && img.height() <= 256,
            "неожиданный размер иконки: {}×{}",
            img.width(),
            img.height()
        );

        std::fs::remove_dir_all(&dir).expect("tmp-каталог должен удаляться");
    }

    /// Hash имени источника: детерминированный, различающий, 16 hex-символов.
    #[test]
    fn source_hash_is_stable_and_distinct() {
        let a = source_hash("notepad.exe");
        assert_eq!(a, source_hash("notepad.exe"), "hash должен быть детерминированным");
        assert_ne!(a, source_hash("cmd.exe"), "разные источники — разные хэши");
        assert_eq!(a.len(), 16, "FNV-1a 64 → 16 hex-символов");
        assert_eq!(cache_file_name("x"), format!("{}.png", source_hash("x")));
    }
}
