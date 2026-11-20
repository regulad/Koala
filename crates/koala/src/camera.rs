//! The camera's video, for trials: `koala guest --camera PATH` asks the
//! iPhone for its camera (entity 1) in place of its microphone, and records
//! what arrives, to be worked out offline as the microphone's payloads were
//! before its decoder.
//!
//! - `PATH`: every authenticated RTP packet of the camera's stream,
//!   decrypted, whole (header and payload), each after its length (u16,
//!   big-endian), in arrival order.
//! - `PATH.offer.bplist`: the iPhone's AVC offer, as it sent it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Where `--camera` records the video, if anywhere: also what makes the
/// session the camera's.
pub static RECORD: OnceLock<PathBuf> = OnceLock::new();

/// The camera's packets, written down (or, by default, only counted).
#[derive(Default)]
pub struct VideoDump {
    file: Option<BufWriter<File>>,
    pub packets: u64,
    pub bytes: u64,
    /// RTP marker bits: one a frame, in most packetizations.
    pub markers: u64,
    payload_types: BTreeMap<u8, u64>,
}

impl VideoDump {
    /// Records to `RECORD`, or counts only.
    pub fn open() -> Self {
        let file = RECORD.get().and_then(|path| match File::create(path) {
            Ok(f) => {
                println!("recording the camera's packets to {}", path.display());
                Some(BufWriter::new(f))
            }
            Err(e) => {
                println!("can't record the camera to {}: {e}", path.display());
                None
            }
        });
        Self { file, packets: 0, bytes: 0, markers: 0, payload_types: BTreeMap::new() }
    }

    /// One authenticated, decrypted RTP packet, whole.
    pub fn packet(&mut self, rtp: &[u8]) {
        if rtp.len() < 12 {
            return;
        }
        self.packets += 1;
        self.bytes += rtp.len() as u64;
        self.markers += u64::from(rtp[1] >> 7);
        *self.payload_types.entry(rtp[1] & 0x7f).or_default() += 1;
        if let Some(file) = &mut self.file {
            let len = u16::try_from(rtp.len()).unwrap_or(u16::MAX);
            let _ = file.write_all(&len.to_be_bytes()).and_then(|_| file.write_all(&rtp[..usize::from(len)]));
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "video: {} packets, {} KB, {} markers, payload types {:?}",
            self.packets,
            self.bytes / 1024,
            self.markers,
            self.payload_types
        )
    }
}

impl Drop for VideoDump {
    fn drop(&mut self) {
        if let Some(file) = &mut self.file {
            let _ = file.flush();
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
    fn packets_are_counted_by_type_and_marker() {
        let mut dump = VideoDump::default();
        let mut rtp = [0u8; 20];
        rtp[0] = 0x80;
        rtp[1] = 0x80 | 100;
        dump.packet(&rtp);
        rtp[1] = 100;
        dump.packet(&rtp);
        dump.packet(&[0x80; 5]);
        assert_eq!((dump.packets, dump.markers, dump.bytes), (2, 1, 40));
        assert!(dump.summary().contains("{100: 2}"));
    }
}
