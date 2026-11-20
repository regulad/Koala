//! A minimal MP4 writer (ISO/IEC 14496-12) for the camera's recordings: one
//! HEVC video track, its sample entry `hvc1`, the stream's parameter sets in
//! `hvcC` (ISO/IEC 14496-15, 8.3.3) and nowhere else. Samples go to disk as
//! they come; the index (`moov`) follows them when the recording ends. A
//! file that wasn't finished has no index, and doesn't play.
//!
//! Not muxide (MIT/Apache, 0.5.0), which was considered: it keeps every
//! sample in memory until the end, and builds `hvcC`'s profile, tier and
//! level from the SPS's raw bytes, emulation prevention bytes included,
//! which gave the iPhone's stream level 0 (trial 23).

use std::io::{self, Seek, SeekFrom, Write};

use koala_capture::video::{Sps, nal};

/// The movie's own timescale (`mvhd`, `tkhd`): milliseconds.
const MOVIE_TIMESCALE: u32 = 1000;

/// An MP4 being written.
pub struct Mp4Writer<W: Write + Seek> {
    out: W,
    timescale: u32,
    width: u16,
    height: u16,
    hvcc: Vec<u8>,
    /// Where `mdat` starts, and its first sample.
    mdat_at: u64,
    data_at: u64,
    sizes: Vec<u32>,
    /// Each sample's decode time, in `timescale` units from the first.
    times: Vec<u64>,
    /// The samples a decoder can start at, numbered from 1.
    sync: Vec<u32>,
    last_timestamp: Option<u32>,
}

