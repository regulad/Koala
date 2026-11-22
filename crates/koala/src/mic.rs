//! The microphone's audio, live: the iPhone's Opus channels
//! (`koala_capture::audio`) decoded as their packets arrive, with lost
//! packets concealed.
//!
//! Each channel is one of the iPhone's microphones
//! (`koala_capture::mic_map`), and each goes out as a microphone of its own:
//! a session with N channels opens N single-channel sinks, once its first
//! packet shows how many there are, named from the iPhone's model. A sink
//! gives its microphone to Windows ([`crate::usb_mics`]), records it to a
//! WAV (`koala guest --mic-wav PATH`), or both.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use koala_capture::audio::{FRAME_SAMPLES, SAMPLE_RATE, channel_frames};
use koala_capture::mic_map::{self, Layout, Mic};

/// Where `--mic-wav` records the microphones, if anywhere.
pub static RECORD: OnceLock<PathBuf> = OnceLock::new();

/// The most lost packets concealed in one gap: 100 ms. A longer gap is
/// skipped over.
const MAX_CONCEAL: u16 = 10;

/// 10 ms of one microphone.
pub type Block = [i16; FRAME_SAMPLES];

/// The iPhone's model, once its AVC offer has said: the session's Control
/// stream sets it, and the microphone reads it when its stream opens.
pub type Model = Arc<OnceLock<String>>;

/// One microphone's way out.
pub trait Sink: Send {
    fn write(&mut self, block: &Block);
}

/// Makes a session's sinks, one per microphone, in channel order.
pub trait Output: Send {
    fn open(&mut self, layout: &Layout) -> Vec<Box<dyn Sink>>;
}

/// Decodes and drops: the stream is still checked.
pub struct Discard;

impl Sink for Discard {
    fn write(&mut self, _: &Block) {}
}

impl Output for Discard {
    fn open(&mut self, layout: &Layout) -> Vec<Box<dyn Sink>> {
        layout.mics.iter().map(|_| Box::new(Discard) as Box<dyn Sink>).collect()
    }
}

/// Several outputs at once: each microphone's blocks go to each.
pub struct Tee(pub Vec<Box<dyn Output>>);

struct TeeSink(Vec<Box<dyn Sink>>);

impl Sink for TeeSink {
    fn write(&mut self, block: &Block) {
        for sink in &mut self.0 {
            sink.write(block);
        }
    }
}

impl Output for Tee {
    fn open(&mut self, layout: &Layout) -> Vec<Box<dyn Sink>> {
        let mut each: Vec<std::vec::IntoIter<Box<dyn Sink>>> =
            self.0.iter_mut().map(|o| o.open(layout).into_iter()).collect();
        layout
            .mics
            .iter()
            .map(|_| Box::new(TeeSink(each.iter_mut().filter_map(Iterator::next).collect())) as Box<dyn Sink>)
            .collect()
    }
}

/// Records each microphone to a WAV of its own: `PATH` with the
/// microphone's id after the file stem, so `trial.wav` gives
/// `trial-bottom-1.wav`, `trial-back.wav` and so on.
pub struct Recordings {
    pub path: PathBuf,
}

impl Recordings {
    pub fn path_for(&self, mic: &Mic) -> PathBuf {
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let extension = self.path.extension().map_or("wav".into(), |e| e.to_string_lossy().into_owned());
        self.path.with_file_name(format!("{stem}-{}.{extension}", mic.id))
    }
}

impl Output for Recordings {
    fn open(&mut self, layout: &Layout) -> Vec<Box<dyn Sink>> {
        layout
            .mics
            .iter()
            .map(|mic| {
                let path = self.path_for(mic);
                match Wav::create(&path) {
                    Ok(w) => {
                        println!("recording microphone {} ({}) to {}", mic.channel, mic.name, path.display());
                        Box::new(w) as Box<dyn Sink>
                    }
                    Err(e) => {
                        println!("can't record microphone {} to {}: {e}", mic.channel, path.display());
                        Box::new(Discard)
                    }
                }
            })
            .collect()
    }
}

