//! The microphones in Windows (PLAN.md, "Audio out"): each of the iPhone's
//! microphones a USB Audio Class 2 capture device of its own, one channel at
//! 48 kHz, served over USB/IP on the loopback by usbip-audio and attached
//! through usbip-win2 (0.9.8.0 or later: the README), so that Windows' own
//! driver makes it a microphone. Windows names one by its device's product
//! string, not its terminal's (the `usb_mics` example found), so that's
//! where the name goes: "Microphone (Parker’s iPhone: Back)" (Parker,
//! 2026-11-21).
//!
//! - **Ids:** pid.codes' 1209:4B4F, requested (pidcodes/pidcodes.github.com
//!   #1398). The serial is the microphone's and the iPhone's name's, hashed,
//!   so another iPhone, or one renamed, is another device, with its own name:
//!   Windows keeps a device's name from when it first saw it.
//! - **When:** attached once the stream's first packet says which
//!   microphones there are, on a thread of their own, so that the audio
//!   isn't held up; detached when the session's microphone stream ends.
//! - **The clock** ([`Following`]): usbip-audio completes the devices'
//!   transfers by a clock of Koala's, which so is the rate Windows records
//!   at. It follows the iPhone's: a little faster when the samples waiting
//!   grow, a little slower when they shrink. No resampling, and no drift.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use koala_capture::audio::{FRAME_SAMPLES, SAMPLE_RATE};
use koala_capture::mic_map::Layout;
use usbip_audio::uac2::{Uac2Config, Uac2Device, Uac2Streams};
use usbip_audio::{Clock, Server};

use crate::mic::{Block, Discard, Output, Sink};

/// Whether sessions give Windows the iPhone's microphones: the tray's
/// "Stream microphones", or `koala guest --usb-mics`.
pub static ON: AtomicBool = AtomicBool::new(false);

/// pid.codes' VID, and the PID asked for there.
const VENDOR: u16 = 0x1209;
const PRODUCT: u16 = 0x4B4F;

/// What's kept waiting for Windows, in samples: 40 ms. More than that
/// speeds the clock up, less slows it.
const TARGET: usize = 1_920;
/// The most kept while Windows records, should it stall: beyond it, back
/// to [`TARGET`]. While nothing records, no more than the target is kept.
const MOST: usize = 9_600;
/// The clock's correction per sample off target, and its limit: 0.2 ppm a
/// sample, at most 1000 ppm. An iPhone's crystal is within some tens.
const PPM_PER_SAMPLE: f64 = 0.2;
const MOST_PPM: f64 = 1_000.0;
/// How often the clock is corrected, in blocks of 10 ms: every half second.
const CORRECT_EVERY: u64 = 50;

/// usbip-win2's client, where its installer puts it.
fn usbip() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from)
        .join(r"USBip\usbip.exe")
}

/// Gives a session's microphones to Windows, named after `iphone`.
pub struct UsbMicrophones {
    pub iphone: String,
}

impl Output for UsbMicrophones {
    fn open(&mut self, layout: &Layout) -> Vec<Box<dyn Sink>> {
        let clock = Arc::new(Following::new());
        let server = match Server::start(0, clock.clone()) {
            Ok(server) => server,
            Err(e) => {
                println!("microphones: no USB/IP server: {e}");
                return Discard.open(layout);
            }
        };
        let pipes: Vec<Arc<Pipe>> = layout.mics.iter().map(|_| Arc::new(Pipe::default())).collect();
        let mut busids = Vec::new();
        for (mic, pipe) in layout.mics.iter().zip(&pipes) {
            let product = format!("{}: {}", self.iphone, mic.name);
            let config = Uac2Config {
                vendor_id: VENDOR,
                product_id: PRODUCT,
                bcd_device: 0x0100,
                manufacturer: "Koala".into(),
                serial: serial(&self.iphone, &mic.id),
                product,
                sample_rate: SAMPLE_RATE,
                playback_channels: 0,
                capture_channels: 1,
                playback_name: None,
                capture_name: Some(mic.name.clone()),
                capture_connector: false,
            };
            match Uac2Device::new(config, pipe.clone()) {
                Ok(device) => {
                    let busid = format!("1-{}", busids.len() + 1);
                    server.add(&busid, Arc::new(device));
                    busids.push(busid);
                }
                Err(e) => println!("microphones: {}: {e}", mic.name),
            }
        }
        let port = server.port();
        let attaching =
            std::thread::Builder::new().name("microphones attaching".into()).spawn(move || attach(port, &busids)).ok();
        let attachment = Arc::new(Attachment {
            _server: server,
            attaching: Mutex::new(attaching),
            pipes: pipes.clone(),
            clock: clock.clone(),
        });
        pipes
            .into_iter()
            .enumerate()
            .map(|(n, pipe)| {
                // The first microphone's waiting samples steer the clock:
                // they all arrive together.
                let steer = (n == 0).then(|| clock.clone());
                Box::new(UsbSink { pipe, steer, blocks: 0, _attachment: attachment.clone() }) as Box<dyn Sink>
            })
            .collect()
    }
}

