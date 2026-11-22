//! Koala's settings, each the user's own, under
//! `HKEY_CURRENT_USER\Software\Koala`:
//! - `StreamMicrophones` (a DWORD, 0 for off): give Windows the iPhone's
//!   microphones while it's connected (`crate::usb_mics`). On unless set
//!   off. The tray's Options menu sets it.
//! - `StreamCamera` (a DWORD, 0 for off): ask the iPhone for its camera in
//!   each session, and give Windows Koala's camera while it's connected
//!   (`crate::vcam`). On unless set off. The tray's Options menu sets it.
//! - `ForceSoftwareDecoding` (a DWORD, 1 for on): decode the camera with
//!   FFmpeg's decoder even where Windows has one ([`crate::decode`]). Off
//!   unless set. The tray's Options menu sets it; `koala guest
//!   --software-decode` turns it on for one run.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW,
};

use crate::tray::win::wide;

const KEY: &str = r"Software\Koala";

/// An on-or-off setting: a DWORD under a key of the user's.
pub struct Switch {
    key: &'static str,
    name: &'static str,
    /// What it is where it's never been set.
    default: bool,
}

/// Whether to give Windows the iPhone's microphones.
pub const STREAM_MICROPHONES: Switch = Switch { key: KEY, name: "StreamMicrophones", default: true };

/// Whether to stream the iPhone's camera to Koala's camera.
pub const STREAM_CAMERA: Switch = Switch { key: KEY, name: "StreamCamera", default: true };

/// Whether to decode the camera with FFmpeg's decoder, whatever Windows has.
pub const FORCE_SOFTWARE_DECODING: Switch = Switch { key: KEY, name: "ForceSoftwareDecoding", default: false };

/// `koala guest --software-decode`: software decoding for this run, whatever
/// the setting.
pub static SOFTWARE_DECODING_THIS_RUN: AtomicBool = AtomicBool::new(false);

/// Whether the camera is to be decoded in software: the setting, or this run.
pub fn software_decoding() -> bool {
    SOFTWARE_DECODING_THIS_RUN.load(Ordering::Relaxed) || FORCE_SOFTWARE_DECODING.on()
}

impl Switch {
    /// Whether it's on: its default where it's never been set.
    pub fn on(&self) -> bool {
        let (key, name) = (wide(self.key), wide(self.name));
        let (mut data, mut size) = (0u32, 4u32);
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
        if status == 0 { data != 0 } else { self.default }
    }

    /// Turns it on or off, making its key if need be.
    pub fn set(&self, on: bool) -> Result<(), String> {
        let (key, name) = (wide(self.key), wide(self.name));
        let data = u32::from(on);
        let status = unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), REG_DWORD, (&raw const data).cast(), 4)
        };
        if status == 0 { Ok(()) } else { Err(format!("{}\\{}: error {status}", self.key, self.name)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::RegDeleteTreeW;

    #[test]
    fn a_switch_is_its_default_until_set() {
        const ROOT: &str = r"Software\Koala-test-settings";
        let root = wide(ROOT);
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, root.as_ptr()) };
        let off = Switch { key: ROOT, name: "Off", default: false };
        let on = Switch { key: ROOT, name: "On", default: true };
        assert!(!off.on());
        assert!(on.on());
        off.set(true).unwrap();
        on.set(false).unwrap();
        assert!(off.on());
        assert!(!on.on());
        off.set(false).unwrap();
        assert!(!off.on());
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, root.as_ptr()) };
    }
}
