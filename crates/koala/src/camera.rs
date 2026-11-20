//! The camera's video, recorded: `koala guest --camera PATH` asks the
//! iPhone for its camera (entity 1) in place of its microphone, and writes
//! what it sends to PATH as an MP4: HEVC as it arrives, not re-encoded
//! (`koala_capture::video`). Windows plays it with Microsoft's HEVC Video
//! Extensions. The iPhone's AVC offer is kept beside it, as
//! `PATH.offer.bplist`.
//!
//! The recording starts at the first picture a decoder can start at: the
//! iPhone sent one, at the start of the session, in trial 23. It's indexed
//! when the session ends; a recording Koala didn't get to end has no index,
//! and doesn't play.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::OnceLock;

use koala_capture::srtp;
use koala_capture::video::{self, AccessUnit, Depacketizer, nal, nal_type};

use crate::mp4::Mp4Writer;

/// Where `--camera` records the video, if anywhere: also what makes the
/// session the camera's.
pub static RECORD: OnceLock<PathBuf> = OnceLock::new();

/// The camera's stream: counted, and recorded to `RECORD` if it's set.
pub struct Recording {
    path: Option<PathBuf>,
    /// The file, until the first keyframe starts the MP4 in it.
    file: Option<File>,
    mp4: Option<Mp4Writer<BufWriter<File>>>,
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
        let file = path.as_ref().and_then(|path| match File::create(path) {
            Ok(f) => {
                println!("camera: recording to {}", path.display());
                Some(f)
            }
            Err(e) => {
                println!("camera: can't record to {}: {e}", path.display());
                None
            }
        });
        Self::to(path, file)
    }

    fn to(path: Option<PathBuf>, file: Option<File>) -> Self {
        Self {
            path,
            file,
            mp4: None,
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
        if self.file.is_none() && self.mp4.is_none() {
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
            if self.mp4.is_some() && self.parameters[slot].as_ref().is_some_and(|p| p != n) {
                println!("camera: the stream's parameter sets changed; the recording keeps its first");
            }
            self.parameters[slot] = Some(n.clone());
        }
        if self.mp4.is_none() && !self.start(&unit) {
            self.skipped += 1;
            if self.skipped == 1 {
                println!("camera: waiting for a picture to start the recording at");
            }
            return;
        }
        let Some(mp4) = &mut self.mp4 else { return };
        // The parameter sets are in hvcC, and only there.
        let units = unit.nals.iter().filter(|n| !nal_type(n).is_some_and(video::is_parameter_set));
        if let Err(e) = mp4.write_sample(unit.timestamp, units.map(Vec::as_slice), unit.is_keyframe()) {
            println!("camera: writing the recording: {e}; it stops here");
            self.mp4 = None;
        }
    }

    /// Starts the MP4 at a keyframe, if this is one and the parameter sets
    /// have come.
    fn start(&mut self, unit: &AccessUnit) -> bool {
        let [Some(vps), Some(sps), Some(pps)] = &self.parameters else { return false };
        if !unit.is_keyframe() {
            return false;
        }
        let Some(file) = self.file.take() else { return false };
        match Mp4Writer::new(BufWriter::new(file), video::CLOCK_RATE, vps, sps, pps) {
            Ok(mp4) => {
                if self.skipped > 0 {
                    println!("camera: recording from here, {} pictures in", self.skipped);
                }
                self.mp4 = Some(mp4);
                true
            }
            Err(e) => {
                println!("camera: can't start the recording: {e}");
                false
            }
        }
    }

    pub fn summary(&self) -> String {
        let pictures = self.mp4.as_ref().map_or(0, Mp4Writer::samples);
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
        let Some(mp4) = self.mp4.take() else {
            if self.file.is_some() {
                println!("camera: no picture to start at arrived; {} is empty", path.display());
            }
            return;
        };
        let (pictures, seconds) = (mp4.samples(), mp4.seconds());
        match mp4.finish() {
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
        let mut camera = Recording::to(None, None);
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
    /// an MP4: `KOALA_CAMERA_DUMP=trial.rtp cargo test -- --ignored`, then
    /// check `trial.rtp.mp4` with ffprobe or a player.
    #[test]
    #[ignore = "needs a trial's packets"]
    fn a_trials_packets_make_an_mp4() {
        let Some(dump) = std::env::var_os("KOALA_CAMERA_DUMP").map(PathBuf::from) else { return };
        let data = std::fs::read(&dump).unwrap();
        let mut out = dump.into_os_string();
        out.push(".mp4");
        let out = PathBuf::from(out);
        let mut camera = Recording::to(Some(out.clone()), Some(File::create(&out).unwrap()));
        let mut rest = &data[..];
        while let [high, low, tail @ ..] = rest {
            let len = usize::from(u16::from_be_bytes([*high, *low]));
            camera.packet(&tail[..len]);
            rest = &tail[len..];
        }
        assert!(camera.mp4.as_ref().is_some_and(|m| m.samples() > 0));
        drop(camera);
        assert!(std::fs::metadata(&out).unwrap().len() > 0);
    }
}
