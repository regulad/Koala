//! The QR window: the code 4 inches square on the screen, whatever its size
//! or scale, in the corner of the primary monitor's work area nearest the
//! notification area (so by the tray, wherever the taskbar is), 12 pt in
//! from its edges, on top of other windows. It opens with the code, or with
//! why the attempt failed; nothing shows while the code is made, but the
//! cursor (`busy`). Closing it stops the attempt; it closes itself once the
//! iPhone is connected, and for another attempt ("Try again").
//!
//! WinUI 3 through Reactor, which places and sizes windows but can't yet
//! keep one on top or out of the taskbar: those, and the exact place, are
//! set on its native window (`ComponentContext::run_window`).

use qrcode::{EcLevel, QrCode};
use windows_reactor::*;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GWL_STYLE, GWLP_HWNDPARENT, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST,
    RegisterClassExW, SW_HIDE, SW_SHOW, SWP_FRAMECHANGED, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WNDCLASSEXW, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP,
};

use super::controller::{Qr, Status};
use super::display;
use super::win::wide;

/// The code's side.
const SIDE_INCHES: f64 = 4.0;
/// From the work area's edges.
const MARGIN_PT: f64 = 12.0;
/// The quiet zone the standard asks for, in modules, allowing for codes of
/// version 5 (37 modules) and up: Koala's are about version 8.
const QUIET_MODULES: f64 = 4.0;
const SMALLEST_MODULES: f64 = 37.0;
/// The card's layout, in effective pixels.
const PAD: f64 = 24.0;
const TITLE: f64 = 28.0;
const STATUS: f64 = 20.0;
const GAP: f64 = 16.0;

#[derive(Clone, PartialEq)]
pub struct Input {
    pub qr: Qr,
    pub status: Status,
    /// The window closed, by the user or by Koala.
    pub on_closed: Callback<()>,
    pub on_try_again: Callback<()>,
}

pub struct QrWindow {
    on_closed: Callback<()>,
}

impl Component for QrWindow {
    type Input = Input;
    type Message = ();

    fn create(input: &Input, context: &ComponentContext<Self>) -> Self {
        let _ = context.run_window(|handle| place(handle.as_raw()));
        Self { on_closed: input.on_closed.clone() }
    }

    fn input_changed(&mut self, input: &Input, context: &ComponentContext<Self>) {
        if !input.qr.shown() {
            let _ = context.close_window();
        }
    }

    fn view(&self, input: &Input, context: &mut ViewContext<Self>) -> View {
        let size = Size::now();
        context.window_title("Koala");
        context
            .window_visuals(WindowVisuals::new().client_size(size.width, size.height).backdrop(WindowBackdrop::Mica));
        // Open only with the code or a failure (`Qr::shown`): anything else
        // is the moment before it closes.
        let status = match (&input.status, &input.qr) {
            (Status::Connecting(name), _) => format!("Connecting to {name}…"),
            (_, Qr::Failed(_)) => "Koala couldn't connect.".into(),
            (_, Qr::Code(_)) => "Open the Camera app and point it at this code.".into(),
            _ => String::new(),
        };
        let plate: View = match &input.qr {
            Qr::Code(url) => match svg(url) {
                Some(svg) => Image::new().source_svg(EncodedImage::new(svg)).width(size.code).height(size.code).into(),
                None => message("The code couldn't be made.", None),
            },
            Qr::Failed(why) => message(why, Some(input.on_try_again.clone())),
            _ => message("", None),
        };
        StackPanel::new()
            .margin(Thickness::uniform(PAD))
            .children((
                TextBlock::new().text("Scan with your iPhone").font_size(20.0).font_weight(FontWeight::SEMI_BOLD),
                TextBlock::new().text(status).opacity(0.75).margin(Thickness::new(0.0, 0.0, 0.0, GAP)),
                Border::new()
                    .background(Color::rgb(255, 255, 255))
                    .corner_radius(CornerRadius::uniform(8.0))
                    .padding(Thickness::uniform(size.quiet))
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .content(plate),
            ))
            .into()
    }
}

impl Drop for QrWindow {
    fn drop(&mut self) {
        self.on_closed.call(());
    }
}

/// Text in the code's place, dark on its white plate, and maybe a button.
fn message(text: &str, try_again: Option<Callback<()>>) -> View {
    let size = Size::now();
    let text: View =
        TextBlock::new().text(text).text_wrapping(TextWrapping::Wrap).foreground(Color::rgb(0x1a, 0x1a, 0x1a)).into();
    let content: View = match try_again {
        Some(callback) => StackPanel::new()
            .spacing(12.0)
            .children((text, Button::new().style(ButtonStyle::Accent).content("Try again").on_click(callback)))
            .into(),
        None => text,
    };
    Border::new().width(size.code).height(size.code).padding(Thickness::uniform(24.0)).content(content).into()
}

/// The card's sizes, in effective pixels, for the primary monitor now.
struct Size {
    code: f64,
    quiet: f64,
    width: f64,
    height: f64,
}

