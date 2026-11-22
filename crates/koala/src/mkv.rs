//! The camera's recordings, written by FFmpeg's Matroska muxer (libavformat,
//! through ffmpeg-next): one HEVC track. FFmpeg builds its CodecPrivate
//! (`hvcC`) from the stream's parameter sets, given as Annex B extradata,
//! and takes the pictures in Annex B too, writing them length-prefixed.
//!
//! Matroska, not MP4 (Parker, 2026-11-20): a Matroska file is readable up
//! to wherever it stops, where an MP4 is nothing without the index it gets
//! at the end. Trial 24's MP4, cut off by a Ctrl+C, had 87 s of pictures and
//! no index. The muxer keeps a cluster in memory until it closes (every 5 s
//! by default), so each picture closes its own and goes to disk: a recording
//! that's killed keeps all but the picture in flight, for a cluster header
//! of about 20 bytes a picture.

use std::path::Path;
use std::ptr;

use ffmpeg::{Dictionary, Packet, Rational, codec, encoder, format, media};
use ffmpeg_next as ffmpeg;
use koala_capture::video::{AccessUnit, Sps, annex_b};

/// A Matroska file being written.
pub struct MkvWriter {
    output: format::context::Output,
    /// The RTP timestamps' rate, and the stream's time base after the
    /// header (Matroska's is a millisecond).
    timescale: i32,
    time_base: Rational,
    last: Option<(u32, i64)>,
    /// The last step between pictures, for the last picture's length.
    step: i64,
    samples: usize,
}

impl MkvWriter {
    /// Starts a Matroska file at `path` for a stream with these parameter
    /// sets (NAL units, without start codes) whose timestamps run at
    /// `timescale`.
    pub fn create(path: &Path, timescale: u32, vps: &[u8], sps: &[u8], pps: &[u8]) -> Result<Self, String> {
        ffmpeg::init().map_err(|e| e.to_string())?;
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
        let info = Sps::parse(sps).ok_or("an SPS Koala can't read")?;
        let timescale = i32::try_from(timescale).map_err(|_| "a timescale too large")?;
        let mut output = format::output_as(path, "matroska").map_err(|e| format!("{}: {e}", path.display()))?;
        let mut parameters = codec::Parameters::new();
        parameters.set_medium(media::Type::Video);
        parameters.set_id(codec::Id::HEVC);
        let extradata = annex_b([vps, sps, pps]);
        // SAFETY: `parameters` owns its AVCodecParameters, which frees the
        // extradata with av_free; FFmpeg wants it from av_malloc, padded.
        unsafe {
            let p = parameters.as_mut_ptr();
            (*p).width = info.width as i32;
            (*p).height = info.height as i32;
            let padded = extradata.len() + ffmpeg::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
            let buffer = ffmpeg::ffi::av_mallocz(padded) as *mut u8;
            if buffer.is_null() {
                return Err("out of memory".into());
            }
            ptr::copy_nonoverlapping(extradata.as_ptr(), buffer, extradata.len());
            (*p).extradata = buffer;
            (*p).extradata_size = extradata.len() as i32;
        }
        let mut stream = output.add_stream(encoder::find(codec::Id::None)).map_err(|e| e.to_string())?;
        stream.set_time_base(Rational(1, timescale));
        stream.set_parameters(parameters);
        // Each write to disk at once, not left in FFmpeg's I/O buffer.
        let mut options = Dictionary::new();
        options.set("flush_packets", "1");
        output.write_header_with(options).map_err(|e| format!("the Matroska header: {e}"))?;
        let time_base = output.stream(0).ok_or("no stream")?.time_base();
        Ok(Self { output, timescale, time_base, last: None, step: 0, samples: 0 })
    }

