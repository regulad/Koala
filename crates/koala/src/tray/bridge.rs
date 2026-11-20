//! The controller's views, from Tokio's threads to the UI thread.
//!
//! Reactor's application messages can only be sent on the UI thread (its
//! `ApplicationContext` gives a `LocalSender`). So a hidden message-only
//! window lives there, and a [`Poster`] on any thread leaves the latest view
//! and posts to it; the window hands the view to the app. Reactor's WinUI
//! message loop dispatches to it like any window on its thread.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, PostMessageW, RegisterClassExW, WM_APP, WNDCLASSEXW,
};

use super::View;
use super::win::wide;

const CLASS: &str = "KoalaBridge";
const WM_VIEW: u32 = WM_APP + 1;

type Latest = Arc<Mutex<Option<View>>>;
/// The app's side: the latest view, and where it goes.
type Delivery = (Latest, Box<dyn Fn(View)>);

thread_local! {
    static DELIVERY: RefCell<Option<Delivery>> = const { RefCell::new(None) };
}

/// The window, on the UI thread; destroyed when dropped.
pub struct Bridge {
    window: HWND,
    latest: Latest,
}

/// Posts views to a [`Bridge`] from any thread.
#[derive(Clone)]
pub struct Poster {
    window: isize,
    latest: Latest,
}

impl Bridge {
    /// Made on the UI thread: `deliver` gets each view there.
    pub fn new(deliver: impl Fn(View) + 'static) -> Result<Self, String> {
        let class = wide(CLASS);
        let window = unsafe {
            let spec = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(procedure),
                hInstance: GetModuleHandleW(std::ptr::null()),
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&spec);
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if window.is_null() {
            return Err("the tray's message window couldn't be made".into());
        }
        let latest = Latest::default();
        DELIVERY.set(Some((latest.clone(), Box::new(deliver))));
        Ok(Self { window, latest })
    }

    pub fn poster(&self) -> Poster {
        Poster { window: self.window as isize, latest: self.latest.clone() }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        DELIVERY.set(None);
        unsafe { DestroyWindow(self.window) };
    }
}

impl Poster {
    /// Leaves `view` for the UI thread. Views left before it takes them are
    /// replaced: only the latest matters.
    pub fn post(&self, view: View) {
        *self.latest.lock().unwrap() = Some(view);
        unsafe { PostMessageW(self.window as HWND, WM_VIEW, 0, 0) };
    }
}

unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_VIEW {
        DELIVERY.with_borrow(|delivery| {
            if let Some((latest, deliver)) = delivery
                && let Some(view) = latest.lock().unwrap().take()
            {
                deliver(view);
            }
        });
        return 0;
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