/// A device's serial: the microphone's id, and a hash of the iPhone's name.
fn serial(iphone: &str, mic: &str) -> String {
    // FNV-1a: stable across builds, as Windows needs it to be.
    let hash = iphone.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{mic}-{:08x}", hash as u32)
}

/// Attaches each device, returning the ports to detach.
fn attach(port: u16, busids: &[String]) -> Vec<u32> {
    let mut ports = Vec::new();
    for busid in busids {
        let out = Command::new(usbip())
            .args(["--tcp-port", &port.to_string(), "attach", "-r", "127.0.0.1", "-b", busid])
            .output();
        match out {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
                // "successfully attached to port N"
                match text.split_whitespace().last().and_then(|w| w.trim_end_matches('.').parse().ok()) {
                    Some(p) if out.status.success() => ports.push(p),
                    _ => println!("microphones: attaching {busid}: {}", text.trim()),
                }
            }
            Err(e) => println!("microphones: usbip-win2's usbip.exe: {e}"),
        }
    }
    println!("microphones: {} attached", ports.len());
    ports
}

/// The session's devices: served, attached, and detached when the last of
/// their sinks goes.
struct Attachment {
    _server: Server,
    attaching: Mutex<Option<JoinHandle<Vec<u32>>>>,
    /// For the session's summary: how often Windows found a microphone
    /// short, and where the clock ended.
    pipes: Vec<Arc<Pipe>>,
    clock: Arc<Following>,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let attaching = self.attaching.lock().unwrap_or_else(|p| p.into_inner()).take();
        let ports = attaching.and_then(|t| t.join().ok()).unwrap_or_default();
        // Only the ports Koala attached, as usbip-audio asks of everything
        // sharing the driver.
        for port in &ports {
            let _ = Command::new(usbip()).args(["detach", "--port", &port.to_string()]).output();
        }
        if !ports.is_empty() {
            let short: Vec<u64> = self.pipes.iter().map(|p| p.short.load(Ordering::Relaxed)).collect();
            println!(
                "microphones: {} detached; samples Windows found missing, by microphone, {short:?}; the clock at {:+.0} \
                 ppm, {} samples waiting",
                ports.len(),
                self.clock.ppm(),
                self.pipes.first().map_or(0, |p| p.waiting())
            );
        }
    }
}

/// One microphone's samples, from its decoder to its device.
struct UsbSink {
    pipe: Arc<Pipe>,
    steer: Option<Arc<Following>>,
    blocks: u64,
    _attachment: Arc<Attachment>,
}

impl Sink for UsbSink {
    fn write(&mut self, block: &Block) {
        self.pipe.push(block);
        self.blocks += 1;
        if let Some(clock) = &self.steer
            && self.blocks.is_multiple_of(CORRECT_EVERY)
            && self.pipe.recording()
        {
            let off = self.pipe.waiting() as f64 - TARGET as f64;
            clock.set_ppm((off * PPM_PER_SAMPLE).clamp(-MOST_PPM, MOST_PPM));
        }
    }
}

/// A microphone's samples waiting for Windows, as usbip-audio's subslots:
/// 24 bits at the top of an `i32`.
#[derive(Default)]
struct Pipe {
    samples: Mutex<VecDeque<i32>>,
    /// When Windows last took samples (ms since the pipe's first), or 0.
    taken_at: AtomicU64,
    start: std::sync::OnceLock<Instant>,
    /// Samples Windows wanted that hadn't come.
    short: AtomicU64,
}

impl Pipe {
    fn now_ms(&self) -> u64 {
        self.start.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
    }

