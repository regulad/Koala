//! `koala`, the console tool for trials (PLAN.md). For now, one command, the
//! K2 trial:
//!
//! ```text
//! koala guest [SECONDS] [--session SECONDS] [--mic-wav PATH | --camera PATH]
//! ```
//!
//! It plays an Apple TV offering guest pairing (`docs/guest-pairing.md`):
//! shows a QR code for the iPhone's camera, advertises `_rdlink._tcp` over
//! AWDL through Warlock, and pairs with the iPhone that connects, printing
//! every frame. It needs Warlock running, and Administrator rights or
//! membership of `warlock-users`. `koala-tray` is the app.

use std::time::Duration;

use koala::{camera, guest, mic};

/// How long the trial follows a capture session, unless `--session` says.
const SESSION_TIME: Duration = Duration::from_secs(120);
const USAGE: &str = "usage: koala guest [SECONDS to wait, default 600] [--session SECONDS, default 120] \
                     [--mic-wav PATH | --camera PATH.mkv]";

/// Takes `flag PATH` out of `args`: the PATH, if the flag is there.
fn take_path(args: &mut Vec<String>, flag: &str) -> Option<std::path::PathBuf> {
    let at = args.iter().position(|a| a == flag)?;
    let Some(path) = args.get(at + 1).cloned() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    args.drain(at..at + 2);
    Some(path.into())
}

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--mic-wav PATH`: record each of the iPhone's microphones to a WAV of
    // its own, named from PATH (`trial.wav` gives `trial-back.wav`, …).
    if let Some(path) = take_path(&mut args, "--mic-wav") {
        let _ = mic::RECORD.set(path);
    }
    // `--camera PATH`: ask for the camera in place of the microphone, and
    // record its video to PATH, a Matroska file (`koala::camera`).
    if let Some(path) = take_path(&mut args, "--camera") {
        let _ = camera::RECORD.set(path);
    }
    // `--session SECONDS`: how long to follow the capture session.
    let session = match take_path(&mut args, "--session").map(|s| s.to_string_lossy().parse::<u64>()) {
        None => SESSION_TIME,
        Some(Ok(seconds)) => Duration::from_secs(seconds),
        Some(Err(_)) => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let seconds = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["guest"] => 600,
        ["guest", seconds] if seconds.parse::<u64>().is_ok() => seconds.parse().unwrap(),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let options = guest::Options {
        wait: Some(Duration::from_secs(seconds)),
        session: Some(session),
        terminal: true,
        report: guest::Report::default(),
    };
    if let Err(e) = guest::run(options).await {
        eprintln!("koala: {e}");
        std::process::exit(1);
    }
}
