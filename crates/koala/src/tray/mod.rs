//! `koala-tray`: Koala in the notification area, in WinUI 3 through
//! windows-rs's Reactor (PLAN.md, "UI").
//!
//! Clicking the icon (either button, or Enter) opens its menu. Reactor opens it
//! for a right-click; a left click, which it only reports, opens it through
//! the same message ([`display::open_icon_menu`]):
//! - "Koala", and a separator;
//! - the status: "Warlock service unavailable"; what Warlock reports
//!   missing, while nothing runs ("No Wi-Fi adapter for Warlock", "No
//!   Bluetooth 5 adapter", "Bluetooth is off"); "Disconnected",
//!   "Connecting to <iPhone>…" or "Connected to <iPhone>";
//! - Connect, greyed out unless disconnected;
//! - a separator, then Options (a submenu, each item on or off: "Start on
//!   logon", [`startup`]; "Stream camera" and "Force software decoding",
//!   [`crate::settings`]), About, which opens About Koala ([`about`]), and
//!   Quit.
//!
//! Reactor's menu items are plain text for now, so the status row is the
//! menu's own size and About has no icon (Parker's choice, 2026-11-20:
//! Reactor, with its limits).
//!
//! While an iPhone is connected, and "Stream camera" is on (its default),
//! Windows has Koala's camera ([`crate::vcam`]; `docs/virtual-camera.md`),
//! named after the iPhone, as a Mac has it (Parker, 2026-11-21): it appears
//! as the iPhone pairs, before its camera stream starts, and goes when the
//! connection ends, as a webcam unplugged does. With it off, or where it
//! can't be made, sessions are the microphones alone.
//!
//! Connect starts a guest run ([`crate::guest`]) and shows its QR code
//! ([`qr`]) until the iPhone is connected. Until the code is ready, the
//! cursor is Windows' "working in background" one ([`busy`]), and no window
//! opens: it opens with the code (Parker, 2026-11-21). Reactor runs the UI
//! on this thread; the run and the polling of Warlock run on Tokio
//! ([`controller`]), whose views come back through a [`bridge`].

mod about;
mod bridge;
mod busy;
mod controller;
pub mod display;
mod glyph;
mod qr;
mod startup;
pub(crate) mod win;

use std::path::PathBuf;

use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use windows_reactor::*;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

pub use controller::{Command, Qr, Status, View};

use crate::settings;
use win::{system_dark, wide};

/// The icon's sizes: Windows picks the one for the taskbar's scale.
const ICON_SIZES: [i32; 9] = [16, 20, 24, 28, 32, 36, 40, 48, 64];

/// Real pixels everywhere, each monitor at its own scale. First thing in
/// the process: it can be set only once, and coordinates read before it
/// are scaled.
pub fn dpi_aware() {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Runs the tray until Windows ends the session. Returns early if another
/// copy is running.
pub fn run() -> Result<(), String> {
    let name = wide(r"Local\Koala.Tray");
    let _single = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if unsafe { GetLastError() } == 183 {
        // ERROR_ALREADY_EXISTS
        return Err("Koala is already running".into());
    }
    App::run_application::<Tray>(()).map_err(|e| e.to_string())
}

/// The virtual camera, called `name` (`docs/virtual-camera.md`). Without it
/// (its source not installed, or a Windows before 11), sessions are the
/// microphones alone.
fn camera(name: &str) -> Option<koala_vcam::VirtualCamera> {
    match koala_vcam::VirtualCamera::start(name) {
        Ok(camera) => {
            crate::diagnostics::step("tray: the virtual camera is up");
            println!("koala-tray: the camera \"{name}\" is up");
            Some(camera)
        }
        Err(e) => {
            crate::diagnostics::step("tray: no virtual camera");
            println!("koala-tray: no virtual camera: {e}");
            None
        }
    }
}

/// Whether sessions ask for the iPhone's camera: while "Stream camera" is
/// on. The camera is made as the iPhone pairs, while the session sets out:
/// the session goes by this, not by whether the camera is there yet.
fn camera_wanted(on: bool) {
    crate::vcam::ON.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Clone)]
enum Message {
    /// The controller's latest view.
    View(View),
    /// A menu choice, by key.
    Menu(Key),
    /// The QR window closed.
    QrClosed,
    TryAgain,
    /// A left click (or Enter) on the icon, where it was.
    Activate(ScreenPoint),
}