impl<W: Write + Seek> Mp4Writer<W> {
    /// Starts the file, `ftyp` and `mdat`'s header, for a stream with these
    /// parameter sets (NAL units, without start codes) whose timestamps run
    /// at `timescale`.
    pub fn new(mut out: W, timescale: u32, vps: &[u8], sps: &[u8], pps: &[u8]) -> io::Result<Self> {
        let info = Sps::parse(sps).ok_or_else(|| invalid("an SPS Koala can't read"))?;
        let width = u16::try_from(info.width).map_err(|_| invalid("too wide"))?;
        let height = u16::try_from(info.height).map_err(|_| invalid("too tall"))?;
        let hvcc = hvcc(&info, vps, sps, pps)?;
        let ftyp = boxed(b"ftyp", &[b"isom".as_slice(), &512u32.to_be_bytes(), b"isom", b"iso2", b"mp41"].concat());
        let start = out.stream_position()?;
        out.write_all(&ftyp)?;
        // `mdat` with a 64-bit size, filled in at the end.
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&0u64.to_be_bytes())?;
        let mdat_at = start + ftyp.len() as u64;
        Ok(Self {
            out,
            timescale,
            width,
            height,
            hvcc,
            mdat_at,
            data_at: mdat_at + 16,
            sizes: Vec::new(),
            times: Vec::new(),
            sync: Vec::new(),
            last_timestamp: None,
        })
    }

    /// One picture: its RTP timestamp, its NAL units (no parameter sets:
    /// `hvcC` has them all), and whether a decoder can start at it.
    pub fn write_sample<'a>(
        &mut self,
        timestamp: u32,
        units: impl IntoIterator<Item = &'a [u8]>,
        keyframe: bool,
    ) -> io::Result<()> {
        let time = match (self.last_timestamp, self.times.last()) {
            // A timestamp behind the last counts as the same time.
            (Some(last), Some(&t)) => {
                t + u64::from(Some(timestamp.wrapping_sub(last)).filter(|d| *d < 1 << 31).unwrap_or(0))
            }
            _ => 0,
        };
        let mut size = 0u32;
        for unit in units {
            let len = u32::try_from(unit.len()).map_err(|_| invalid("a NAL unit over 4 GB"))?;
            self.out.write_all(&len.to_be_bytes())?;
            self.out.write_all(unit)?;
            size = size.checked_add(4 + len).ok_or_else(|| invalid("a picture over 4 GB"))?;
        }
        self.sizes.push(size);
        self.times.push(time);
        if keyframe {
            self.sync.push(self.sizes.len() as u32);
        }
        self.last_timestamp = Some(timestamp);
        Ok(())
    }

    /// How many pictures are written.
    pub fn samples(&self) -> usize {
        self.sizes.len()
    }

    /// The recording's length, in seconds.
    pub fn seconds(&self) -> f64 {
        self.durations().iter().sum::<u64>() as f64 / f64::from(self.timescale)
    }

    /// Ends the file: `mdat`'s size, then `moov`. Gives the writer back.
    pub fn finish(mut self) -> io::Result<W> {
        let end = self.out.stream_position()?;
        self.out.seek(SeekFrom::Start(self.mdat_at + 8))?;
        self.out.write_all(&(end - self.mdat_at).to_be_bytes())?;
        self.out.seek(SeekFrom::Start(end))?;
        let moov = self.moov()?;
        self.out.write_all(&moov)?;
        self.out.flush()?;
        Ok(self.out)
    }

    /// Each sample's duration: to the next one's time, and for the last, the
    /// one before's (or a 30th of a second, alone).
    fn durations(&self) -> Vec<u64> {
        let mut d: Vec<u64> = self.times.windows(2).map(|w| w[1] - w[0]).collect();
        if !self.times.is_empty() {
            d.push(d.last().copied().unwrap_or(u64::from(self.timescale / 30)));
        }
        d
    }

    fn moov(&self) -> io::Result<Vec<u8>> {
        let durations = self.durations();
        let media_duration: u64 = durations.iter().sum();
        let duration = u32::try_from(media_duration * u64::from(MOVIE_TIMESCALE) / u64::from(self.timescale))
            .map_err(|_| invalid("over 49 days"))?;
        let media_duration = u32::try_from(media_duration).map_err(|_| invalid("too long"))?;
        let count = self.sizes.len() as u32;

        let mvhd = full(
            b"mvhd",
            0,
            0,
            &[
                &[0u8; 8][..], // creation and modification times
                &MOVIE_TIMESCALE.to_be_bytes(),
                &duration.to_be_bytes(),
                &0x0001_0000u32.to_be_bytes(), // rate 1.0
                &0x0100u16.to_be_bytes(),      // volume 1.0
                &[0u8; 10],
                &MATRIX,
                &[0u8; 24],
                &2u32.to_be_bytes(), // next track ID
            ]
            .concat(),
        );
        let tkhd = full(
            b"tkhd",
            0,
            0x3, // enabled, in the movie
            &[
                &[0u8; 8][..],
                &1u32.to_be_bytes(), // track ID
                &[0u8; 4],
                &duration.to_be_bytes(),
                &[0u8; 16], // reserved, layer, alternate group, volume, reserved
                &MATRIX,
                &(u32::from(self.width) << 16).to_be_bytes(),
                &(u32::from(self.height) << 16).to_be_bytes(),
            ]
            .concat(),
        );
        let mdhd = full(
            b"mdhd",
            0,
            0,
            &[
                &[0u8; 8][..],
                &self.timescale.to_be_bytes(),
                &media_duration.to_be_bytes(),
                &0x55c4u16.to_be_bytes(), // language "und"
                &[0u8; 2],
            ]
            .concat(),
        );
        let hdlr = full(b"hdlr", 0, 0, &[&[0u8; 4][..], b"vide", &[0u8; 12], b"VideoHandler\0"].concat());
        let vmhd = full(b"vmhd", 0, 1, &[0u8; 8]);
        let dinf = boxed(b"dinf", &full(b"dref", 0, 0, &[&1u32.to_be_bytes()[..], &full(b"url ", 0, 1, &[])].concat()));

        let sample_entry = boxed(
            b"hvc1",
            &[
                &[0u8; 6][..],
                &1u16.to_be_bytes(), // data reference index
                &[0u8; 16],
                &self.width.to_be_bytes(),
                &self.height.to_be_bytes(),
                &0x0048_0000u32.to_be_bytes(), // 72 dpi
                &0x0048_0000u32.to_be_bytes(),
                &[0u8; 4],
                &1u16.to_be_bytes(), // frame count
                &[0u8; 32],          // compressor name
                &0x0018u16.to_be_bytes(),
                &(-1i16).to_be_bytes(),
                &self.hvcc,
            ]
            .concat(),
        );
        let stsd = full(b"stsd", 0, 0, &[&1u32.to_be_bytes()[..], &sample_entry].concat());
        // Durations, run-length coded.
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for &d in &durations {
            let d = u32::try_from(d).map_err(|_| invalid("a picture over a day long"))?;
            match runs.last_mut() {
                Some((n, delta)) if *delta == d => *n += 1,
                _ => runs.push((1, d)),
            }
        }
        // Each table: its entry count, then its entries.
        let stts =
            full(b"stts", 0, 0, &words([runs.len() as u32].into_iter().chain(runs.iter().flat_map(|(n, d)| [*n, *d]))));
        let stss = full(b"stss", 0, 0, &words([self.sync.len() as u32].into_iter().chain(self.sync.iter().copied())));
        // One chunk: the samples follow each other in `mdat`.
        let stsc = full(b"stsc", 0, 0, &words([1, 1, count, 1]));
        // No common sample size, then each sample's.
        let stsz = full(b"stsz", 0, 0, &words([0, count].into_iter().chain(self.sizes.iter().copied())));
        let first = u32::try_from(self.data_at).map_err(|_| invalid("mdat too far in"))?;
        let stco = full(b"stco", 0, 0, &words([1, first]));
        let stbl = boxed(b"stbl", &[stsd, stts, stss, stsc, stsz, stco].concat());
        let minf = boxed(b"minf", &[vmhd, dinf, stbl].concat());
        let mdia = boxed(b"mdia", &[mdhd, hdlr, minf].concat());
        let trak = boxed(b"trak", &[tkhd, mdia].concat());
        Ok(boxed(b"moov", &[mvhd, trak].concat()))
    }
}

