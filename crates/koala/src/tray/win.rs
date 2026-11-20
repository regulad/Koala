//! Small Win32 helpers for the tray: strings, theme, the icon font.

use windows_sys::Win32::Foundation::COLORREF;
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetDC, GetTextFaceW, HDC, HGDIOBJ, LOGFONTW, ReleaseDC, SelectObject,
};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

/// A NUL-terminated UTF-16 string.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// UTF-16 without the NUL, for counted calls.
pub fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

fn personalize(value: &str) -> Option<u32> {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide(value);
    let mut data = 0u32;
    let mut size = 4u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast(),
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

/// Whether Windows itself, the taskbar included, is dark.
pub fn system_dark() -> bool {
    personalize("SystemUsesLightTheme") == Some(0)
}

/// A GDI object, deleted when dropped.
pub struct Gdi(pub HGDIOBJ);

impl Drop for Gdi {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DeleteObject(self.0) };
        }
    }
}

/// The first of `faces` that Windows has, `px` pixels high (the em, not the
/// cell), at `weight` and rendering `quality` (`*_QUALITY`).
pub fn font_with(faces: &[&str], px: f64, weight: i32, quality: u8) -> Gdi {
    for (i, face) in faces.iter().enumerate() {
        let mut spec: LOGFONTW = unsafe { std::mem::zeroed() };
        spec.lfHeight = -(px.round() as i32);
        spec.lfWeight = weight;
        spec.lfQuality = quality;
        spec.lfCharSet = 1; // DEFAULT_CHARSET
        for (dst, src) in spec.lfFaceName.iter_mut().zip(face.encode_utf16().take(31)) {
            *dst = src;
        }
        let made = Gdi(unsafe { CreateFontIndirectW(&spec) });
        if i + 1 == faces.len() || has_face(&made, face) {
            return made;
        }
    }
    unreachable!("faces isn't empty")
}

/// Whether a font really is `face`, not Windows' substitute for it.
fn has_face(font: &Gdi, face: &str) -> bool {
    unsafe {
        let dc: HDC = GetDC(std::ptr::null_mut());
        let old = SelectObject(dc, font.0);
        let mut name = [0u16; 64];
        let len = GetTextFaceW(dc, name.len() as i32, name.as_mut_ptr());
        SelectObject(dc, old);
        ReleaseDC(std::ptr::null_mut(), dc);
        let got = String::from_utf16_lossy(&name[..(len.max(1) - 1) as usize]);
        got.eq_ignore_ascii_case(face)
    }
}
