//! The camera's payload: HEVC (H.265) over RTP (RFC 7798), as an iPhone
//! sends it to a host.
//!
//! Measured in trial 23 (2026-11-20; an iPhone18,1 on iOS 27.2.1, asked for
//! 1920×1080 at 30 pictures a second):
//! - payload type 100, one SSRC, a header extension on every packet;
//! - the VPS, SPS, PPS and a prefix SEI in one aggregation packet ahead of
//!   the first IDR picture; each picture one slice, in fragmentation units;
//! - timestamps from 0, 800 apart: 30 pictures a second on a 24 kHz clock;
//! - one IDR picture, at the start of a two-minute session, and trailing
//!   pictures (`TRAIL_R`) after it, none reordered;
//! - Main profile, level 4, 4:2:0, 8 bits;
//! - in order: none of 32,178 packets reordered, and one lost.

/// The camera's RTP clock: its timestamps advance 800 a picture at 30
/// pictures a second (trial 23).
pub const CLOCK_RATE: u32 = 24_000;

/// NAL unit types (H.265 table 7-1) and RTP payload structures (RFC 7798
/// section 4.4).
pub mod nal {
    /// Video parameter set.
    pub const VPS: u8 = 32;
    /// Sequence parameter set.
    pub const SPS: u8 = 33;
    /// Picture parameter set.
    pub const PPS: u8 = 34;
    /// Aggregation packet: several NAL units, each after its length.
    pub const AP: u8 = 48;
    /// Fragmentation unit: part of one NAL unit.
    pub const FU: u8 = 49;
    /// PACI packet: a payload header extension, which no capture has shown.
    pub const PACI: u8 = 50;
}

/// A NAL unit's type, from its header.
pub fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| (b >> 1) & 0x3f)
}

/// Whether a NAL unit of this type is part of a picture (a VCL NAL unit).
pub fn is_vcl(kind: u8) -> bool {
    kind < 32
}

/// Whether a picture of this type is one a decoder can start at: an IRAP
/// picture (BLA, IDR or CRA, types 16 to 23).
pub fn is_irap(kind: u8) -> bool {
    (16..=23).contains(&kind)
}

/// Whether a NAL unit is a parameter set.
pub fn is_parameter_set(kind: u8) -> bool {
    matches!(kind, nal::VPS | nal::SPS | nal::PPS)
}

/// NAL units in Annex B (H.265 annex B): each after a four-byte start code,
/// as decoders and FFmpeg's muxers take them.
pub fn annex_b<T: AsRef<[u8]>>(units: impl IntoIterator<Item = T>) -> Vec<u8> {
    let mut out = Vec::new();
    for unit in units {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(unit.as_ref());
    }
    out
}

/// One picture's NAL units, with what came ahead of it (parameter sets,
/// SEI), and its RTP timestamp.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessUnit {
    pub timestamp: u32,
    pub nals: Vec<Vec<u8>>,
}

impl AccessUnit {
    /// Whether it holds a picture.
    pub fn has_picture(&self) -> bool {
        self.nals.iter().any(|n| nal_type(n).is_some_and(is_vcl))
    }

    /// Whether a decoder can start at it.
    pub fn is_keyframe(&self) -> bool {
        self.nals.iter().any(|n| nal_type(n).is_some_and(is_irap))
    }
}

/// Puts the camera's RTP packets back together into access units.
///
/// An access unit ends at its last packet's marker bit, or when a packet
/// with a new timestamp arrives. NAL units that come without a picture
/// (the parameter sets ahead of the first) join the next one. Packets are
/// taken in arrival order, which was sequence order in trial 23: one older
/// than the last is dropped, and a NAL unit that lost a fragment is dropped
/// whole.
#[derive(Debug, Default)]
pub struct Depacketizer {
    unit: Option<AccessUnit>,
    fragment: Option<Vec<u8>>,
    last_seq: Option<u16>,
    /// Packets missing, by sequence number.
    pub lost: u64,
    /// NAL units dropped for a missing fragment.
    pub broken: u64,
}