    /// One picture: its RTP timestamp, its NAL units, and whether a decoder
    /// can start at it.
    pub fn write_sample<'a>(
        &mut self,
        timestamp: u32,
        units: impl IntoIterator<Item = &'a [u8]>,
        keyframe: bool,
    ) -> Result<(), String> {
        let time = match self.last {
            Some((last, at)) => {
                // A timestamp behind the last, or the same, gets the next
                // tick: timestamps only go forward.
                let delta = Some(timestamp.wrapping_sub(last)).filter(|d| *d < 1 << 31).unwrap_or(0);
                at + i64::from(delta).max(1)
            }
            None => 0,
        };
        if let Some((_, at)) = self.last {
            self.step = time - at;
        }
        let mut packet = Packet::copy(&annex_b(units));
        packet.set_stream(0);
        packet.set_pts(Some(time));
        packet.set_dts(Some(time));
        if keyframe {
            packet.set_flags(codec::packet::Flags::KEY);
        }
        packet.rescale_ts(Rational(1, self.timescale), self.time_base);
        let picture = self.samples + 1;
        packet.write(&mut self.output).map_err(|e| format!("picture {picture}: {e}"))?;
        // A flush (no packet) closes the cluster, which the muxer then writes.
        // SAFETY: the context is open, its header written.
        let flushed = unsafe { ffmpeg::ffi::av_write_frame(self.output.as_mut_ptr(), ptr::null_mut()) };
        if flushed < 0 {
            return Err(format!("picture {picture}: {}", ffmpeg::Error::from(flushed)));
        }
        self.last = Some((timestamp, time));
        self.samples += 1;
        Ok(())
    }

    /// How many pictures are written.
    pub fn samples(&self) -> usize {
        self.samples
    }

    /// The recording's length, in seconds: to the last picture's end.
    pub fn seconds(&self) -> f64 {
        let step = if self.step > 0 { self.step } else { i64::from(self.timescale / 30) };
        self.last.map_or(0, |(_, at)| at + step) as f64 / f64::from(self.timescale)
    }

    /// Ends the file: its index (cues) and duration, which a player can do
    /// without.
    pub fn finish(mut self) -> Result<(), String> {
        self.output.write_trailer().map_err(|e| format!("the Matroska index: {e}"))
    }
}

