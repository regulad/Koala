//! The camera's video, recorded: `koala guest --camera PATH` asks the
//! iPhone for its camera (entity 1) as well as its microphones, in the same
//! session (`docs/guest-pairing.md`, "Both at once"), and writes
//! what it sends to PATH as a Matroska file: HEVC as it arrives, not
//! re-encoded (`koala_capture::video`), in FFmpeg's Matroska muxer
//! (`crate::mkv`). Windows plays it with Microsoft's HEVC Video Extensions.
//! The iPhone's AVC offer is kept beside it, as `PATH.offer.bplist`.
//!
//! The recording starts at the first picture a decoder can start at: the
//! iPhone sent one, at the start of the session, in trials 23 and 24. A
//! recording cut short, Koala killed, still plays to where it stopped.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::OnceLock;

use koala_capture::srtp;
use koala_capture::video::{self, AccessUnit, Depacketizer, nal, nal_type};

use crate::mkv::MkvWriter;

/// Where `--camera` records the video, if anywhere: also what adds the
/// camera to the session.
pub static RECORD: OnceLock<PathBuf> = OnceLock::new();

/// The camera's stream: counted, and recorded to `RECORD` if it's set.
pub struct Recording {
    path: Option<PathBuf>,
    /// Whether the recording is still to start, at the first keyframe.
    waiting: bool,
    recording: Option<MkvWriter>,
    depacketizer: Depacketizer,
    /// The latest parameter sets: VPS, SPS, PPS.
    parameters: [Option<Vec<u8>>; 3],
    pub packets: u64,
    pub bytes: u64,
    payload_types: BTreeMap<u8, u64>,
    /// Pictures before the first keyframe, which aren't recorded.
    skipped: u64,
}

impl Recording {
    /// Records to `RECORD`, or counts only.
    pub fn open() -> Self {
        let path = RECORD.get().cloned();
        // Made now, so a path that can't be written fails before the stream.
        let writable = path.as_ref().is_some_and(|path| match File::create(path) {
            Ok(_) => {
                println!("camera: recording to {}", path.display());
                true
            }
            Err(e) => {
                println!("camera: can't record to {}: {e}", path.display());
                false
            }
        });
        Self::to(path, writable)
    }

    fn to(path: Option<PathBuf>, waiting: bool) -> Self {
        Self {
            path,
            waiting,
            recording: None,
            depacketizer: Depacketizer::default(),
            parameters: [None, None, None],
            packets: 0,
            bytes: 0,
            payload_types: BTreeMap::new(),
            skipped: 0,
        }
    }

    /// One authenticated, decrypted RTP packet, whole.
    pub fn packet(&mut self, rtp: &[u8]) {
        let Some(header) = srtp::rtp_header_len(rtp) else { return };
        self.packets += 1;
        self.bytes += rtp.len() as u64;
        *self.payload_types.entry(rtp[1] & 0x7f).or_default() += 1;
        if !self.waiting && self.recording.is_none() {
            return;
        }
        let seq = u16::from_be_bytes([rtp[2], rtp[3]]);
        let timestamp = u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]);
        let marker = rtp[1] & 0x80 != 0;
        for unit in self.depacketizer.push(seq, timestamp, marker, &rtp[header..]) {
            self.picture(unit);
        }
    }

    fn picture(&mut self, unit: AccessUnit) {
        for n in &unit.nals {
            let slot = match nal_type(n) {
                Some(nal::VPS) => 0,
                Some(nal::SPS) => 1,
                Some(nal::PPS) => 2,
                _ => continue,
            };
            if self.recording.is_some() && self.parameters[slot].as_ref().is_some_and(|p| p != n) {
                println!("camera: the stream's parameter sets changed; the recording keeps its first");
            }
            self.parameters[slot] = Some(n.clone());
        }
        if self.recording.is_none() && !self.start(&unit) {
            self.skipped += 1;
            if self.skipped == 1 {
                println!("camera: waiting for a picture to start the recording at");
            }
            return;
        }
        let Some(recording) = &mut self.recording else { return };
        if let Err(e) = recording.write_sample(unit.timestamp, unit.nals.iter().map(Vec::as_slice), unit.is_keyframe())
        {
            println!("camera: writing the recording: {e}; it stops here");
            self.recording = None;
        }
    }

    /// Starts the recording at a keyframe, if this is one and the parameter sets
    /// have come.
    fn start(&mut self, unit: &AccessUnit) -> bool {
        let [Some(vps), Some(sps), Some(pps)] = &self.parameters else { return false };
        if !unit.is_keyframe() {
            return false;
        }
        let Some(path) = self.path.as_ref().filter(|_| self.waiting) else { return false };
        self.waiting = false;
        match MkvWriter::create(path, video::CLOCK_RATE, vps, sps, pps) {
            Ok(recording) => {
                if self.skipped > 0 {
                    println!("camera: recording from here, {} pictures in", self.skipped);
                }
                self.recording = Some(recording);
                true
            }
            Err(e) => {
                println!("camera: can't start the recording: {e}");
                false
            }
        }
    }

    pub fn summary(&self) -> String {
        let pictures = self.recording.as_ref().map_or(0, MkvWriter::samples);
        format!(
            "video: {} packets, {} KB, {pictures} pictures recorded, {} packets lost, payload types {:?}",
            self.packets,
            self.bytes / 1024,
            self.depacketizer.lost,
            self.payload_types
        )
    }
}

