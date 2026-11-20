//! The microphone's audio, live: the iPhone's Opus channels
//! (`koala_capture::audio`) decoded as their packets arrive, with lost
//! packets concealed, 10 ms of every channel at a time handed to a sink.
//!
//! The channels are the iPhone's separate microphones (Parker, listening
//! to trial 22's recording). Which reaches Windows, and through which
//! driver, is open (`docs/guest-pairing.md`); for now a sink can record
//! them to a WAV (`koala guest --mic-wav PATH`).

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use koala_capture::audio::{CHANNELS, FRAME_SAMPLES, SAMPLE_RATE, channel_frames};

/// Where `--mic-wav` records the microphone, if anywhere.
pub static RECORD: OnceLock<PathBuf> = OnceLock::new();

/// The most lost packets concealed in one gap: 100 ms. A longer gap is
/// skipped over.
const MAX_CONCEAL: u16 = 10;

/// 10 ms of each channel.
pub type Block = [[i16; FRAME_SAMPLES]; CHANNELS];

/// Where decoded audio goes.
pub trait Sink: Send {
    fn write(&mut self, block: &Block);
}

/// Decodes and drops: the stream is still checked.
pub struct Discard;

impl Sink for Discard {
    fn write(&mut self, _: &Block) {}
}

/// A 16-bit PCM WAV with every channel, its sizes written as it closes.
pub struct Wav {
    file: BufWriter<File>,
    samples: u32,
}

impl Wav {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(&wav_header(0))?;
        Ok(Self { file, samples: 0 })
    }
}

impl Sink for Wav {
    fn write(&mut self, block: &Block) {
        let mut out = Vec::with_capacity(FRAME_SAMPLES * CHANNELS * 2);
        for s in 0..FRAME_SAMPLES {
            for channel in block {
                out.extend_from_slice(&channel[s].to_le_bytes());
            }
        }
        if self.file.write_all(&out).is_ok() {
            self.samples += FRAME_SAMPLES as u32;
        }
    }
}

impl Drop for Wav {
    fn drop(&mut self) {
        let data = self.samples * (CHANNELS as u32) * 2;
        let _ = self.file.seek(SeekFrom::Start(0)).and_then(|_| self.file.write_all(&wav_header(data)));
        let _ = self.file.flush();
    }
}

fn wav_header(data_len: u32) -> [u8; 44] {
    let channels = CHANNELS as u16;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(SAMPLE_RATE * u32::from(channels) * 2).to_le_bytes());
    h[32..34].copy_from_slice(&(channels * 2).to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// The microphone stream's decoder.
pub struct Microphone {
    decoders: Vec<opus::Decoder>,
    last_seq: Option<u16>,
    sink: Box<dyn Sink>,
    /// Packets decoded, packets concealed, and frames that wouldn't decode.
    pub decoded: u64,
    pub concealed: u64,
    pub errors: u64,
}

impl Microphone {
    pub fn new(sink: Box<dyn Sink>) -> Result<Self, String> {
        let decoders = (0..CHANNELS)
            .map(|_| opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono).map_err(|e| format!("Opus: {e}")))
            .collect::<Result<_, _>>()?;
        Ok(Self { decoders, last_seq: None, sink, decoded: 0, concealed: 0, errors: 0 })
    }

    /// The sink `--mic-wav` asks for, or [`Discard`].
    pub fn sink() -> Box<dyn Sink> {
        match RECORD.get() {
            Some(path) => match Wav::create(path) {
                Ok(w) => {
                    println!("recording the microphone to {}", path.display());
                    Box::new(w)
                }
                Err(e) => {
                    println!("can't record the microphone to {}: {e}", path.display());
                    Box::new(Discard)
                }
            },
            None => Box::new(Discard),
        }
    }

    /// Takes one authenticated packet's sequence number and payload. A late
    /// or repeated packet is dropped; packets missing before it are
    /// concealed, up to [`MAX_CONCEAL`].
    pub fn packet(&mut self, seq: u16, payload: &[u8]) {
        if let Some(last) = self.last_seq {
            let ahead = seq.wrapping_sub(last);
            if ahead == 0 || ahead >= 0x8000 {
                return;
            }
            for _ in 0..(ahead - 1).min(MAX_CONCEAL) {
                self.conceal();
            }
        }
        self.last_seq = Some(seq);
        let frames = match channel_frames(payload) {
            Ok(f) if f.len() == CHANNELS => f,
            _ => {
                self.errors += 1;
                return self.conceal();
            }
        };
        let mut block = [[0i16; FRAME_SAMPLES]; CHANNELS];
        for (c, frame) in frames.iter().enumerate() {
            let ok = matches!(self.decoders[c].decode(frame, &mut block[c], false), Ok(FRAME_SAMPLES));
            if !ok {
                self.errors += 1;
                let _ = self.decoders[c].decode(&[], &mut block[c], false);
            }
        }
        self.decoded += 1;
        self.sink.write(&block);
    }

    /// 10 ms the decoders make up for a lost packet.
    fn conceal(&mut self) {
        let mut block = [[0i16; FRAME_SAMPLES]; CHANNELS];
        for (c, d) in self.decoders.iter_mut().enumerate() {
            let _ = d.decode(&[], &mut block[c], false);
        }
        self.concealed += 1;
        self.sink.write(&block);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Counts what reaches it.
    struct Count(Arc<Mutex<usize>>);

    impl Sink for Count {
        fn write(&mut self, _: &Block) {
            *self.0.lock().unwrap() += 1;
        }
    }

    /// The opening silence on every channel, as trial 22 began.
    const SILENCE: [u8; 20] =
        [0, 3, 0xf0, 0xff, 0xfe, 0, 3, 0xf0, 0xff, 0xfe, 0, 3, 0xf0, 0xff, 0xfe, 0, 3, 0xf0, 0xff, 0xfe];

    #[test]
    fn packets_decode_and_gaps_are_concealed() {
        let blocks = Arc::new(Mutex::new(0));
        let mut mic = Microphone::new(Box::new(Count(blocks.clone()))).unwrap();
        mic.packet(100, &SILENCE);
        mic.packet(101, &SILENCE);
        // 102 and 103 lost.
        mic.packet(104, &SILENCE);
        // Late, and repeated: dropped.
        mic.packet(103, &SILENCE);
        mic.packet(104, &SILENCE);
        assert_eq!((mic.decoded, mic.concealed, mic.errors), (3, 2, 0));
        assert_eq!(*blocks.lock().unwrap(), 5, "10 ms for every packet, received or concealed");
        // A payload that doesn't split is concealed too.
        mic.packet(105, &[0, 9, 1]);
        assert_eq!((mic.concealed, mic.errors), (3, 1));
    }

    #[test]
    fn a_recording_is_a_whole_wav() {
        let path = std::env::temp_dir().join(format!("koala-mic-test-{}.wav", std::process::id()));
        {
            let mut mic = Microphone::new(Box::new(Wav::create(&path).unwrap())).unwrap();
            for seq in 0..3 {
                mic.packet(seq, &SILENCE);
            }
        }
        let wav = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let data = 3 * FRAME_SAMPLES * CHANNELS * 2;
        assert_eq!(wav.len(), 44 + data);
        assert_eq!(&wav[40..44], &(data as u32).to_le_bytes());
        assert_eq!(&wav[22..24], &4u16.to_le_bytes());
    }
}