/// A mono 16-bit PCM WAV, its sizes written as it closes.
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
        let bytes: Vec<u8> = block.iter().flat_map(|s| s.to_le_bytes()).collect();
        if self.file.write_all(&bytes).is_ok() {
            self.samples += FRAME_SAMPLES as u32;
        }
    }
}

impl Drop for Wav {
    fn drop(&mut self) {
        let _ = self.file.seek(SeekFrom::Start(0)).and_then(|_| self.file.write_all(&wav_header(self.samples * 2)));
        let _ = self.file.flush();
    }
}

fn wav_header(data_len: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// The microphones' decoders and sinks, from the stream's first packet.
struct Open {
    decoders: Vec<opus::Decoder>,
    sinks: Vec<Box<dyn Sink>>,
}

/// The microphone stream's decoder.
pub struct Microphone {
    output: Box<dyn Output>,
    model: Model,
    open: Option<Open>,
    last_seq: Option<u16>,
    /// The stream's microphones, once its first packet has arrived.
    pub layout: Option<Layout>,
    /// Packets decoded, packets concealed, and frames that wouldn't decode.
    pub decoded: u64,
    pub concealed: u64,
    pub errors: u64,
}

impl Microphone {
    pub fn new(output: Box<dyn Output>, model: Model) -> Self {
        Self { output, model, open: None, last_seq: None, layout: None, decoded: 0, concealed: 0, errors: 0 }
    }

    /// The session's outputs: Windows' microphones, named after `iphone`, if
    /// they're on ([`crate::usb_mics::ON`]); the WAVs `--mic-wav` asks for;
    /// or, with neither, [`Discard`].
    pub fn output(iphone: &str) -> Box<dyn Output> {
        let mut outputs: Vec<Box<dyn Output>> = Vec::new();
        if crate::usb_mics::ON.load(std::sync::atomic::Ordering::Relaxed) {
            outputs.push(Box::new(crate::usb_mics::UsbMicrophones { iphone: iphone.to_owned() }));
        }
        if let Some(path) = RECORD.get() {
            outputs.push(Box::new(Recordings { path: path.clone() }));
        }
        match outputs.len() {
            0 => Box::new(Discard),
            1 => outputs.remove(0),
            _ => Box::new(Tee(outputs)),
        }
    }

    /// The iPhone's packets come from a new source (SSRC), as after a pause:
    /// their sequence numbers start afresh, from anywhere.
    pub fn new_source(&mut self) {
        self.last_seq = None;
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
            Ok(f) if !f.is_empty() => f,
            _ => {
                self.errors += 1;
                return self.conceal();
            }
        };
        if self.open.is_none() && !self.start(frames.len()) {
            self.errors += 1;
            return;
        }
        let Some(open) = self.open.as_mut() else { return };
        if frames.len() != open.decoders.len() {
            self.errors += 1;
            return self.conceal();
        }
        for ((decoder, sink), frame) in open.decoders.iter_mut().zip(&mut open.sinks).zip(frames) {
            let mut block = [0i16; FRAME_SAMPLES];
            if !matches!(decoder.decode(frame, &mut block, false), Ok(FRAME_SAMPLES)) {
                self.errors += 1;
                let _ = decoder.decode(&[], &mut block, false);
            }
            sink.write(&block);
        }
        self.decoded += 1;
    }

    /// Opens the stream's `channels` microphones: names them from the
    /// iPhone's model, and makes a decoder and a sink for each.
    fn start(&mut self, channels: usize) -> bool {
        let layout = mic_map::layout(self.model.get().map(String::as_str), channels);
        println!("microphone: {channels} channels: {layout}");
        let decoders: Result<Vec<_>, _> =
            (0..channels).map(|_| opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)).collect();
        let decoders = match decoders {
            Ok(d) => d,
            Err(e) => {
                println!("microphone: no Opus decoder: {e}");
                return false;
            }
        };
        let sinks = self.output.open(&layout);
        self.open = Some(Open { decoders, sinks });
        self.layout = Some(layout);
        true
    }

    /// 10 ms the decoders make up for a lost packet.
    fn conceal(&mut self) {
        let Some(open) = self.open.as_mut() else { return };
        for (decoder, sink) in open.decoders.iter_mut().zip(&mut open.sinks) {
            let mut block = [0i16; FRAME_SAMPLES];
            let _ = decoder.decode(&[], &mut block, false);
            sink.write(&block);
        }
        self.concealed += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Counts the blocks each microphone gets.
    struct Count(Arc<Mutex<Vec<usize>>>);

    struct CountSink(Arc<Mutex<Vec<usize>>>, usize);

    impl Sink for CountSink {
        fn write(&mut self, _: &Block) {
            self.0.lock().unwrap()[self.1] += 1;
        }
    }

    impl Output for Count {
        fn open(&mut self, layout: &Layout) -> Vec<Box<dyn Sink>> {
            *self.0.lock().unwrap() = vec![0; layout.mics.len()];
            (0..layout.mics.len()).map(|c| Box::new(CountSink(self.0.clone(), c)) as Box<dyn Sink>).collect()
        }
    }

    /// Opus's silence frame on `n` channels, as trial 22 began.
    fn silence(n: usize) -> Vec<u8> {
        [0, 3, 0xf0, 0xff, 0xfe].repeat(n)
    }

    fn model(m: &str) -> Model {
        let model = Model::default();
        model.set(m.into()).unwrap();
        model
    }

    #[test]
    fn packets_decode_and_gaps_are_concealed() {
        let blocks = Arc::new(Mutex::new(Vec::new()));
        let mut mic = Microphone::new(Box::new(Count(blocks.clone())), model("iPhone18,1"));
        mic.packet(100, &silence(4));
        mic.packet(101, &silence(4));
        // 102 and 103 lost.
        mic.packet(104, &silence(4));
        // Late, and repeated: dropped.
        mic.packet(103, &silence(4));
        mic.packet(104, &silence(4));
        assert_eq!((mic.decoded, mic.concealed, mic.errors), (3, 2, 0));
        assert_eq!(*blocks.lock().unwrap(), [5; 4], "10 ms per microphone for every packet, received or concealed");
        // A payload that doesn't split, or has another channel count, is concealed.
        mic.packet(105, &[0, 9, 1]);
        mic.packet(106, &silence(3));
        assert_eq!((mic.concealed, mic.errors), (4, 2));
        // A new source's numbers, however far behind: taken.
        mic.new_source();
        mic.packet(7, &silence(4));
        mic.packet(8, &silence(4));
        assert_eq!((mic.decoded, mic.concealed), (5, 4));
        let ids: Vec<_> = mic.layout.unwrap().mics.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, ["bottom-1", "back", "front", "bottom-2"]);
    }

    #[test]
    fn the_stream_says_how_many_microphones() {
        let blocks = Arc::new(Mutex::new(Vec::new()));
        let mut mic = Microphone::new(Box::new(Count(blocks.clone())), Model::default());
        mic.packet(7, &silence(2));
        assert_eq!(*blocks.lock().unwrap(), [1, 1]);
        assert_eq!(mic.layout.unwrap().mics[1].name, "Mic 2");
    }

    #[test]
    fn each_microphone_records_to_a_wav_of_its_own() {
        let dir = std::env::temp_dir().join(format!("koala-mic-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = Recordings { path: dir.join("trial.wav") };
        {
            let mut mic = Microphone::new(Box::new(output), model("iPhone18,1"));
            for seq in 0..3 {
                mic.packet(seq, &silence(4));
            }
        }
        for id in ["bottom-1", "back", "front", "bottom-2"] {
            let wav = std::fs::read(dir.join(format!("trial-{id}.wav"))).unwrap();
            let data = 3 * FRAME_SAMPLES * 2;
            assert_eq!(wav.len(), 44 + data, "{id}");
            assert_eq!(&wav[40..44], &(data as u32).to_le_bytes());
            assert_eq!(&wav[22..24], &1u16.to_le_bytes(), "mono");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
