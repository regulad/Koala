//! The camera's recordings, written by FFmpeg's MP4 muxer (libavformat's
//! `mov`, through ffmpeg-next): one HEVC track, `hvc1`. FFmpeg builds its
//! `hvcC` from the stream's parameter sets, given as Annex B extradata, and
//! takes the pictures in Annex B too, writing them length-prefixed without
//! their parameter sets, as `hvc1` wants. The index (`moov`) is written when
//! the recording ends; a file that wasn't finished doesn't play.

use std::path::Path;
use std::ptr;

use ffmpeg::{Packet, Rational, codec, encoder, format, media};
use ffmpeg_next as ffmpeg;
use koala_capture::video::Sps;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// A picture held back until the next one gives its duration: its Annex B
/// bytes, time (in the stream's timescale, from the first) and whether a
/// decoder can start at it.
struct Held {
    data: Vec<u8>,
    time: i64,
    keyframe: bool,
}

/// An MP4 being written.
pub struct Mp4Writer {
    output: format::context::Output,
    /// The RTP timestamps' rate, and the stream's time base after the
    /// header (the muxer may choose another).
    timescale: i32,
    time_base: Rational,
    held: Option<Held>,
    last_timestamp: Option<u32>,
    /// The last duration written, for the last picture's.
    step: i64,
    samples: usize,
}

impl Mp4Writer {
    /// Starts an MP4 at `path` for a stream with these parameter sets (NAL
    /// units, without start codes) whose timestamps run at `timescale`.
    pub fn create(path: &Path, timescale: u32, vps: &[u8], sps: &[u8], pps: &[u8]) -> Result<Self, String> {
        ffmpeg::init().map_err(|e| e.to_string())?;
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
        let info = Sps::parse(sps).ok_or("an SPS Koala can't read")?;
        let timescale = i32::try_from(timescale).map_err(|_| "a timescale too large")?;
        let mut output = format::output_as(path, "mp4").map_err(|e| format!("{}: {e}", path.display()))?;
        let mut parameters = codec::Parameters::new();
        parameters.set_medium(media::Type::Video);
        parameters.set_id(codec::Id::HEVC);
        let extradata: Vec<u8> =
            [vps, sps, pps].iter().flat_map(|unit| START_CODE.iter().chain(unit.iter())).copied().collect();
        // SAFETY: `parameters` owns its AVCodecParameters, which frees the
        // extradata with av_free; FFmpeg wants it from av_malloc, padded.
        unsafe {
            let p = parameters.as_mut_ptr();
            (*p).width = info.width as i32;
            (*p).height = info.height as i32;
            (*p).codec_tag = u32::from_le_bytes(*b"hvc1");
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
        output.write_header().map_err(|e| format!("the MP4's header: {e}"))?;
        let time_base = output.stream(0).ok_or("no stream")?.time_base();
        Ok(Self { output, timescale, time_base, held: None, last_timestamp: None, step: 0, samples: 0 })
    }

    /// One picture: its RTP timestamp, its NAL units, and whether a decoder
    /// can start at it.
    pub fn write_sample<'a>(
        &mut self,
        timestamp: u32,
        units: impl IntoIterator<Item = &'a [u8]>,
        keyframe: bool,
    ) -> Result<(), String> {
        let time = match (&self.held, self.last_timestamp) {
            (Some(held), Some(last)) => {
                // A timestamp behind the last, or the same, gets the next
                // tick: the muxer takes only increasing ones.
                let delta = Some(timestamp.wrapping_sub(last)).filter(|d| *d < 1 << 31).unwrap_or(0);
                held.time + i64::from(delta).max(1)
            }
            _ => 0,
        };
        let data = units.into_iter().flat_map(|unit| START_CODE.iter().chain(unit.iter())).copied().collect();
        if let Some(held) = self.held.take() {
            self.step = time - held.time;
            self.write(held, self.step)?;
        }
        self.held = Some(Held { data, time, keyframe });
        self.last_timestamp = Some(timestamp);
        Ok(())
    }

