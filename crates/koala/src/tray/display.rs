//! The primary monitor: its work area, its scale, and how many of its pixels
//! make an inch, for a QR code of a fixed physical size.
//!
//! Windows' DPI is a scale setting, not a measurement: 96 × the scale, on
//! any panel. The real pixel density comes from the size the monitor
//! reports (its EDID), through WinRT's `DisplayMonitor`.

use windows::Devices::Display::DisplayMonitor;
use windows::core::HSTRING;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, EnumDisplayDevicesW, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW,
    MonitorFromPoint,
};
use windows_sys::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Shell::{
    ABE_LEFT, ABE_TOP, ABM_GETTASKBARPOS, APPBARDATA, NOTIFYICONIDENTIFIER, SHAppBarMessage, Shell_NotifyIconGetRect,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EDD_GET_DEVICE_INTERFACE_NAME, FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE, PostMessageW, WM_CONTEXTMENU,
    WM_USER,
};

/// What a monitor's reported size must come to, to be believed: projectors
/// and some TVs report nothing, or nonsense.
const PLAUSIBLE_PPI: std::ops::RangeInclusive<f64> = 50.0..=1000.0;

#[derive(Clone, Copy)]
pub struct Primary {
    /// Without the taskbar, in physical pixels.
    pub work: RECT,
    /// The scale, as a DPI: 96 at 100 %.
    pub dpi: u32,
    /// Physical pixels per inch.
    pub ppi: f64,
    /// Whether `ppi` came from the monitor's size, not the scale.
    pub measured: bool,
}

/// The primary monitor (the one at 0,0), now.
pub fn primary() -> Primary {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>());
        let (mut dpi, mut _dpi_y) = (96u32, 96u32);
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut _dpi_y);
        let full = info.monitorInfo.rcMonitor;
        let (width, height) = (f64::from(full.right - full.left), f64::from(full.bottom - full.top));
        let measured = inches(&info.szDevice)
            .map(|(w, h)| width.hypot(height) / w.hypot(h))
            .filter(|ppi| PLAUSIBLE_PPI.contains(ppi));
        Primary {
            work: info.monitorInfo.rcWork,
            dpi,
            ppi: measured.unwrap_or(f64::from(dpi)),
            measured: measured.is_some(),
        }
    }
}

/// The physical size, in inches, of the monitor on display device `device`
/// (`\\.\DISPLAY1`, …), as it reports it.
fn inches(device: &[u16; 32]) -> Option<(f64, f64)> {
    let mut display: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
    display.cb = size_of::<DISPLAY_DEVICEW>() as u32;
    // The monitor attached to that output, by its device interface path.
    if unsafe { EnumDisplayDevicesW(device.as_ptr(), 0, &mut display, EDD_GET_DEVICE_INTERFACE_NAME) } == 0 {
        return None;
    }
    let len = display.DeviceID.iter().position(|&c| c == 0).unwrap_or(display.DeviceID.len());
    let id = String::from_utf16_lossy(&display.DeviceID[..len]);
    // WinRT, on a thread of its own in the multithreaded apartment: the
    // caller is a UI thread.
    std::thread::spawn(move || unsafe {
        let initialized = CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) >= 0;
        let size = (|| {
            let monitor = DisplayMonitor::FromInterfaceIdAsync(&HSTRING::from(id)).ok()?.join().ok()?;
            let size = monitor.PhysicalSizeInInches().ok()?.Value().ok()?;
            Some((f64::from(size.Width), f64::from(size.Height)))
        })();
        if initialized {
            CoUninitialize();
        }
        size
    })
    .join()
    .ok()
    .flatten()
    .filter(|&(w, h)| w > 0.0 && h > 0.0)
}

/// Where the notification area is: the middle of Koala's own icon (or of
/// the overflow button, when Windows keeps the icon in the overflow), else
/// the end of the taskbar where the notification area sits.
pub fn tray_point() -> Option<POINT> {
    if let Some(window) = icon_window() {
        let id = NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: window,
            uID: ICON_ID,
            guidItem: unsafe { std::mem::zeroed() },
        };
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if unsafe { Shell_NotifyIconGetRect(&id, &mut rect) } >= 0 && rect.right > rect.left {
            return Some(POINT { x: (rect.left + rect.right) / 2, y: (rect.top + rect.bottom) / 2 });
        }
    }
    let mut bar: APPBARDATA = unsafe { std::mem::zeroed() };
    bar.cbSize = size_of::<APPBARDATA>() as u32;
    if unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut bar) } == 0 {
        return None;
    }
    let rc = bar.rc;
    // The notification area is at the right end of a horizontal taskbar (in
    // a left-to-right layout) and the bottom of a vertical one.
    Some(match bar.uEdge {
        ABE_TOP => POINT { x: rc.right, y: rc.top },
        ABE_LEFT => POINT { x: rc.left, y: rc.bottom },
        _ => POINT { x: rc.right, y: rc.bottom },
    })
}

/// Reactor's notification-icon window in this process (it's named so),
/// whose icon is number 1.
fn icon_window() -> Option<HWND> {
    const TITLE: &str = "windows-notifyicon";
    let title: Vec<u16> = TITLE.encode_utf16().chain(Some(0)).collect();
    let ours = |window: HWND| {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        pid == std::process::id()
    };
    // Top-level windows, then message-only ones.
    for parent in [std::ptr::null_mut(), HWND_MESSAGE] {
        let mut after: HWND = std::ptr::null_mut();
        loop {
            after = unsafe { FindWindowExW(parent, after, std::ptr::null(), title.as_ptr()) };
            if after.is_null() {
                break;
            }
            if ours(after) {
                return Some(after);
            }
        }
    }
    None
}

const ICON_ID: u32 = 1;

/// Opens the icon's menu as a right-click on it would: Reactor's callback
/// message (`WM_USER + 1`) as Explorer sends it (`NOTIFYICON_VERSION_4`).
/// For the tray's preview.
pub fn open_icon_menu() {
    let Some(window) = icon_window() else { return };
    let at = tray_point().unwrap_or(POINT { x: 0, y: 0 });
    let position = (at.x as u16 as usize) | ((at.y as u16 as usize) << 16);
    unsafe { PostMessageW(window, WM_USER + 1, position, (WM_CONTEXTMENU | ICON_ID << 16) as isize) };
}

/// Prints what Koala knows of the primary monitor: `koala-tray --monitor`.
pub fn describe() -> String {
    let p = primary();
    let source = if p.measured { "from its reported size" } else { "assumed from the scale" };
    format!(
        "primary monitor: work area {},{} to {},{}; scale {} % ({} dpi); {:.1} pixels per inch, {source}",
        p.work.left,
        p.work.top,
        p.work.right,
        p.work.bottom,
        p.dpi * 100 / 96,
        p.dpi,
        p.ppi
    )
}