struct Tray {
    view: View,
    commands: UnboundedSender<Command>,
    _bridge: bridge::Bridge,
    _runtime: tokio::runtime::Runtime,
    /// Whether Windows will start Koala at logon ([`startup`]).
    start_on_logon: bool,
    /// Whether the camera is decoded by FFmpeg, whatever Windows has
    /// ([`crate::settings`]).
    software_decoding: bool,
    /// Whether the iPhone's camera is streamed to Koala's camera
    /// ([`crate::settings`]); the camera, while an iPhone is connected; and
    /// whether this connection has tried to make it (once each).
    stream_camera: bool,
    camera: Option<koala_vcam::VirtualCamera>,
    camera_tried: bool,
    /// The cursor while the code is being made.
    busy: busy::Busy,
}

impl Application for Tray {
    type Input = ();
    type Message = Message;

    fn create(_: &(), context: &ApplicationContext<Self>) -> Self {
        let runtime = tokio::runtime::Runtime::new().expect("a Tokio runtime");
        let sender = context.sender();
        let bridge = bridge::Bridge::new(move |view| {
            let _ = sender.send(Message::View(view));
        })
        .expect("the tray's message window");
        let poster = bridge.poster();
        let (commands, receiver) = unbounded_channel();
        match std::env::var("KOALA_TRAY_PREVIEW") {
            Ok(what) => preview(&what, poster, context),
            Err(_) => {
                runtime.spawn(controller::run(receiver, move |view| poster.post(view.clone())));
            }
        }
        // Started at logon from somewhere else (Koala moved, or another
        // build): the entry follows this copy.
        let start_on_logon = startup::KOALA.enabled();
        if start_on_logon
            && let Ok(command) = startup::this_program()
            && startup::KOALA.command().as_deref() != Some(command.as_str())
            && let Err(e) = startup::KOALA.set(Some(&command))
        {
            println!("koala-tray: start on logon: {e}");
        }
        let stream_camera = settings::STREAM_CAMERA.on();
        camera_wanted(stream_camera);
        Self {
            view: View::default(),
            commands,
            _bridge: bridge,
            _runtime: runtime,
            start_on_logon,
            software_decoding: settings::FORCE_SOFTWARE_DECODING.on(),
            stream_camera,
            camera: None,
            camera_tried: false,
            busy: busy::Busy::new(),
        }
    }

    fn update(&mut self, message: Message, context: &ApplicationContext<Self>) {
        match message {
            Message::View(view) => {
                // The window opens with something to show: the code, or why
                // the attempt failed. Until then, the cursor says Koala is at it.
                let opening = !self.view.qr.shown() && view.qr.shown();
                self.busy.set(view.qr == Qr::Preparing);
                self.follow_connection(&view.status);
                self.view = view;
                if opening {
                    context.show_window("qr");
                }
            }
            Message::Menu(key) => match key.as_str() {
                Some("connect") => {
                    let _ = self.commands.send(Command::Connect);
                    if self.view.qr.shown() {
                        context.show_window("qr");
                    }
                }
                Some("about") => context.show_window("about"),
                // Closes the windows and the icon; dropping the tray then
                // ends the run, and with it any session.
                Some("quit") => context.exit(),
                Some("logon") => {
                    let command = (!self.start_on_logon).then(startup::this_program).transpose();
                    if let Err(e) = command.and_then(|c| startup::KOALA.set(c.as_deref())) {
                        println!("koala-tray: start on logon: {e}");
                    }
                    self.start_on_logon = startup::KOALA.enabled();
                }
                // Taken up by the next session's camera.
                Some("software") => {
                    if let Err(e) = settings::FORCE_SOFTWARE_DECODING.set(!self.software_decoding) {
                        println!("koala-tray: force software decoding: {e}");
                    }
                    self.software_decoding = settings::FORCE_SOFTWARE_DECODING.on();
                }
                // Off, the camera goes now; on, it comes with the next
                // connection, whose session asks for the iPhone's camera.
                Some("camera") => {
                    if let Err(e) = settings::STREAM_CAMERA.set(!self.stream_camera) {
                        println!("koala-tray: stream camera: {e}");
                    }
                    self.stream_camera = settings::STREAM_CAMERA.on();
                    camera_wanted(self.stream_camera);
                    if !self.stream_camera {
                        self.camera = None;
                    }
                }
                _ => {}
            },
            // Closed while showing the code or a failure: the user gave up.
            // (It closes itself for another attempt, "Try again": that's no
            // giving up.)
            Message::QrClosed => {
                if self.view.qr.shown() {
                    let _ = self.commands.send(Command::Cancel);
                }
            }
            // The same menu as a right-click.
            Message::Activate(at) => {
                display::open_icon_menu(Some(windows_sys::Win32::Foundation::POINT { x: at.x, y: at.y }));
            }
            Message::TryAgain => {
                let _ = self.commands.send(Command::Connect);
            }
        }
    }