    /// How many pictures are written.
    pub fn samples(&self) -> usize {
        self.samples + usize::from(self.held.is_some())
    }

    /// The recording's length, in seconds.
    pub fn seconds(&self) -> f64 {
        let end = self.held.as_ref().map_or(0, |h| h.time + self.last_step());
        end as f64 / f64::from(self.timescale)
    }

    /// Ends the file: its last picture, then its index.
    pub fn finish(mut self) -> Result<(), String> {
        if let Some(held) = self.held.take() {
            let step = self.last_step();
            self.write(held, step)?;
        }
        self.output.write_trailer().map_err(|e| format!("the MP4's index: {e}"))
    }

    /// The last picture's duration: the one before's, or a 30th of a
    /// second, alone.
    fn last_step(&self) -> i64 {
        if self.step > 0 { self.step } else { i64::from(self.timescale / 30) }
    }

    fn write(&mut self, held: Held, duration: i64) -> Result<(), String> {
        let mut packet = Packet::copy(&held.data);
        packet.set_stream(0);
        packet.set_pts(Some(held.time));
        packet.set_dts(Some(held.time));
        packet.set_duration(duration);
        if held.keyframe {
            packet.set_flags(codec::packet::Flags::KEY);
        }
        packet.rescale_ts(Rational(1, self.timescale), self.time_base);
        packet.write_interleaved(&mut self.output).map_err(|e| format!("picture {}: {e}", self.samples + 1))?;
        self.samples += 1;
        Ok(())
    }
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

    #[test]
    fn ffmpeg_writes_an_hvc1_track_with_the_pictures_timing() {
        let (vps, sps, pps) = (hex(VPS), hex(SPS), hex(PPS));
        let path = std::env::temp_dir().join(format!("koala-mp4-test-{}.mp4", std::process::id()));
        let mut w = Mp4Writer::create(&path, 24_000, &vps, &sps, &pps).unwrap();
        let idr: &[u8] = &[20 << 1, 1, 0xaa];
        let sei: &[u8] = &[39 << 1, 1, 0x05];
        let trail: &[u8] = &[1 << 1, 1, 0xbb, 0xcc];
        // The first picture with its parameter sets in band, as the iPhone
        // sends them: they're left out of the sample.
        w.write_sample(1000, [vps.as_slice(), &sps, &pps, sei, idr], true).unwrap();
        w.write_sample(1800, [trail], false).unwrap();
        w.write_sample(2600, [trail], false).unwrap();
        assert_eq!(w.samples(), 3);
        assert!((w.seconds() - 0.1).abs() < 1e-9, "3 pictures of 800/24000 s");
        w.finish().unwrap();

        // Read back with FFmpeg's own demuxer.
        let mut input = format::input(&path).unwrap();
        let stream = input.stream(0).unwrap();
        let (tag, width, height, id) = unsafe {
            let p = stream.parameters().as_ptr();
            ((*p).codec_tag.to_le_bytes(), (*p).width, (*p).height, (*p).codec_id)
        };
        assert_eq!((&tag, width, height), (b"hvc1", 1920, 1080));
        assert_eq!(id, ffmpeg::ffi::AVCodecID::AV_CODEC_ID_HEVC);
        let time_base = stream.time_base();
        let packets: Vec<(Vec<u8>, i64, bool)> = input
            .packets()
            .map(|(_, p)| {
                let at =
                    p.pts().unwrap() * i64::from(time_base.numerator()) * 24_000 / i64::from(time_base.denominator());
                (types(p.data().unwrap()), at, p.is_key())
            })
            .collect();
        assert_eq!(packets, [(vec![39, 20], 0, true), (vec![1], 800, false), (vec![1], 1600, false)]);
        drop(input);
        let _ = std::fs::remove_file(&path);
    }
}