impl Depacketizer {
    /// One RTP packet: its sequence number, timestamp, marker bit and
    /// payload. Returns the access units it completes.
    pub fn push(&mut self, seq: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Vec<AccessUnit> {
        let mut done = Vec::new();
        if let Some(last) = self.last_seq {
            let step = seq.wrapping_sub(last);
            if step == 0 || step >= 0x8000 {
                return done;
            }
            if step > 1 {
                self.lost += u64::from(step - 1);
                self.drop_fragment();
            }
        }
        self.last_seq = Some(seq);
        if let Some(previous) = self.unit.take_if(|u| u.timestamp != timestamp) {
            self.drop_fragment();
            if previous.has_picture() {
                done.push(previous);
            } else {
                self.unit = Some(AccessUnit { timestamp, nals: previous.nals });
            }
        }
        self.unit.get_or_insert_with(|| AccessUnit { timestamp, nals: Vec::new() });
        self.payload(payload);
        if marker && self.unit.as_ref().is_some_and(AccessUnit::has_picture) {
            self.drop_fragment();
            done.extend(self.unit.take());
        }
        done
    }

    /// The access unit in progress, at the end of the stream, if it holds a
    /// picture.
    pub fn flush(&mut self) -> Option<AccessUnit> {
        self.drop_fragment();
        self.unit.take().filter(AccessUnit::has_picture)
    }

    fn payload(&mut self, payload: &[u8]) {
        if payload.len() < 2 {
            return;
        }
        match nal_type(payload) {
            Some(nal::AP) => {
                let mut rest = &payload[2..];
                while let [high, low, tail @ ..] = rest {
                    let len = usize::from(u16::from_be_bytes([*high, *low]));
                    let Some(unit) = tail.get(..len) else { break };
                    self.add(unit.to_vec());
                    rest = &tail[len..];
                }
            }
            Some(nal::FU) => {
                let Some(&header) = payload.get(2) else { return };
                let (start, end, kind) = (header & 0x80 != 0, header & 0x40 != 0, header & 0x3f);
                if start {
                    self.drop_fragment();
                    // The NAL unit's header: the payload header's F bit and
                    // layer and temporal IDs, with the fragment's type.
                    let mut unit = vec![(payload[0] & 0x81) | (kind << 1), payload[1]];
                    unit.extend_from_slice(&payload[3..]);
                    self.fragment = Some(unit);
                } else if let Some(unit) = &mut self.fragment {
                    unit.extend_from_slice(&payload[3..]);
                }
                if end && let Some(unit) = self.fragment.take() {
                    self.add(unit);
                }
            }
            Some(nal::PACI) => {}
            _ => self.add(payload.to_vec()),
        }
    }

    fn add(&mut self, unit: Vec<u8>) {
        if let Some(au) = &mut self.unit {
            au.nals.push(unit);
        }
    }

    fn drop_fragment(&mut self) {
        if self.fragment.take().is_some() {
            self.broken += 1;
        }
    }
}

/// What a container needs from a sequence parameter set (H.265 section
/// 7.3.2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sps {
    /// The general profile, tier and level, as `profile_tier_level` has
    /// them: profile space, tier and profile, the compatibility flags (4
    /// bytes), the constraint flags (6), the level.
    pub profile_tier_level: [u8; 12],
    pub max_sub_layers: u8,
    pub temporal_id_nesting: bool,
    pub chroma_format_idc: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    /// The picture's size, after the conformance window's cropping.
    pub width: u32,
    pub height: u32,
}

impl Sps {
    /// Reads an SPS NAL unit, header included. `None` if it isn't one, or
    /// is cut short.
    pub fn parse(unit: &[u8]) -> Option<Self> {
        if nal_type(unit) != Some(nal::SPS) {
            return None;
        }
        let rbsp = unescape(unit.get(2..)?);
        let mut r = Bits { data: &rbsp, pos: 0 };
        r.skip(4)?; // sps_video_parameter_set_id
        let max_sub_layers = r.read(3)? as u8 + 1;
        let temporal_id_nesting = r.read(1)? == 1;
        let mut profile_tier_level = [0u8; 12];
        for byte in &mut profile_tier_level {
            *byte = r.read(8)? as u8;
        }
        let sub_layers = usize::from(max_sub_layers - 1);
        let mut present = [(false, false); 7];
        for p in &mut present[..sub_layers] {
            *p = (r.read(1)? == 1, r.read(1)? == 1);
        }
        if sub_layers > 0 {
            r.skip(2 * (8 - sub_layers))?;
        }
        for &(profile, level) in &present[..sub_layers] {
            r.skip(if profile { 88 } else { 0 } + if level { 8 } else { 0 })?;
        }
        r.ue()?; // sps_seq_parameter_set_id
        let chroma_format_idc = u8::try_from(r.ue()?).ok().filter(|c| *c <= 3)?;
        let separate_colour_planes = chroma_format_idc == 3 && r.read(1)? == 1;
        let (mut width, mut height) = (r.ue()?, r.ue()?);
        if r.read(1)? == 1 {
            let (left, right, top, bottom) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
            // SubWidthC and SubHeightC (table 6-1); 1 with separate planes.
            let (sub_width, sub_height) = match (chroma_format_idc, separate_colour_planes) {
                (1, _) => (2, 2),
                (2, _) => (2, 1),
                _ => (1, 1),
            };
            width = width.checked_sub(sub_width * left.checked_add(right)?)?;
            height = height.checked_sub(sub_height * top.checked_add(bottom)?)?;
        }
        let depth = |r: &mut Bits| u8::try_from(r.ue()?).ok().filter(|d| *d <= 8).map(|d| d + 8);
        let bit_depth_luma = depth(&mut r)?;
        let bit_depth_chroma = depth(&mut r)?;
        Some(Self {
            profile_tier_level,
            max_sub_layers,
            temporal_id_nesting,
            chroma_format_idc,
            bit_depth_luma,
            bit_depth_chroma,
            width,
            height,
        })
    }
}

