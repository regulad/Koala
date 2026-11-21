//! `koala-tray`: Koala in the notification area (`koala::tray`).
//!
//! ```text
//! koala-tray [--mic-wav PATH] [--monitor]
//! ```
//!
//! A Windows program, without a console: what it prints goes to
//! `%LOCALAPPDATA%\Koala\koala-tray.log` unless its output is redirected.
//! `--mic-wav` records each microphone as `koala guest` does; `--monitor`
//! prints what Koala makes of the primary monitor, and exits.

#![windows_subsystem = "windows"]

use std::os::windows::io::AsRawHandle;

use koala::{diagnostics, mic, tray};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};

fn main() {
    tray::dpi_aware();
    let _log = log_to_file();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(at) = args.iter().position(|a| a == "--mic-wav")
        && let Some(path) = args.get(at + 1).cloned()
    {
        let _ = mic::RECORD.set(path.into());
        args.drain(at..at + 2);
    }
    if args.iter().any(|a| a == "--monitor") {
        println!("{}", tray::display::describe());
        return;
    }
    // Diagnostics (PRIVACY.md), unless DO_NOT_TRACK is set.
    let diagnostics = diagnostics::start("koala-tray");
    if let Err(e) = tray::run() {
        println!("koala-tray: {e}");
        diagnostics::error(&e);
        // Exiting skips destructors: what's left goes first.
        drop(diagnostics);
        std::process::exit(1);
    }
}

/// Sends what's printed to the log file, if it has nowhere else to go. The
/// file is kept open for as long as the value lives.
fn log_to_file() -> Option<std::fs::File> {
    let inherited = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if !inherited.is_null() && inherited as isize != -1 {
        return None;
    }
    let dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Koala");
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::File::create(dir.join("koala-tray.log")).ok()?;
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, file.as_raw_handle());
        SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle());
    }
    Some(file)
}