/// A recording's pictures, read back by FFmpeg's demuxer: each sample's NAL
/// units (length-prefixed in Matroska), the parameter sets of its `hvcC`
/// ahead of the first, its time on the camera's RTP clock
/// (`koala_capture::video::CLOCK_RATE`).
pub fn read(path: &Path) -> Result<Vec<AccessUnit>, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = format::input(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let stream = input.stream(0).ok_or("no stream")?;
    let time_base = stream.time_base();
    // SAFETY: the stream's parameters, which it owns, while it's open.
    let hvcc = unsafe {
        let p = stream.parameters().as_ptr();
        if (*p).extradata.is_null() {
            return Err("no hvcC".into());
        }
        std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize).to_vec()
    };
    // ISO/IEC 14496-15's HEVCDecoderConfigurationRecord: 22 bytes, the
    // NAL unit length's size in the last's low bits, then arrays of units.
    let short = || "an hvcC cut short".to_string();
    let length_size = usize::from(hvcc.get(21).ok_or_else(short)? & 3) + 1;
    let mut parameter_sets = Vec::new();
    let mut rest = hvcc.get(23..).ok_or_else(short)?;
    for _ in 0..hvcc[22] {
        let [_, high, low, tail @ ..] = rest else { return Err(short()) };
        rest = tail;
        for _ in 0..u16::from_be_bytes([*high, *low]) {
            let [high, low, tail @ ..] = rest else { return Err(short()) };
            let len = usize::from(u16::from_be_bytes([*high, *low]));
            parameter_sets.push(tail.get(..len).ok_or_else(short)?.to_vec());
            rest = &tail[len..];
        }
    }
    let mut units = Vec::new();
    for (_, packet) in input.packets() {
        let mut nals = if units.is_empty() { parameter_sets.clone() } else { Vec::new() };
        let mut data = packet.data().unwrap_or_default();
        while data.len() > length_size {
            let len = data[..length_size].iter().fold(0usize, |n, b| (n << 8) | usize::from(*b));
            let Some(nal) = data.get(length_size..length_size + len) else { break };
            nals.push(nal.to_vec());
            data = &data[length_size + len..];
        }
        let pts = packet.pts().unwrap_or(0);
        let timestamp = pts * i64::from(time_base.numerator()) * i64::from(koala_capture::video::CLOCK_RATE)
            / i64::from(time_base.denominator());
        units.push(AccessUnit { timestamp: timestamp as u32, nals });
    }
    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// Trial 23's parameter sets (`koala_capture::video`'s tests).
    const VPS: &str = "40010c01ffff016000000300b000000300000300781b0240";
    const SPS: &str = "420101016000000300b00000030000030078a003c0801107cb881b9245232e7f13f0bfa1bf5220";
    const PPS: &str = "4401c0724c16c9";

    /// The NAL unit types in a length-prefixed sample.
    fn types(mut sample: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while sample.len() >= 4 {
            let len = u32::from_be_bytes(sample[..4].try_into().unwrap()) as usize;
            out.push((sample[4] >> 1) & 0x3f);
            sample = &sample[4 + len..];
        }
        out
    }

    /// Three pictures written to `name` in the temporary directory, the
    /// recording finished or not.
    fn record(name: &str, finish: bool) -> std::path::PathBuf {
        let (vps, sps, pps) = (hex(VPS), hex(SPS), hex(PPS));
        let path = std::env::temp_dir().join(format!("koala-{name}-{}.mkv", std::process::id()));
        let mut w = MkvWriter::create(&path, 24_000, &vps, &sps, &pps).unwrap();
        let idr: &[u8] = &[20 << 1, 1, 0xaa];
        let sei: &[u8] = &[39 << 1, 1, 0x05];
        let trail: &[u8] = &[1 << 1, 1, 0xbb, 0xcc];
        w.write_sample(1000, [sei, idr], true).unwrap();
        w.write_sample(1800, [trail], false).unwrap();
        w.write_sample(2600, [trail], false).unwrap();
        assert_eq!(w.samples(), 3);
        assert!((w.seconds() - 0.1).abs() < 1e-9, "3 pictures of 800/24000 s");
        if finish {
            w.finish().unwrap();
        }
        path
    }

    /// The pictures FFmpeg's own demuxer reads back: their NAL unit types,
    /// times in milliseconds, and keyframe flags.
    fn read_back(path: &std::path::Path) -> Vec<(Vec<u8>, i64, bool)> {
        let mut input = format::input(&path).unwrap();
        let stream = input.stream(0).unwrap();
        let (width, height, id) = unsafe {
            let p = stream.parameters().as_ptr();
            ((*p).width, (*p).height, (*p).codec_id)
        };
        assert_eq!((width, height, id), (1920, 1080, ffmpeg::ffi::AVCodecID::AV_CODEC_ID_HEVC));
        let time_base = stream.time_base();
        input
            .packets()
            .map(|(_, p)| {
                let ms =
                    p.pts().unwrap() * i64::from(time_base.numerator()) * 1000 / i64::from(time_base.denominator());
                (types(p.data().unwrap()), ms, p.is_key())
            })
            .collect()
    }

    #[test]
    fn ffmpeg_writes_an_hevc_track_with_the_pictures_timing() {
        let path = record("mkv-test", true);
        assert_eq!(read_back(&path), [(vec![39, 20], 0, true), (vec![1], 33, false), (vec![1], 67, false)]);
        let _ = std::fs::remove_file(&path);
    }

    /// A recording never finished, as when Koala is killed, keeps its
    /// pictures.
    #[test]
    fn an_unfinished_recording_still_reads() {
        let path = record("mkv-cut", false);
        assert_eq!(read_back(&path).len(), 3);
        let _ = std::fs::remove_file(&path);
    }
}