impl Drop for Recording {
    /// Ends the recording: its last picture, then its index.
    fn drop(&mut self) {
        if let Some(unit) = self.depacketizer.flush() {
            self.picture(unit);
        }
        let Some(path) = &self.path else { return };
        let Some(recording) = self.recording.take() else {
            if self.waiting {
                println!("camera: no picture to start at arrived; {} is empty", path.display());
            }
            return;
        };
        let (pictures, seconds) = (recording.samples(), recording.seconds());
        match recording.finish() {
            Ok(_) => println!("camera: {pictures} pictures, {seconds:.1} s, written to {}", path.display()),
            Err(e) => println!("camera: finishing {}: {e}", path.display()),
        }
    }
}

/// Keeps the iPhone's AVC offer beside the recording.
pub fn save_offer(offer: &[u8]) {
    let Some(path) = RECORD.get() else { return };
    let mut name = path.clone().into_os_string();
    name.push(".offer.bplist");
    match std::fs::write(&name, offer) {
        Ok(()) => println!("capture: the AVC offer saved to {}", PathBuf::from(name).display()),
        Err(e) => println!("capture: the AVC offer couldn't be saved: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_are_counted_by_type() {
        let mut camera = Recording::to(None, false);
        let mut rtp = [0u8; 20];
        rtp[0] = 0x80;
        rtp[1] = 0x80 | 100;
        camera.packet(&rtp);
        rtp[1] = 100;
        camera.packet(&rtp);
        camera.packet(&[0x80; 5]);
        assert_eq!((camera.packets, camera.bytes), (2, 40));
        assert!(camera.summary().contains("{100: 2}"));
    }

    /// A trial's camera packets, recorded by an earlier Koala (each
    /// decrypted RTP packet after its length, a big-endian u16), made into
    /// a recording: `KOALA_CAMERA_DUMP=trial.rtp cargo test -- --ignored`,
    /// then check `trial.rtp.mkv` with ffprobe or a player.
    #[test]
    #[ignore = "needs a trial's packets"]
    fn a_trials_packets_make_a_recording() {
        let Some(dump) = std::env::var_os("KOALA_CAMERA_DUMP").map(PathBuf::from) else { return };
        let data = std::fs::read(&dump).unwrap();
        let mut out = dump.into_os_string();
        out.push(".mkv");
        let out = PathBuf::from(out);
        let mut camera = Recording::to(Some(out.clone()), true);
        let mut rest = &data[..];
        while let [high, low, tail @ ..] = rest {
            let len = usize::from(u16::from_be_bytes([*high, *low]));
            camera.packet(&tail[..len]);
            rest = &tail[len..];
        }
        assert!(camera.recording.as_ref().is_some_and(|m| m.samples() > 0));
        drop(camera);
        assert!(std::fs::metadata(&out).unwrap().len() > 0);
    }
}
