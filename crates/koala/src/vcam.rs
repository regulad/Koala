//! The virtual camera's pictures (`docs/virtual-camera.md`): the iPhone's
//! camera decoded ([`crate::decode`]) and written to the camera's shared
//! memory (`koala_vcam::frames`), from which Windows' camera service gives
//! them to apps.
//!
//! The camera itself is `koala_vcam::VirtualCamera`, made by whoever runs
//! the session (`koala guest --vcam`, `koala vcam`), who then sets [`ON`].
//! Each session's camera stream gets a [`FeedThread`]: decoding is heavy,
//! and the stream's packets come on Tokio's threads. A decoder starts at the
//! stream's first keyframe, which is its only one (trial 23): pictures are
//! decoded from it whether or not an app is watching, so one that opens the
//! camera later has them at once.
//!
//! While the stream lives and no picture comes (the iPhone paused), the
//! camera holds the last one ([`HOLD`]); it goes black when the session
//! ends, and with it the feed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use koala_capture::video::{self, AccessUnit, Sps, annex_b, nal, nal_type};
use koala_vcam::frames::{Names, Writer, current_user_sid};

use crate::decode::{Decoder, Picture};

/// Whether the virtual camera is up: asks the iPhone for its camera, and
/// feeds it.
pub static ON: AtomicBool = AtomicBool::new(false);

/// How often Koala looks for the camera's shared memory while it isn't
/// there (no app has started the camera yet).
const LOOK: Duration = Duration::from_millis(500);

/// How often the feed tells the camera it's still there while no picture
/// comes, well within the source's `koala_vcam::frames::STALE_MS`, after
/// which it would show black: so the camera holds the last picture.
const HOLD: Duration = Duration::from_millis(500);

/// One stream's pictures, decoded and written to the camera.
pub struct Feed {
    software: bool,
    decoder: Option<Decoder>,
    /// The latest parameter sets: VPS, SPS, PPS.
    parameters: [Option<Vec<u8>>; 3],
    names: Names,
    writer: Option<Writer>,
    looked: Option<Instant>,
    /// The latest picture, which the camera holds while no other comes.
    last: Option<Picture>,
    pub pictures: u64,
    errors: u64,
}

impl Feed {
    /// For this user's camera, decoding in software if the settings say.
    pub fn new() -> Result<Self, String> {
        let sid = current_user_sid()?;
        Ok(Self {
            software: crate::settings::software_decoding(),
            decoder: None,
            parameters: [None, None, None],
            names: Names::for_user(&sid),
            writer: None,
            looked: None,
            last: None,
            pictures: 0,
            errors: 0,
        })
    }

    /// One access unit of the camera's stream, in decoding order.
    pub fn unit(&mut self, unit: &AccessUnit) {
        for n in &unit.nals {
            let slot = match nal_type(n) {
                Some(nal::VPS) => 0,
                Some(nal::SPS) => 1,
                Some(nal::PPS) => 2,
                _ => continue,
            };
            self.parameters[slot] = Some(n.clone());
        }
        let first = self.decoder.is_none();
        if first && !self.start(unit) {
            return;
        }
        // The first gets the parameter sets ahead of it, wherever they came.
        let data = if first {
            let parameters = self.parameters.iter().flatten().filter(|p| !unit.nals.contains(p));
            annex_b(parameters.chain(&unit.nals))
        } else {
            annex_b(&unit.nals)
        };
        let time = i64::from(unit.timestamp) * 10_000_000 / i64::from(video::CLOCK_RATE);
        let Some(decoder) = &mut self.decoder else { return };
        match decoder.decode(&data, time) {
            Ok(pictures) => {
                for picture in pictures {
                    self.write(&picture);
                    self.last = Some(picture);
                }
            }
            Err(e) => {
                self.errors += 1;
                if self.errors <= 3 || self.errors.is_power_of_two() {
                    println!("camera: decoding ({} errors so far): {e}", self.errors);
                }
            }
        }
    }

