//! About Koala: the version, the licence, what Koala sends for diagnostics
//! (`PRIVACY.md`, in short), and the licences of everything built into it
//! (`data/third-party-notices.txt`, from `tools/third_party_notices.py`).

use windows_reactor::*;

const LICENCE: &str = include_str!("../../../../LICENSE.md");
const NOTICES: &str = include_str!("../../data/third-party-notices.txt");
/// `PRIVACY.md`, in short (`crate::diagnostics`).
const PRIVACY: &str = "To find and fix problems, Koala sends crash reports and diagnostics to Sentry (sentry.io): \
                       errors, usage statistics, Koala's and Warlock's versions, this computer's Windows version, \
                       processor and memory, and the iPhone's model and iOS version. Nothing that identifies you or \
                       your devices is sent: no names, IDs or addresses. To send nothing, set the environment \
                       variable DO_NOT_TRACK to 1. Privacy policy: https://github.com/regulad/Koala/blob/master/PRIVACY.md";

#[derive(Clone, PartialEq)]
pub struct Input {
    /// Warlock's version, once it has answered.
    pub warlock: Option<String>,
}

pub struct AboutWindow;

impl Component for AboutWindow {
    type Input = Input;
    type Message = ();

    fn create(_: &Input, context: &ComponentContext<Self>) -> Self {
        // Centred on the primary monitor (Reactor opens windows where Windows
        // puts new ones).
        let _ = context.run_window(|handle| center(handle.as_raw()));
        Self
    }

    fn view(&self, input: &Input, context: &mut ViewContext<Self>) -> View {
        context.window_title("About Koala");
        context.window_visuals(WindowVisuals::new().client_size(640.0, 720.0).backdrop(WindowBackdrop::Mica));
        let author = env!("CARGO_PKG_AUTHORS").split('<').next().unwrap_or_default().trim();
        let mut version = format!("Version {}", env!("CARGO_PKG_VERSION"));
        if let Some(warlock) = &input.warlock {
            version.push_str(&format!(" · Warlock {warlock}"));
        }
        let paragraph = |text: String| -> View { TextBlock::new().text(text).text_wrapping(TextWrapping::Wrap).into() };
        ScrollViewer::new()
            .content(
                StackPanel::new().margin(Thickness::uniform(24.0)).spacing(12.0).children((
                    TextBlock::new().text("Koala").font_size(28.0).font_weight(FontWeight::SEMI_BOLD),
                    TextBlock::new().text(version).opacity(0.75),
                    paragraph("An iPhone as a Windows microphone, with no app on the iPhone.".into()),
                    paragraph(format!("Copyright © 2026 {author}")),
                    paragraph(
                        "Koala is free software: you can redistribute it and modify it under the terms of the GNU \
                         Affero General Public License, version 3 or (at your option) any later version. It comes \
                         with no warranty. The licence, and those of the software Koala is built from, are below."
                            .into(),
                    ),
                    paragraph(
                        "Koala is not affiliated with Apple. Apple TV, iPhone, AWDL and Continuity are Apple's.".into(),
                    ),
                    TextBlock::new()
                        .text("Privacy")
                        .font_size(20.0)
                        .font_weight(FontWeight::SEMI_BOLD)
                        .margin(Thickness::new(0.0, 12.0, 0.0, 0.0)),
                    paragraph(PRIVACY.into()),
                    TextBlock::new()
                        .text("Licences")
                        .font_size(20.0)
                        .font_weight(FontWeight::SEMI_BOLD)
                        .margin(Thickness::new(0.0, 12.0, 0.0, 0.0)),
                    licences(LICENCE),
                    licences(NOTICES),
                )),
            )
            .into()
    }
}

fn center(raw: *mut core::ffi::c_void) {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };
    let window = raw as HWND;
    let work = super::display::primary().work;
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe {
        GetWindowRect(window, &mut rect);
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let x = work.left + (work.right - work.left - width) / 2;
        let y = work.top + (work.bottom - work.top - height) / 2;
        SetWindowPos(
            window,
            std::ptr::null_mut(),
            x,
            y.max(work.top),
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// A licence text, in a monospace face (they're laid out for one), selectable.
fn licences(text: &str) -> View {
    TextBlock::new()
        .text(text)
        .font_family("Cascadia Mono, Consolas")
        .font_size(12.0)
        .text_wrapping(TextWrapping::Wrap)
        .is_text_selection_enabled(true)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a, 64 bits: what `tools/third_party_notices.py` hashes
    /// Cargo.lock with.
    fn fnv1a64(data: &[u8]) -> u64 {
        data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
    }

    #[test]
    fn the_notices_are_for_the_dependencies_in_use() {
        let lock = fnv1a64(include_bytes!("../../../../Cargo.lock"));
        assert!(
            NOTICES.contains(&format!("from Cargo.lock {lock:016x}.")),
            "Cargo.lock changed: run `python -I tools/third_party_notices.py`"
        );
    }
}
