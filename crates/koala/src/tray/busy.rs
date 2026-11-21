//! The cursor while Koala makes its QR code: Windows' "working in
//! background" arrow (`IDC_APPSTARTING`, an arrow with a spinner, once with
//! an hourglass), in place of a window saying "Getting ready" (Parker,
//! 2026-11-21). It's what Windows shows while a program starts; the full
//! busy cursor would make the whole desktop look frozen.
//!
//! Once the tray's menu has closed, the mouse is over no window of Koala's,
//! so the cursor is set system-wide, as the normal arrow's replacement. The
//! user's cursors come back from their own settings (`SPI_SETCURSORS`) when
//! the code is ready or the attempt ends, when Koala quits, and when it
//! starts, in case a Koala was stopped before it could put them back.

use windows_sys::Win32::UI::WindowsAndMessaging::{
    CopyIcon, IDC_APPSTARTING, LoadCursorW, OCR_NORMAL, SPI_SETCURSORS, SetSystemCursor, SystemParametersInfoW,
};

/// Whether the busy cursor is up; put back when dropped.
pub struct Busy {
    on: bool,
}

impl Busy {
    /// The user's own cursors, whatever an earlier Koala left.
    pub fn new() -> Self {
        restore();
        Self { on: false }
    }

    /// Shows the busy cursor, or puts the user's back.
    pub fn set(&mut self, on: bool) {
        if on == self.on {
            return;
        }
        self.on = on;
        if on { busy() } else { restore() }
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        if self.on {
            restore();
        }
    }
}

fn busy() {
    // SAFETY: a shared system cursor, copied: SetSystemCursor takes the
    // cursor it's given and destroys it.
    unsafe {
        let copy = CopyIcon(LoadCursorW(std::ptr::null_mut(), IDC_APPSTARTING));
        if !copy.is_null() {
            SetSystemCursor(copy, OCR_NORMAL);
        }
    }
}

/// The user's cursors, as their settings have them.
fn restore() {
    // SAFETY: no pointers.
    unsafe { SystemParametersInfoW(SPI_SETCURSORS, 0, std::ptr::null_mut(), 0) };
}
