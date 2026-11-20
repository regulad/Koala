//! Glyphs from Windows' icon font (Segoe Fluent Icons, or Segoe MDL2 Assets
//! on Windows 10), drawn as the tray icon.

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DrawTextW, GdiFlush, HDC, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};

use super::win::{Gdi, font_with, rgb, utf16};

pub const MICROPHONE: char = '\u{E720}';

pub const ICON_FACES: [&str; 2] = ["Segoe Fluent Icons", "Segoe MDL2 Assets"];

/// How much of each pixel `glyph` covers, 0–255, drawn `size` pixels
/// square with its em filling the square.
pub fn coverage(glyph: char, size: i32) -> Vec<u8> {
    let (dib, bits) = dib(size);
    unsafe {
        let dc: HDC = CreateCompatibleDC(std::ptr::null_mut());
        let old_bitmap = SelectObject(dc, dib.0);
        // Grayscale antialiasing: ClearType's colour fringes would show.
        let font = font_with(&ICON_FACES, f64::from(size), 400, ANTIALIASED_QUALITY);
        let old_font = SelectObject(dc, font.0);
        SetTextColor(dc, rgb(255, 255, 255));
        SetBkMode(dc, TRANSPARENT as _);
        let mut text = utf16(&glyph.to_string());
        let mut rect = RECT { left: 0, top: 0, right: size, bottom: size };
        DrawTextW(
            dc,
            text.as_mut_ptr(),
            text.len() as i32,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        GdiFlush();
        SelectObject(dc, old_font);
        SelectObject(dc, old_bitmap);
        DeleteDC(dc);
        let pixels = std::slice::from_raw_parts(bits, (size * size) as usize);
        // White on black: any channel is the coverage.
        pixels.iter().map(|p| (p >> 16) as u8).collect()
    }
}

/// A top-down 32-bit DIB section, `size` square, and its pixels.
fn dib(size: i32) -> (Gdi, *mut u32) {
    let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: size,
        biHeight: -size,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..unsafe { std::mem::zeroed() }
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bitmap =
        unsafe { CreateDIBSection(std::ptr::null_mut(), &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0) };
    (Gdi(bitmap), bits.cast())
}

/// An `.ico` file of `glyph` in `color` at `opacity` (0–255), at each of
/// `sizes`: Windows picks the one for the scale.
pub fn ico(glyph: char, sizes: &[i32], color: (u8, u8, u8), opacity: u8) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| ico_image(glyph, size, color, opacity)).collect();
    let mut out = Vec::new();
    // ICONDIR, then an ICONDIRENTRY per image.
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&size, image) in sizes.iter().zip(&images) {
        let side = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

/// One image of an `.ico`: a BITMAPINFOHEADER (twice the height, for the
/// mask), straight-alpha BGRA rows bottom to top, then an empty AND mask.
fn ico_image(glyph: char, size: i32, color: (u8, u8, u8), opacity: u8) -> Vec<u8> {
    let cover = coverage(glyph, size);
    let side = size as usize;
    let mask_row = side.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + side * side * 4 + mask_row * side);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(2 * size).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..side).rev() {
        for x in 0..side {
            let a = u32::from(cover[y * side + x]) * u32::from(opacity) / 255;
            out.extend_from_slice(&[color.2, color.1, color.0, a as u8]);
        }
    }
    out.resize(out.len() + mask_row * side, 0);
    out
}