/// The identity transformation matrix of `mvhd` and `tkhd`: 1.0 (16.16) at
/// a and d, 1.0 (2.30) at w.
#[rustfmt::skip]
const MATRIX: [u8; 36] = [
    0, 1, 0, 0,   0, 0, 0, 0,   0, 0, 0, 0,
    0, 0, 0, 0,   0, 1, 0, 0,   0, 0, 0, 0,
    0, 0, 0, 0,   0, 0, 0, 0,   0x40, 0, 0, 0,
];

/// The `hvcC` box: the decoder configuration record (ISO/IEC 14496-15,
/// 8.3.3.1.2), its parameter set arrays complete, as `hvc1` requires.
fn hvcc(sps: &Sps, vps_unit: &[u8], sps_unit: &[u8], pps_unit: &[u8]) -> io::Result<Vec<u8>> {
    let mut b = vec![1]; // configurationVersion
    b.extend_from_slice(&sps.profile_tier_level);
    b.extend_from_slice(&[0xf0, 0x00]); // min_spatial_segmentation_idc: none
    b.push(0xfc); // parallelismType: unknown
    b.push(0xfc | sps.chroma_format_idc);
    b.push(0xf8 | ((sps.bit_depth_luma - 8) & 0x07));
    b.push(0xf8 | ((sps.bit_depth_chroma - 8) & 0x07));
    b.extend_from_slice(&[0, 0]); // avgFrameRate: unspecified
    // constantFrameRate 0, numTemporalLayers, temporalIdNested, NAL lengths
    // of 4 bytes.
    b.push(((sps.max_sub_layers & 0x07) << 3) | (u8::from(sps.temporal_id_nesting) << 2) | 3);
    b.push(3);
    for (kind, unit) in [(nal::VPS, vps_unit), (nal::SPS, sps_unit), (nal::PPS, pps_unit)] {
        b.push(0x80 | kind); // array_completeness: none in the samples
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(
            &u16::try_from(unit.len()).map_err(|_| invalid("a parameter set over 64 KB"))?.to_be_bytes(),
        );
        b.extend_from_slice(unit);
    }
    Ok(boxed(b"hvcC", &b))
}

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(8 + body.len());
    b.extend_from_slice(&(8 + body.len() as u32).to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(body);
    b
}

/// A full box: a version and flags ahead of its body.
fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    boxed(kind, &[&((u32::from(version) << 24) | flags).to_be_bytes()[..], body].concat())
}