    fn push(&self, block: &Block) {
        let mut samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        samples.extend(block.iter().map(|&s| i32::from(s) << 16));
        // Nothing recording: the oldest go, so that a recording starts at
        // the target. (Kept to 200 ms, trial 34's started up to that late,
        // which the clock's correction takes minutes to work off.) Windows
        // stalled: the same.
        let most = if self.recording() { MOST } else { TARGET };
        if samples.len() > most {
            let extra = samples.len() - TARGET;
            samples.drain(..extra);
        }
    }

    fn waiting(&self) -> usize {
        self.samples.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Whether Windows has taken samples in the last 200 ms.
    fn recording(&self) -> bool {
        let at = self.taken_at.load(Ordering::Relaxed);
        at != 0 && self.now_ms().saturating_sub(at) < 200
    }
}

impl Uac2Streams for Pipe {
    fn playback(&self, _frame: u64, _samples: &[i32]) {}

    fn capture(&self, _frame: u64, out: &mut [i32]) {
        self.taken_at.store(self.now_ms(), Ordering::Relaxed);
        let mut samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        for slot in out.iter_mut() {
            *slot = match samples.pop_front() {
                Some(s) => s,
                None => {
                    self.short.fetch_add(1, Ordering::Relaxed);
                    0
                }
            };
        }
    }
}

/// A clock that runs at real time's rate, corrected by parts per million.
struct Following {
    /// When the rate last changed, the clock's reading then, and the rate.
    state: Mutex<(Instant, u64, f64)>,
}

impl Following {
    fn new() -> Self {
        Self { state: Mutex::new((Instant::now(), 0, 1.0)) }
    }

    fn ppm(&self) -> f64 {
        (self.state.lock().unwrap_or_else(|p| p.into_inner()).2 - 1.0) * 1e6
    }

    fn set_ppm(&self, ppm: f64) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let (since, at, rate) = *state;
        let now = at + (since.elapsed().as_nanos() as f64 * rate) as u64;
        *state = (Instant::now(), now, 1.0 + ppm / 1e6);
    }
}

impl Clock for Following {
    fn now_ns(&self) -> u64 {
        let (since, at, rate) = *self.state.lock().unwrap_or_else(|p| p.into_inner());
        at + (since.elapsed().as_nanos() as f64 * rate) as u64
    }
}

/// 10 ms of a microphone: what a [`Pipe`] takes in at a time.
const _: () = assert!(FRAME_SAMPLES == 480);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serials_differ_by_iphone_and_microphone() {
        let a = serial("Parker’s iPhone", "back");
        assert!(a.starts_with("back-"));
        assert_ne!(a, serial("Parker’s iPhone", "front"));
        assert_ne!(a, serial("Another iPhone", "back"));
        assert_eq!(a, serial("Parker’s iPhone", "back"), "stable");
    }

    #[test]
    fn an_unread_pipe_keeps_the_target_only() {
        let pipe = Pipe::default();
        let block = [0i16; FRAME_SAMPLES];
        for _ in 0..20 {
            pipe.push(&block);
        }
        assert!(!pipe.recording());
        assert_eq!(pipe.waiting(), TARGET, "a recording would start 40 ms behind, not more");
    }

    #[test]
    fn a_pipe_keeps_order_pads_short_and_trims_when_unread() {
        let pipe = Pipe::default();
        let block: Block = std::array::from_fn(|i| i as i16);
        pipe.push(&block);
        let mut out = [7i32; 500];
        pipe.capture(0, &mut out);
        assert_eq!(out[1], 1 << 16);
        assert_eq!(out[479], 479 << 16);
        assert_eq!(&out[480..], &[0; 20], "short: zeros");
        assert_eq!(pipe.short.load(Ordering::Relaxed), 20);
        assert!(pipe.recording());
        for _ in 0..30 {
            pipe.push(&block);
        }
        assert!(pipe.waiting() > TARGET, "recording: more may wait");
        assert!(pipe.waiting() <= MOST, "but never more than the most");
    }

    #[test]
    fn the_clock_follows_its_rate() {
        let clock = Following::new();
        clock.set_ppm(1_000.0);
        let (a, start) = (clock.now_ns(), Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(200));
        let ran = clock.now_ns() - a;
        let real = start.elapsed().as_nanos() as u64;
        assert!(ran > real, "{ran} ns on the clock in {real} ns: faster");
        assert!(ran < real + real / 100, "but only by about 0.1%");
    }
}