    /// The decoder, at a keyframe with its parameter sets come.
    fn start(&mut self, unit: &AccessUnit) -> bool {
        let Some(sps) = self.parameters[1].as_deref().and_then(Sps::parse) else { return false };
        if !unit.is_keyframe() || self.parameters.iter().any(Option::is_none) {
            return false;
        }
        match Decoder::new(self.software, sps.width, sps.height) {
            Ok(decoder) => {
                println!("camera: {}x{}, decoded by {}", sps.width, sps.height, decoder.describe());
                self.decoder = Some(decoder);
                true
            }
            Err(e) => {
                println!("camera: no decoder: {e}");
                false
            }
        }
    }

    fn write(&mut self, picture: &Picture) {
        self.pictures += 1;
        self.open();
        if let Some(writer) = &mut self.writer {
            writer.write(picture.width, picture.height, &picture.nv12);
        }
    }

    /// No picture for [`HOLD`]: the camera keeps the last. An app that
    /// opens the camera meanwhile gets it too.
    fn hold(&mut self) {
        let had = self.writer.is_some();
        self.open();
        match (&mut self.writer, &self.last) {
            (Some(writer), Some(last)) if !had => writer.write(last.width, last.height, &last.nv12),
            (Some(writer), _) => writer.keep_alive(),
            (None, _) => {}
        }
    }

    /// The camera's shared memory, once an app has started the camera.
    fn open(&mut self) {
        if self.writer.is_some() || self.looked.is_some_and(|t| t.elapsed() < LOOK) {
            return;
        }
        self.looked = Some(Instant::now());
        match Writer::open(&self.names) {
            Ok(Some(writer)) => {
                println!("camera: an app has the camera; its pictures go to it");
                self.writer = Some(writer);
            }
            Ok(None) => {}
            Err(e) => println!("camera: the camera's shared memory: {e}"),
        }
    }
}

/// A [`Feed`] on a thread of its own, taking access units as they come.
pub struct FeedThread {
    units: Option<SyncSender<AccessUnit>>,
    thread: Option<JoinHandle<()>>,
    dropped: u64,
}

impl FeedThread {
    /// A feed, if the virtual camera is up.
    pub fn start() -> Option<Self> {
        if !ON.load(Ordering::Relaxed) {
            return None;
        }
        // Four seconds of pictures: decoding runs ahead of the camera.
        let (units, received) = sync_channel::<AccessUnit>(120);
        // The feed made on its thread, where its decoder's COM objects live.
        let thread = std::thread::Builder::new()
            .name("camera feed".into())
            .spawn(move || {
                let mut feed = match Feed::new() {
                    Ok(feed) => feed,
                    Err(e) => return println!("camera: no feed for the virtual camera: {e}"),
                };
                loop {
                    match received.recv_timeout(HOLD) {
                        Ok(unit) => feed.unit(&unit),
                        Err(RecvTimeoutError::Timeout) => feed.hold(),
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                println!("camera: {} pictures to the virtual camera", feed.pictures);
            })
            .ok()?;
        Some(Self { units: Some(units), thread: Some(thread), dropped: 0 })
    }

    pub fn send(&mut self, unit: AccessUnit) {
        let Some(units) = &self.units else { return };
        match units.try_send(unit) {
            Ok(()) => {}
            // The decoder that far behind: the picture goes, and its
            // successors decode with a gap.
            Err(TrySendError::Full(_)) => {
                self.dropped += 1;
                if self.dropped.is_power_of_two() {
                    println!("camera: the decoder is behind; {} pictures dropped", self.dropped);
                }
            }
            Err(TrySendError::Disconnected(_)) => self.units = None,
        }
    }
}

impl Drop for FeedThread {
    /// The stream's over: the feed finishes what it has, and ends.
    fn drop(&mut self) {
        drop(self.units.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