    fn view(&self, _: &(), context: &ApplicationViewContext<Self>) -> ApplicationView {
        let status = &self.view.status;
        let connect = if status.can_connect() {
            MenuItem::item("connect", "Connect")
        } else {
            MenuItem::disabled("connect", "Connect")
        };
        let menu = Menu::new(
            [
                MenuItem::disabled("koala", "Koala"),
                MenuItem::separator("separator-name"),
                MenuItem::disabled("status", status.heading()),
                connect,
                MenuItem::separator("separator-app"),
                // Reactor's menus have no checkable items yet: the state is
                // a check mark in the label.
                MenuItem::submenu(
                    "options",
                    "Options",
                    [
                        MenuItem::item("logon", checked(self.start_on_logon, "Start on logon")),
                        MenuItem::item("camera", checked(self.stream_camera, "Stream camera")),
                        MenuItem::item("software", checked(self.software_decoding, "Force software decoding")),
                    ],
                ),
                MenuItem::item("about", "About"),
                MenuItem::item("quit", "Quit"),
            ],
            context.callback(Message::Menu),
        );
        let qr = qr::Input {
            qr: self.view.qr.clone(),
            status: status.clone(),
            on_closed: context.message(Message::QrClosed),
            on_try_again: context.message(Message::TryAgain),
        };
        ApplicationView::new()
            .window::<qr::QrWindow>("qr", qr)
            .window::<about::AboutWindow>("about", about::Input { warlock: self.view.warlock.clone() })
            .notify_icon(
                "tray",
                NotifyIcon::new(icon(matches!(status, Status::Connected(_))))
                    .tooltip(format!("Koala: {}", status.heading()))
                    .on_activate(context.callback(Message::Activate))
                    .menu(menu),
            )
    }
}

impl Tray {
    /// The camera with the connection: made, named after the iPhone, as it
    /// pairs (once a connection: a camera that can't be made isn't tried at
    /// every view), and gone when the connection is.
    fn follow_connection(&mut self, status: &Status) {
        match status {
            Status::Connecting(name) | Status::Connected(name) => {
                if self.stream_camera && !self.camera_tried {
                    self.camera_tried = true;
                    self.camera = camera(name);
                }
            }
            _ => {
                self.camera_tried = false;
                if self.camera.take().is_some() {
                    println!("koala-tray: the camera is gone, with the connection");
                }
            }
        }
    }
}

/// An on-or-off item's label: with a check mark when on.
fn checked(on: bool, label: &str) -> String {
    if on { format!("✓  {label}") } else { label.to_owned() }
}

/// The tray icon's file: the microphone, white on a dark taskbar and black
/// on a light one, dimmed unless connected. Made the first time it's
/// needed, under `%LOCALAPPDATA%\Koala`.
fn icon(connected: bool) -> PathBuf {
    let dir = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from).join("Koala");
    let dark = system_dark();
    let path = dir.join(format!(
        "tray-1-{}-{}.ico",
        if dark { "white" } else { "black" },
        if connected { "on" } else { "off" }
    ));
    if !path.exists() {
        let color = if dark { (255, 255, 255) } else { (0, 0, 0) };
        let opacity = if connected { 255 } else { 128 };
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&path, glyph::ico(glyph::MICROPHONE, &ICON_SIZES, color, opacity));
    }
    path
}

/// `KOALA_TRAY_PREVIEW=menu|qr|about`: the UI alone, with scripted views in
/// place of the controller's, for looking at (screenshots, layout checks).
/// - `menu`: disconnected, and the menu opens as a right-click on the icon
///   would.
/// - `qr`: a sample code; 8 s later the view hides it, as a connection does,
///   and the window should close itself.
/// - `about`: About Koala.
fn preview(what: &str, poster: bridge::Poster, context: &ApplicationContext<Tray>) {
    let disconnected = View { status: Status::Disconnected, qr: Qr::Hidden, warlock: Some("0.2.2".into()) };
    match what {
        "menu" => {
            poster.post(disconnected);
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(2500));
                display::open_icon_menu(None);
            });
        }
        "qr" => {
            let url = format!(
                "continuitycamera://connect?_pubID={}&_pinC={}&_pi={}&model=AppleTV14,1&_rdsr=1",
                crate::uuid(),
                crate::uuid(),
                crate::uuid()
            );
            poster.post(View { qr: Qr::Code(url), ..disconnected.clone() });
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(8));
                poster.post(disconnected);
            });
        }
        "about" => {
            poster.post(disconnected);
            context.show_window("about");
        }
        _ => {}
    }
}