/// A NAL unit's payload without its emulation prevention bytes (the 3 in
/// each `00 00 03`): its RBSP.
fn unescape(ebsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ebsp.len());
    let mut zeros = 0;
    for &b in ebsp {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// A big-endian bit reader.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            v = (v << 1) | u32::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(v)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.pos += n;
        (self.pos <= self.data.len() * 8).then_some(())
    }

    /// An unsigned Exp-Golomb code, `ue(v)`.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        ((1u32 << zeros) - 1).checked_add(self.read(zeros)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// Trial 23's parameter sets, from its first packet.
    const VPS: &str = "40010c01ffff016000000300b000000300000300781b0240";
    const SPS: &str = "420101016000000300b00000030000030078a003c0801107cb881b9245232e7f13f0bfa1bf5220";
    const PPS: &str = "4401c0724c16c9";

    #[test]
    fn the_iphones_sps_is_1080p_main_at_level_4() {
        let sps = Sps::parse(&hex(SPS)).unwrap();
        assert_eq!((sps.width, sps.height), (1920, 1080));
        // Main, compatible with Main and Main 10; progressive, frame only;
        // level 4 (120): read past the emulation prevention bytes.
        assert_eq!(sps.profile_tier_level, [0x01, 0x60, 0, 0, 0, 0xb0, 0, 0, 0, 0, 0, 0x78]);
        assert_eq!((sps.max_sub_layers, sps.temporal_id_nesting), (1, true));
        assert_eq!((sps.chroma_format_idc, sps.bit_depth_luma, sps.bit_depth_chroma), (1, 8, 8));
        assert_eq!(Sps::parse(&hex(PPS)), None);
        assert_eq!(Sps::parse(&hex(SPS)[..20]), None);
    }

    /// An aggregation packet of the given NAL units.
    fn ap(units: &[&[u8]]) -> Vec<u8> {
        let mut p = vec![nal::AP << 1, 1];
        for u in units {
            p.extend_from_slice(&(u.len() as u16).to_be_bytes());
            p.extend_from_slice(u);
        }
        p
    }

    /// `unit` cut into fragmentation units of `size` bytes of payload.
    fn fus(unit: &[u8], size: usize) -> Vec<Vec<u8>> {
        let kind = nal_type(unit).unwrap();
        let chunks: Vec<&[u8]> = unit[2..].chunks(size).collect();
        chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let mut header = kind;
                if i == 0 {
                    header |= 0x80;
                }
                if i == chunks.len() - 1 {
                    header |= 0x40;
                }
                let mut p = vec![(unit[0] & 0x81) | (nal::FU << 1), unit[1], header];
                p.extend_from_slice(chunk);
                p
            })
            .collect()
    }

    #[test]
    fn packets_become_pictures() {
        let (vps, sps, pps) = (hex(VPS), hex(SPS), hex(PPS));
        // An IDR (type 20) of 100 bytes, then a trailing picture (type 1).
        let mut idr = vec![20 << 1, 1];
        idr.extend((0..100).map(|i| i as u8));
        let trail = vec![1 << 1, 1, 0xaa, 0xbb];
        let mut d = Depacketizer::default();
        // The parameter sets, marked as the iPhone's sometimes are: they
        // wait for their picture.
        assert!(d.push(10, 0, true, &ap(&[&vps, &sps, &pps])).is_empty());
        let fragments = fus(&idr, 40);
        assert_eq!(fragments.len(), 3);
        let mut out = Vec::new();
        for (i, f) in fragments.iter().enumerate() {
            out.extend(d.push(11 + i as u16, 0, i == 2, f));
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].nals, vec![vps, sps, pps, idr]);
        assert!(out[0].is_keyframe());
        // A picture without a marker ends at the next timestamp: this packet
        // ends that one, and its own with its marker.
        assert!(d.push(14, 800, false, &trail).is_empty());
        let next = d.push(15, 1600, true, &trail);
        let stamps: Vec<_> = next.iter().map(|u| (u.timestamp, u.is_keyframe())).collect();
        assert_eq!(stamps, [(800, false), (1600, false)]);
        assert_eq!((d.lost, d.broken), (0, 0));
    }

    #[test]
    fn a_lost_fragment_drops_its_nal_unit() {
        let mut slice = vec![1 << 1, 1];
        slice.extend([7u8; 90]);
        let f = fus(&slice, 30);
        let mut d = Depacketizer::default();
        assert!(d.push(1, 800, false, &f[0]).is_empty());
        // The middle fragment never arrives.
        assert!(d.push(3, 800, true, &f[2]).is_empty());
        assert_eq!((d.lost, d.broken), (1, 1));
        // A late or repeated packet is dropped.
        assert!(d.push(2, 800, false, &f[1]).is_empty());
        // The next picture is whole.
        let whole = d.push(4, 1600, true, &[1 << 1, 1, 9]);
        assert_eq!(whole.len(), 1);
        assert_eq!(d.flush(), None);
    }
}
