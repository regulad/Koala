//! `koala`, the console tool for trials (PLAN.md). For now, one command, the
//! K2 trial:
//!
//! ```text
//! koala guest [SECONDS] [--mic-wav PATH]
//! ```
//!
//! It plays an Apple TV offering guest pairing (`docs/guest-pairing.md`):
//! shows a QR code for the iPhone's camera, advertises `_rdlink._tcp` over
//! AWDL through Warlock, and pairs with the iPhone that connects, printing
//! every frame. It needs Warlock running, and Administrator rights or
//! membership of `warlock-users`. `koala-tray` is the app.

use std::time::Duration;

use koala::{guest, mic};

/// How long the trial follows a capture session.
const SESSION_TIME: Duration = Duration::from_secs(120);

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--mic-wav PATH`: record each of the iPhone's microphones to a WAV of
    // its own, named from PATH (`trial.wav` gives `trial-back.wav`, …).
    if let Some(at) = args.iter().position(|a| a == "--mic-wav") {
        let Some(path) = args.get(at + 1).cloned() else {
            eprintln!("usage: koala guest [SECONDS] [--mic-wav PATH]");
            std::process::exit(2);
        };
        let _ = mic::RECORD.set(path.into());
        args.drain(at..at + 2);
    }
    let seconds = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["guest"] => 600,
        ["guest", seconds] if seconds.parse::<u64>().is_ok() => seconds.parse().unwrap(),
        _ => {
            eprintln!("usage: koala guest [SECONDS to wait, default 600] [--mic-wav PATH]");
            std::process::exit(2);
        }
    };
    let options = guest::Options {
        wait: Some(Duration::from_secs(seconds)),
        session: Some(SESSION_TIME),
        terminal: true,
        report: guest::Report::default(),
    };
    if let Err(e) = guest::run(options).await {
        eprintln!("koala: {e}");
        std::process::exit(1);
    }
}
