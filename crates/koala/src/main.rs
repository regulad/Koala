//! `koala`, the console tool for trials (PLAN.md):
//!
//! ```text
//! koala guest [SECONDS] [--session SECONDS] [--mic-wav PATH] [--usb-mics] [--camera PATH] [--vcam] [--software-decode]
//! koala vcam [--replay PATH.mkv] [--software-decode]
//! ```
//!
//! - `guest`, the K2 trial, plays an Apple TV offering guest pairing
//!   (`docs/guest-pairing.md`): shows a QR code for the iPhone's camera,
//!   advertises `_rdlink._tcp` over AWDL through Warlock, and pairs with the
//!   iPhone that connects, printing every frame. It needs Warlock running,
//!   and Administrator rights or membership of `warlock-users`. With
//!   `--vcam`, it makes the virtual camera for the run and gives it the
//!   iPhone's camera (`docs/virtual-camera.md`); with `--usb-mics`, it gives
//!   Windows the iPhone's microphones (`koala::usb_mics`).
//! - `vcam` makes the virtual camera, and with `--replay` plays a recording
//!   (`--camera`'s) into it, over and over, until Ctrl+C: the camera without
//!   an iPhone.
//!
//! `koala-tray` is the app.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use koala::{camera, diagnostics, guest, mic, settings, usb_mics, vcam};
use koala_vcam::VirtualCamera;

/// How long the trial follows a capture session, unless `--session` says.
const SESSION_TIME: Duration = Duration::from_secs(120);
const USAGE: &str = "usage: koala guest [SECONDS to wait, default 600] [--session SECONDS, default 120] \
                     [--mic-wav PATH] [--usb-mics] [--camera PATH.mkv] [--vcam] [--software-decode]\n       \
                     koala vcam [--replay PATH.mkv] [--software-decode]";

fn usage() -> ! {
    eprintln!("{USAGE}");
    std::process::exit(2);
}

/// Takes `flag PATH` out of `args`: the PATH, if the flag is there.
fn take_path(args: &mut Vec<String>, flag: &str) -> Option<std::path::PathBuf> {
    let at = args.iter().position(|a| a == flag)?;
    let Some(path) = args.get(at + 1).cloned() else { usage() };
    args.drain(at..at + 2);
    Some(path.into())
}

/// Takes `flag` out of `args`: whether it was there.
fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let Some(at) = args.iter().position(|a| a == flag) else { return false };
    args.remove(at);
    true
}

/// The virtual camera, made for the whole run (the tray's comes and goes
/// with the iPhone, and has its name), or why not; the process ends if it
/// can't be.
fn virtual_camera() -> VirtualCamera {
    let name = koala_vcam::DEFAULT_NAME;
    match VirtualCamera::start(name, crate::diagnostics::allowed()) {
        Ok(camera) => {
            vcam::ON.store(true, Ordering::Relaxed);
            println!("vcam: \"{name}\" is up, for apps to open");
            camera
        }
        Err(e) => {
            eprintln!("koala: no virtual camera: {e}");
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--mic-wav PATH`: record each of the iPhone's microphones to a WAV of
    // its own, named from PATH (`trial.wav` gives `trial-back.wav`, …).
    if let Some(path) = take_path(&mut args, "--mic-wav") {
        let _ = mic::RECORD.set(path);
    }
    // `--camera PATH`: ask for the camera too, in the same session as the
    // microphones, and record its video to PATH, a Matroska file
    // (`koala::camera`).
    if let Some(path) = take_path(&mut args, "--camera") {
        let _ = camera::RECORD.set(path);
    }
    // `--software-decode`: decode the camera with FFmpeg's decoder, as the
    // tray's "Force software decoding" does (`koala::settings`).
    if take_flag(&mut args, "--software-decode") {
        settings::SOFTWARE_DECODING_THIS_RUN.store(true, Ordering::Relaxed);
    }
    let replay = take_path(&mut args, "--replay");
    let with_vcam = take_flag(&mut args, "--vcam");
    // `--usb-mics`: give Windows the iPhone's microphones, as the tray does
    // (`koala::usb_mics`).
    if take_flag(&mut args, "--usb-mics") {
        usb_mics::ON.store(true, Ordering::Relaxed);
    }
    // `--session SECONDS`: how long to follow the capture session.
    let session = match take_path(&mut args, "--session").map(|s| s.to_string_lossy().parse::<u64>()) {
        None => SESSION_TIME,
        Some(Ok(seconds)) => Duration::from_secs(seconds),
        Some(Err(_)) => usage(),
    };
    let seconds = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["vcam"] => return vcam_command(replay).await,
        ["guest"] if replay.is_none() => 600,
        ["guest", seconds] if replay.is_none() && seconds.parse::<u64>().is_ok() => seconds.parse().unwrap(),
        _ => usage(),
    };
    let _camera = with_vcam.then(virtual_camera);
    // Diagnostics (PRIVACY.md), unless DO_NOT_TRACK is set.
    let diagnostics = diagnostics::start("koala");
    let options = guest::Options {
        wait: Some(Duration::from_secs(seconds)),
        session: Some(session),
        terminal: true,
        report: guest::Report::default(),
    };
    if let Err(e) = guest::run(options).await {
        eprintln!("koala: {e}");
        diagnostics::error(&e);
        // Exiting skips destructors: what's left goes first.
        drop(diagnostics);
        drop(_camera);
        std::process::exit(1);
    }
}

/// `koala vcam`: the camera until Ctrl+C, playing a recording if given one.
async fn vcam_command(replay: Option<std::path::PathBuf>) {
    let camera = virtual_camera();
    let stop = Arc::new(AtomicBool::new(false));
    let player = replay.map(|path| {
        let stop = stop.clone();
        std::thread::spawn(move || {
            if let Err(e) = play(&path, &stop) {
                eprintln!("koala: {}: {e}", path.display());
            }
        })
    });
    println!("vcam: Ctrl+C ends it");
    let _ = tokio::signal::ctrl_c().await;
    stop.store(true, Ordering::Relaxed);
    if let Some(player) = player {
        let _ = player.join();
    }
    drop(camera);
}

/// A recording into the camera, at its own pace, over and over.
fn play(path: &std::path::Path, stop: &AtomicBool) -> Result<(), String> {
    let units = koala::mkv::read(path)?;
    let first = units.first().ok_or("an empty recording")?.timestamp;
    println!("vcam: playing {} ({} pictures) into the camera", path.display(), units.len());
    while !stop.load(Ordering::Relaxed) {
        // A new feed each time round: its decoder starts at the first
        // picture again.
        let mut feed = vcam::Feed::new()?;
        let started = Instant::now();
        for unit in &units {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let at = f64::from(unit.timestamp.wrapping_sub(first)) / f64::from(koala_capture::video::CLOCK_RATE);
            if let Some(wait) = Duration::from_secs_f64(at).checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
            feed.unit(unit);
        }
    }
    Ok(())
}
