//! A spike for the microphones (PLAN.md, "Audio out"): four mono USB Audio
//! Class 2 microphones, one per iPhone microphone, served over USB/IP by
//! usbip-audio and attached through usbip-win2, each playing a test tone of
//! its own (220, 440, 660 and 880 Hz). Whether Windows makes one microphone
//! of each, named by its terminal and the device, is what it checks.
//!
//! ```text
//! cargo run --example usb_mics -- [SECONDS, default 60]
//! ```
//!
//! It uses pid.codes' testing VID/PID (1209:0007), as a spike may.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use usbip_audio::uac2::{Uac2Config, Uac2Device, Uac2Streams};
use usbip_audio::{MonotonicClock, Server};

const USBIP: &str = r"C:\Program Files\USBip\usbip.exe";
const RATE: u32 = 48_000;

/// A sine at `hz`, a quarter of full scale, 24 bits in a 32-bit subslot.
struct Tone {
    hz: f64,
}

impl Uac2Streams for Tone {
    fn playback(&self, _frame: u64, _samples: &[i32]) {}

    fn capture(&self, frame: u64, samples: &mut [i32]) {
        for (i, sample) in samples.iter_mut().enumerate() {
            let t = (frame + i as u64) as f64 / f64::from(RATE);
            let value = (t * self.hz * std::f64::consts::TAU).sin() * 0.25 * f64::from(i32::MAX);
            *sample = (value as i32) & !0xff;
        }
    }
}

fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    let server = Server::start(0, Arc::new(MonotonicClock::new())).expect("the USB/IP server");
    let port = server.port();
    println!("serving on 127.0.0.1:{port}");
    let names = ["Front", "Back", "Bottom 1", "Bottom 2"];
    for (n, name) in names.iter().enumerate() {
        let config = Uac2Config {
            vendor_id: 0x1209,
            product_id: 0x0007,
            bcd_device: 0x0100,
            manufacturer: "Koala".into(),
            product: "Koala spike".into(),
            serial: format!("spike-{n}"),
            sample_rate: RATE,
            playback_channels: 0,
            capture_channels: 1,
            playback_name: None,
            capture_name: Some((*name).into()),
            capture_connector: false,
        };
        let device = Uac2Device::new(config, Tone { hz: 220.0 * (n + 1) as f64 }).expect("a device");
        server.add(&format!("1-{}", n + 1), Arc::new(device));
    }
    let mut ports = Vec::new();
    for n in 1..=names.len() {
        let out = Command::new(USBIP)
            .args(["--tcp-port", &port.to_string(), "attach", "-r", "127.0.0.1", "-b", &format!("1-{n}")])
            .output()
            .expect("usbip.exe");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        println!("attach 1-{n}: {} {}", out.status, text.trim());
        // "succesfully attached to port N" (sic): the port to detach.
        if let Some(p) = text.split_whitespace().last().and_then(|w| w.trim_end_matches('.').parse::<u32>().ok()) {
            ports.push(p);
        }
    }
    println!("attached on ports {ports:?}; {seconds} s");
    std::thread::sleep(Duration::from_secs(seconds));
    for p in ports {
        let out = Command::new(USBIP).args(["detach", "--port", &p.to_string()]).output().expect("usbip.exe");
        println!("detach {p}: {} {}", out.status, String::from_utf8_lossy(&out.stdout).trim());
    }
}