/// Big-endian `u32`s, one after another.
fn words(values: impl IntoIterator<Item = u32>) -> Vec<u8> {
    values.into_iter().flat_map(u32::to_be_bytes).collect()
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
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

    /// The boxes in `data`, by type, each with its body.
    fn children(mut data: &[u8]) -> Vec<([u8; 4], &[u8])> {
        let mut out = Vec::new();
        while data.len() >= 8 {
            let size = u32::from_be_bytes(data[..4].try_into().unwrap()) as u64;
            let kind: [u8; 4] = data[4..8].try_into().unwrap();
            let (header, size) =
                if size == 1 { (16, u64::from_be_bytes(data[8..16].try_into().unwrap())) } else { (8, size) };
            out.push((kind, &data[header..size as usize]));
            data = &data[size as usize..];
        }
        out
    }

    fn find<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> &'a [u8] {
        let (first, rest) = path.split_first().unwrap();
        let body = children(data).into_iter().find(|(k, _)| k == *first).unwrap_or_else(|| panic!("no {first:?}")).1;
        if rest.is_empty() { body } else { find(body, rest) }
    }

    #[test]
    fn a_recording_is_indexed_with_its_timing_and_keyframes() {
        let (vps, sps, pps) = (hex(VPS), hex(SPS), hex(PPS));
        let mut w = Mp4Writer::new(io::Cursor::new(Vec::new()), 24_000, &vps, &sps, &pps).unwrap();
        let idr: &[u8] = &[20 << 1, 1, 0xaa];
        let sei: &[u8] = &[39 << 1, 1, 0x05];
        let trail: &[u8] = &[1 << 1, 1, 0xbb, 0xcc];
        w.write_sample(1000, [sei, idr], true).unwrap();
        w.write_sample(1800, [trail], false).unwrap();
        w.write_sample(2600, [trail], false).unwrap();
        assert_eq!(w.samples(), 3);
        assert!((w.seconds() - 0.1).abs() < 1e-9, "3 pictures of 800/24000 s");
        let file = w.finish().unwrap().into_inner();

        let top: Vec<[u8; 4]> = children(&file).iter().map(|(k, _)| *k).collect();
        assert_eq!(top, [*b"ftyp", *b"mdat", *b"moov"]);
        // The samples, length-prefixed, in mdat.
        let mdat = find(&file, &[b"mdat"]);
        assert_eq!(&mdat[..4], &3u32.to_be_bytes());
        assert_eq!(mdat.len(), (4 + 3) * 2 + (4 + 4) * 2);
        let stbl = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];
        let table = |name: &[u8; 4]| find(&file, &[stbl.as_slice(), &[name]].concat()).to_vec();
        // One run of three 800-tick durations.
        assert_eq!(table(b"stts")[4..], [0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 3, 0x20]);
        // The first sample is the keyframe.
        assert_eq!(table(b"stss")[4..], [0, 0, 0, 1, 0, 0, 0, 1]);
        assert_eq!(table(b"stsz")[8..12], 3u32.to_be_bytes());
        // The chunk starts after ftyp and mdat's 16-byte header.
        let ftyp_len = 8 + 4 + 4 + 12;
        assert_eq!(table(b"stco")[8..], ((ftyp_len + 16) as u32).to_be_bytes());
        // hvc1 at 1920×1080, its hvcC at level 4 with all three parameter sets.
        let stsd = table(b"stsd");
        let hvc1 = find(&stsd[8..], &[b"hvc1"]);
        assert_eq!((&hvc1[24..26], &hvc1[26..28]), (&1920u16.to_be_bytes()[..], &1080u16.to_be_bytes()[..]));
        let hvcc = find(&hvc1[78..], &[b"hvcC"]);
        assert_eq!((hvcc[0], hvcc[1], hvcc[12]), (1, 0x01, 0x78));
        assert_eq!(hvcc[22], 3);
        assert_eq!((hvcc[23], hvcc[28]), (0x80 | 32, vps[0]));
        // The movie lasts 100 ms.
        let mvhd = find(&file, &[b"moov", b"mvhd"]);
        assert_eq!(&mvhd[16..20], &100u32.to_be_bytes());
    }
}