impl Size {
    fn now() -> Self {
        let monitor = display::primary();
        let scale = f64::from(monitor.dpi) / 96.0;
        let margin = MARGIN_PT * 96.0 / 72.0;
        let work = monitor.work;
        let room_w = f64::from(work.right - work.left) / scale - 2.0 * margin;
        // Room for the window's own title bar too.
        let room_h = f64::from(work.bottom - work.top) / scale - 2.0 * margin - 48.0;
        let plate_per_code = 1.0 + 2.0 * QUIET_MODULES / SMALLEST_MODULES;
        let fits_w = (room_w - 2.0 * PAD) / plate_per_code;
        let fits_h = (room_h - 2.0 * PAD - TITLE - STATUS - GAP) / plate_per_code;
        // 4 inches in effective pixels: ppi physical pixels an inch, `scale`
        // physical pixels an effective one.
        let code = (SIDE_INCHES * monitor.ppi / scale).min(fits_w).min(fits_h).max(0.0).floor();
        let quiet = (code * QUIET_MODULES / SMALLEST_MODULES).ceil();
        let plate = code + 2.0 * quiet;
        Size { code, quiet, width: plate + 2.0 * PAD, height: PAD + TITLE + STATUS + GAP + plate + PAD }
    }
}

/// The code as an SVG: its modules only (the plate is the quiet zone), one
/// unit each, a path per run of dark modules.
fn svg(url: &str) -> Option<Vec<u8>> {
    let code = QrCode::with_error_correction_level(url, EcLevel::L).ok()?;
    let n = code.width();
    let colors = code.to_colors();
    let mut path = String::new();
    for y in 0..n {
        let mut x = 0;
        while x < n {
            if colors[y * n + x] == qrcode::Color::Dark {
                let start = x;
                while x < n && colors[y * n + x] == qrcode::Color::Dark {
                    x += 1;
                }
                path.push_str(&format!("M{start} {y}h{}v1h-{}z", x - start, x - start));
            } else {
                x += 1;
            }
        }
    }
    Some(
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {n} {n}\" shape-rendering=\"crispEdges\">\
             <path fill=\"#000\" d=\"{path}\"/></svg>"
        )
        .into_bytes(),
    )
}

thread_local! {
    /// A hidden window that owns the QR window, which keeps it out of the
    /// taskbar (a tool window would too, but with a tool window's title bar).
    static OWNER: HWND = unsafe {
        let class = wide("KoalaQrOwner");
        let spec = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: GetModuleHandleW(std::ptr::null()),
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&spec);
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
}

/// Puts the window where it goes, on top and out of the taskbar.
fn place(raw: *mut core::ffi::c_void) {
    let window = raw as HWND;
    let monitor = display::primary();
    let margin = (MARGIN_PT * f64::from(monitor.dpi) / 72.0).round() as i32;
    unsafe {
        // Out of the taskbar: owned. The button goes when it's shown again.
        ShowWindow(window, SW_HIDE);
        SetWindowLongPtrW(window, GWLP_HWNDPARENT, OWNER.with(|owner| *owner) as isize);
        // A card, not a document: no minimising or maximising.
        let style = GetWindowLongPtrW(window, GWL_STYLE);
        SetWindowLongPtrW(window, GWL_STYLE, style & !((WS_MINIMIZEBOX | WS_MAXIMIZEBOX) as isize));
        // Its visible edges, which the margin is from: the window's rectangle
        // includes an invisible resizing border.
        let mut outer = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetWindowRect(window, &mut outer);
        let mut visible = outer;
        DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&raw mut visible).cast(),
            size_of::<RECT>() as u32,
        );
        let (width, height) = (visible.right - visible.left, visible.bottom - visible.top);
        let work = monitor.work;
        // The corner nearest the notification area: bottom right under a
        // bottom taskbar, top right under a top one, and so on.
        let tray = display::tray_point().unwrap_or(POINT { x: work.right, y: work.bottom });
        let right = tray.x > (work.left + work.right) / 2;
        let bottom = tray.y > (work.top + work.bottom) / 2;
        let left = if right { work.right - margin - width } else { work.left + margin };
        let top = if bottom { work.bottom - margin - height } else { work.top + margin };
        let (x, y) = (left - (visible.left - outer.left), top - (visible.top - outer.top));
        SetWindowPos(window, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_FRAMECHANGED);
        ShowWindow(window, SW_SHOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_svg_has_every_dark_module() {
        let url = "continuitycamera://connect?_pubID=X&_pinC=Y&_pi=Z&model=AppleTV14,1&_rdsr=1";
        let svg = String::from_utf8(svg(url).unwrap()).unwrap();
        let code = QrCode::with_error_correction_level(url, EcLevel::L).unwrap();
        let dark = code.to_colors().iter().filter(|c| **c == qrcode::Color::Dark).count();
        // Each run is `M{x} {y}h{len}v1h-{len}z`: the lengths add up to the
        // dark modules.
        let d = svg.split("d=\"").nth(1).unwrap().split('"').next().unwrap();
        let total: usize = d
            .split('z')
            .filter(|run| !run.is_empty())
            .map(|run| run.split('h').nth(1).unwrap().split('v').next().unwrap().parse::<usize>().unwrap())
            .sum();
        assert_eq!(total, dark);
        assert!(svg.starts_with("<svg") && svg.contains(&format!("viewBox=\"0 0 {w} {w}\"", w = code.width())));
    }
}
