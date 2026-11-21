//! AVConference's media negotiation, on the Control stream
//! (`docs/guest-pairing.md`, trial 13): the iPhone's offer, and the host's
//! answer.
//!
//! Both travel as Control selector 2 with `[entity, data]`. The offer is a
//! binary plist whose media blob is a zlib-compressed `VCMediaNegotiationBlob`
//! protobuf. A host answers as tvOS 27.2's `-[AVCMediaStreamNegotiator
//! initWithOffer:error:]` does (`createAnswer`, `-[VCMediaNegotiator
//! newResponseBlob]`): a binary plist of its own media blob and call-info
//! blob, with no mode or call ID. The codec is chosen by `payloadFlags`, a
//! bitmap of RTP payload types; the rest of each side's audio parameters come
//! from its own local settings for the mode.

use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MODE: &str = "avcMediaStreamNegotiatorMode";
pub const CALL_ID: &str = "avcMediaStreamOptionCallID";
pub const MEDIA_BLOB: &str = "avcMediaStreamNegotiatorMediaBlob";
pub const ENDPOINT_INFO: &str = "avcMediaStreamOptionRemoteEndpointInfo";

/// `VCMediaNegotiationBlob`'s fields (from its `writeTo:`), those a mic-only
/// negotiation uses.
pub mod blob {
    pub const ALLOW_DYNAMIC_MAX_BITRATE: u32 = 1;
    pub const ALLOWS_CONTENTS_CHANGE_WITH_ASPECT_PRESERVATION: u32 = 2;
    pub const AUDIO_SETTINGS: u32 = 3;
    pub const VIDEO_SETTINGS: u32 = 4;
    pub const USER_AGENT: u32 = 6;
    pub const BASEBAND_CODEC_SAMPLE_RATE: u32 = 8;
    /// Repeated: `{1 configuration, 2 maxBandwidth, 3 configurationExtension}`.
    pub const BANDWIDTH_SETTINGS: u32 = 9;
    /// NTP format: seconds since 1900 in the high 32 bits, the fraction in the low.
    pub const NTP_TIME: u32 = 13;
    pub const BLOB_VERSION: u32 = 14;
    pub const MEDIA_CONTROL_INFO_VERSION: u32 = 16;
}

/// `VCMediaNegotiationBlobAudioSettings`'s fields.
pub mod audio {
    pub const RTP_SSRC: u32 = 1;
    pub const AUDIO_UNIT_MODEL: u32 = 2;
    pub const SUPPORT_FLAGS: u32 = 3;
    pub const PAYLOAD_FLAGS: u32 = 4;
    pub const SECONDARY_FLAGS: u32 = 5;
    pub const USE_SBR: u32 = 6;
}

/// `VCMediaNegotiationBlobVideoSettings`'s fields (its `writeTo:`), for the
/// camera.
pub mod video {
    pub const RTP_SSRC: u32 = 1;
    pub const ALLOW_RTCP_FB: u32 = 2;
    /// Repeated: the payloads offered, each with its rule collections
    /// (`VCMediaNegotiationBlobVideoRuleCollection`: 1 transport, 2
    /// operation, 3 formats, 4 preferred format, 5 and 6 their extensions).
    pub const PAYLOAD_COLLECTIONS: u32 = 3;
    pub const CUSTOM_WIDTH: u32 = 4;
    pub const CUSTOM_HEIGHT: u32 = 5;
    pub const TILES_PER_FRAME: u32 = 6;
    pub const LTRP_ENABLED: u32 = 7;
    pub const PIXEL_FORMATS: u32 = 8;
    pub const HDR_MODES_SUPPORTED: u32 = 9;
    pub const FEC_ENABLED: u32 = 10;
    pub const RTX_ENABLED: u32 = 11;
    pub const BLACK_FRAME_ON_CLEAR_SCREEN: u32 = 12;
    pub const FOVEATION_SUPPORTED: u32 = 13;
    pub const INTERLEAVED_ENCODING: u32 = 14;
}

/// `VCCallInfoBlob`'s fields: the endpoint info, sent uncompressed.
pub mod call_info {
    pub const CALL_ID: u32 = 1;
    pub const CLIENT_VERSION: u32 = 2;
    pub const DEVICE_TYPE: u32 = 3;
    pub const FRAMEWORK_VERSION: u32 = 4;
    pub const OS_VERSION: u32 = 5;
    pub const DEVICE_NAME: u32 = 6;
    pub const AUDIO_DEVICE_UID: u32 = 7;
}

/// RTP payload types and their bits in `payloadFlags`, from
/// `+[VCMediaNegotiationBlobAudioSettings(AudioRules) negotiationPayloadFromPayload:]`.
/// AVConference's codec numbers for two of them, from
/// `_VCPayloadUtils_CodecTypeForPayload`: 101 is 16 (AAC-ELD 48 kHz, going by
/// the order of the codec names) and 119 is 11.
pub const PAYLOAD_FLAGS: [(u8, u32); 14] = [
    (9, 0x1),
    (106, 0x2),
    (105, 0x4),
    (104, 0x8),
    (122, 0x10),
    (121, 0x20),
    (119, 0x40),
    (97, 0x80),
    (98, 0x100),
    (13, 0x200),
    (20, 0x400),
    (108, 0x800),
    (101, 0x1000),
    (113, 0x4000),
];

pub fn payload_flag(payload: u8) -> Option<u32> {
    PAYLOAD_FLAGS.iter().find(|(p, _)| *p == payload).map(|(_, flag)| *flag)
}

/// The payload types in a `payloadFlags` bitmap, in bit order.
pub fn payloads(flags: u32) -> Vec<u8> {
    let mut found: Vec<(u32, u8)> =
        PAYLOAD_FLAGS.iter().filter(|(_, flag)| flags & flag != 0).map(|(p, flag)| (*flag, *p)).collect();
    found.sort_unstable();
    found.into_iter().map(|(_, p)| p).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum AvcError {
    #[error("the offer isn't a property list: {0}")]
    Plist(#[from] plist::Error),
    #[error("the offer isn't a dictionary")]
    NotADictionary,
    #[error("the offer has no {MEDIA_BLOB}")]
    NoMediaBlob,
    #[error("a blob isn't zlib")]
    Zlib,
    #[error("a blob isn't a protobuf")]
    Protobuf,
}

/// A protobuf message, field by field in wire order. Just enough of the
/// format for these blobs; groups (wire types 3 and 4) aren't.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Proto(pub Vec<(u32, Field)>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Varint(u64),
    Fixed64(u64),
    Bytes(Vec<u8>),
    Fixed32(u32),
}

impl Proto {
    pub fn parse(mut bytes: &[u8]) -> Option<Self> {
        let mut fields = Vec::new();
        while !bytes.is_empty() {
            let key = read_varint(&mut bytes)?;
            let number = u32::try_from(key >> 3).ok().filter(|n| *n != 0)?;
            let field = match key & 7 {
                0 => Field::Varint(read_varint(&mut bytes)?),
                1 => Field::Fixed64(u64::from_le_bytes(take(&mut bytes, 8)?.try_into().ok()?)),
                2 => {
                    let len = usize::try_from(read_varint(&mut bytes)?).ok()?;
                    Field::Bytes(take(&mut bytes, len)?.to_vec())
                }
                5 => Field::Fixed32(u32::from_le_bytes(take(&mut bytes, 4)?.try_into().ok()?)),
                _ => return None,
            };
            fields.push((number, field));
        }
        Some(Self(fields))
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (number, field) in &self.0 {
            let wire = match field {
                Field::Varint(_) => 0,
                Field::Fixed64(_) => 1,
                Field::Bytes(_) => 2,
                Field::Fixed32(_) => 5,
            };
            write_varint(&mut out, u64::from(*number) << 3 | wire);
            match field {
                Field::Varint(v) => write_varint(&mut out, *v),
                Field::Fixed64(v) => out.extend_from_slice(&v.to_le_bytes()),
                Field::Bytes(b) => {
                    write_varint(&mut out, b.len() as u64);
                    out.extend_from_slice(b);
                }
                Field::Fixed32(v) => out.extend_from_slice(&v.to_le_bytes()),
            }
        }
        out
    }

    pub fn get(&self, number: u32) -> Option<&Field> {
        self.0.iter().find(|(n, _)| *n == number).map(|(_, f)| f)
    }

    pub fn all(&self, number: u32) -> impl Iterator<Item = &Field> {
        self.0.iter().filter(move |(n, _)| *n == number).map(|(_, f)| f)
    }

    pub fn varint(&self, number: u32) -> Option<u64> {
        match self.get(number)? {
            Field::Varint(v) => Some(*v),
            _ => None,
        }
    }

    pub fn string(&self, number: u32) -> Option<&str> {
        match self.get(number)? {
            Field::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }

    pub fn message(&self, number: u32) -> Option<Proto> {
        match self.get(number)? {
            Field::Bytes(b) => Proto::parse(b),
            _ => None,
        }
    }

    /// Every field, one a line, nested messages indented, for logs: bytes
    /// that parse as a message are shown as one, printable ones as text,
    /// anything else as hex.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        self.describe_into(&mut out, 0);
        out
    }

    fn describe_into(&self, out: &mut String, depth: usize) {
        let pad = "  ".repeat(depth);
        for (number, field) in &self.0 {
            match field {
                Field::Varint(v) => out.push_str(&format!("{pad}{number}: {v}\n")),
                Field::Fixed64(v) => out.push_str(&format!("{pad}{number}: {v:#018x} (64-bit)\n")),
                Field::Fixed32(v) => out.push_str(&format!("{pad}{number}: {v:#010x} (32-bit)\n")),
                Field::Bytes(b) => match (std::str::from_utf8(b), Proto::parse(b)) {
                    (Ok(s), _) if !s.is_empty() && s.chars().all(|c| !c.is_control()) => {
                        out.push_str(&format!("{pad}{number}: {s:?}\n"))
                    }
                    (_, Some(inner)) if !b.is_empty() && inner.0.iter().all(|(n, _)| *n < 64) => {
                        out.push_str(&format!("{pad}{number}: {{\n"));
                        inner.describe_into(out, depth + 1);
                        out.push_str(&format!("{pad}}}\n"));
                    }
                    _ => {
                        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                        out.push_str(&format!("{pad}{number}: 0x{hex}\n"));
                    }
                },
            }
        }
    }

    /// Sets a field: replaces its first occurrence (and drops any others),
    /// or adds it in field-number order, as protobuf writers emit them.
    pub fn with(mut self, number: u32, field: Field) -> Self {
        match self.0.iter().position(|(n, _)| *n == number) {
            Some(i) => {
                self.0[i].1 = field;
                let mut seen = false;
                self.0.retain(|(n, _)| *n != number || !std::mem::replace(&mut seen, true));
            }
            None => {
                let at = self.0.iter().position(|(n, _)| *n > number).unwrap_or(self.0.len());
                self.0.insert(at, (number, field));
            }
        }
        self
    }
}

fn read_varint(bytes: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if bytes.len() < len {
        return None;
    }
    let (head, rest) = bytes.split_at(len);
    *bytes = rest;
    Some(head)
}

fn decompress(data: &[u8]) -> Result<Vec<u8>, AvcError> {
    // AVConference compresses into a 2500-byte buffer; allow far more.
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, 1 << 20).map_err(|_| AvcError::Zlib)
}

fn compress(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

/// The iPhone's offer.
#[derive(Clone, Debug)]
pub struct Offer {
    pub mode: Option<u64>,
    pub call_id: Option<String>,
    /// The media blob, decompressed.
    pub blob: Proto,
    /// The iPhone's call info: model, AVConference version, OS build.
    pub endpoint: Option<Proto>,
}

impl Offer {
    pub fn parse(data: &[u8]) -> Result<Self, AvcError> {
        let value = plist::Value::from_reader(Cursor::new(data))?;
        let dict = value.as_dictionary().ok_or(AvcError::NotADictionary)?;
        let blob = dict.get(MEDIA_BLOB).and_then(plist::Value::as_data).ok_or(AvcError::NoMediaBlob)?;
        let blob = Proto::parse(&decompress(blob)?).ok_or(AvcError::Protobuf)?;
        let endpoint = match dict.get(ENDPOINT_INFO).and_then(plist::Value::as_data) {
            Some(info) => Some(Proto::parse(info).ok_or(AvcError::Protobuf)?),
            None => None,
        };
        Ok(Self {
            mode: dict.get(MODE).and_then(plist::Value::as_unsigned_integer),
            call_id: dict.get(CALL_ID).and_then(plist::Value::as_string).map(str::to_owned),
            blob,
            endpoint,
        })
    }

    pub fn audio(&self) -> Option<Proto> {
        self.blob.message(blob::AUDIO_SETTINGS)
    }

    /// The camera's settings, in a camera's offer.
    pub fn video(&self) -> Option<Proto> {
        self.blob.message(blob::VIDEO_SETTINGS)
    }

    /// The camera's settings, for a log.
    pub fn describe_video(&self) -> String {
        self.video().map_or_else(|| "none".into(), |v| v.describe())
    }

    /// The iPhone's RTP SSRC.
    pub fn ssrc(&self) -> Option<u32> {
        self.audio()?.varint(audio::RTP_SSRC).and_then(|v| u32::try_from(v).ok())
    }

    /// The iPhone's model identifier, such as `iPhone18,1`.
    pub fn model(&self) -> Option<&str> {
        self.endpoint.as_ref()?.string(call_info::DEVICE_TYPE)
    }

    /// The iPhone's OS build, as its call info gives it (`24B156`).
    pub fn os_build(&self) -> Option<&str> {
        self.endpoint.as_ref()?.string(call_info::OS_VERSION)
    }

    /// The payload types offered.
    pub fn payloads(&self) -> Vec<u8> {
        let flags = self.audio().and_then(|a| a.varint(audio::PAYLOAD_FLAGS)).unwrap_or(0);
        payloads(u32::try_from(flags).unwrap_or(0))
    }
}

/// What the host puts in its answer.
#[derive(Clone, Debug)]
pub struct AnswerSettings<'a> {
    /// The payload type chosen, from those offered.
    pub payload: u8,
    /// The host's RTP SSRC.
    pub ssrc: u32,
    /// [`ntp_now`], normally.
    pub ntp_time: u64,
    /// The host's model and OS build, for its call info.
    pub device_type: &'a str,
    pub os_version: &'a str,
}

/// The host's answer to `offer`.
///
/// The media blob mirrors the offer's (the user agent, the bandwidth
/// settings, the versions), since both ends run the same AVConference, with
/// the time. The call info mirrors the iPhone's, with the host's model and
/// build. An offer without call info is answered with the bare compressed
/// blob, as tvOS does.
///
/// For the microphone, the blob has the host's own audio settings
/// (`settings.ssrc`, `settings.payload` alone in `payloadFlags`, no
/// secondary payloads). For the camera (an offer with video settings), its
/// video settings mirror the offer's with the host's SSRC: the host takes
/// whatever the iPhone offers. (Which payload that leaves is for a trial to
/// show.)
pub fn answer(offer: &Offer, settings: &AnswerSettings) -> Result<Vec<u8>, AvcError> {
    let blob = match offer.video() {
        Some(video) => {
            let video = video.with(video::RTP_SSRC, Field::Varint(u64::from(settings.ssrc)));
            offer.blob.clone().with(blob::VIDEO_SETTINGS, Field::Bytes(video.encode()))
        }
        None => {
            let flag = payload_flag(settings.payload).unwrap_or(0);
            let audio = offer
                .audio()
                .unwrap_or_default()
                .with(audio::RTP_SSRC, Field::Varint(u64::from(settings.ssrc)))
                .with(audio::PAYLOAD_FLAGS, Field::Varint(u64::from(flag)))
                .with(audio::SECONDARY_FLAGS, Field::Varint(0));
            offer.blob.clone().with(blob::AUDIO_SETTINGS, Field::Bytes(audio.encode()))
        }
    };
    let blob = blob.with(blob::NTP_TIME, Field::Varint(settings.ntp_time));
    let compressed = compress(&blob.encode());
    let Some(endpoint) = &offer.endpoint else { return Ok(compressed) };
    let info = endpoint
        .clone()
        .with(call_info::DEVICE_TYPE, Field::Bytes(settings.device_type.as_bytes().to_vec()))
        .with(call_info::OS_VERSION, Field::Bytes(settings.os_version.as_bytes().to_vec()));
    let mut dict = plist::Dictionary::new();
    dict.insert(MEDIA_BLOB.into(), plist::Value::Data(compressed));
    dict.insert(ENDPOINT_INFO.into(), plist::Value::Data(info.encode()));
    let mut out = Vec::new();
    plist::Value::Dictionary(dict).to_writer_binary(&mut out)?;
    Ok(out)
}

/// The time now in NTP format, as the blobs carry it.
pub fn ntp_now() -> u64 {
    const NTP_EPOCH_OFFSET: u64 = 2_208_988_800; // 1900 → 1970
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let fraction = (u64::from(now.subsec_nanos()) << 32) / 1_000_000_000;
    (now.as_secs() + NTP_EPOCH_OFFSET) << 32 | fraction
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offer from trial 12: an iPhone18,1 on 24B156, mode 4, 'call'.
    const OFFER: &[u8] = include_bytes!("../testdata/avc-offer.bplist");

    #[test]
    fn the_offer_parses() {
        let offer = Offer::parse(OFFER).unwrap();
        assert_eq!(offer.mode, Some(4));
        assert_eq!(offer.ssrc(), Some(745_648_787));
        assert_eq!(offer.payloads(), [9, 106, 105, 104, 122, 121, 119, 13, 20, 108, 101]);
        assert_eq!(offer.blob.string(blob::USER_AGENT), Some("Viceroy 1.7.0/GK"));
        assert_eq!(offer.blob.all(blob::BANDWIDTH_SETTINGS).count(), 15);
        assert_eq!(offer.model(), Some("iPhone18,1"));
    }

    #[test]
    fn protobufs_round_trip_byte_for_byte() {
        let dict = plist::Value::from_reader(Cursor::new(OFFER)).unwrap();
        let blob = dict.as_dictionary().unwrap().get(MEDIA_BLOB).unwrap().as_data().unwrap();
        let raw = decompress(blob).unwrap();
        assert_eq!(Proto::parse(&raw).unwrap().encode(), raw);
    }

    #[test]
    fn with_replaces_or_inserts_in_order() {
        let p = Proto(vec![(1, Field::Varint(1)), (3, Field::Varint(3))]);
        let p = p.with(2, Field::Varint(2)).with(3, Field::Varint(30)).with(4, Field::Varint(4));
        let numbers: Vec<_> = p.0.iter().map(|(n, _)| *n).collect();
        assert_eq!(numbers, [1, 2, 3, 4]);
        assert_eq!(p.varint(3), Some(30));
    }

    #[test]
    fn the_answer_has_the_hosts_audio_settings_and_call_info() {
        let offer = Offer::parse(OFFER).unwrap();
        let settings = AnswerSettings {
            payload: 101,
            ssrc: 0x1234_5678,
            ntp_time: 0xeea9_2703_0000_0000,
            device_type: "AppleTV14,1",
            os_version: "24K122",
        };
        let bytes = answer(&offer, &settings).unwrap();
        let value = plist::Value::from_reader(Cursor::new(&bytes)).unwrap();
        let mut keys: Vec<_> = value.as_dictionary().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, [MEDIA_BLOB, ENDPOINT_INFO]);

        // It reads back as an offer would.
        let back = Offer::parse(&bytes).unwrap();
        assert_eq!(back.ssrc(), Some(0x1234_5678));
        assert_eq!(back.payloads(), [101]);
        assert_eq!(back.audio().unwrap().varint(audio::SECONDARY_FLAGS), Some(0));
        assert_eq!(back.blob.varint(blob::NTP_TIME), Some(0xeea9_2703_0000_0000));
        assert_eq!(back.blob.varint(blob::BLOB_VERSION), Some(2));
        assert_eq!(back.blob.all(blob::BANDWIDTH_SETTINGS).count(), 15);
        let info = back.endpoint.unwrap();
        assert_eq!(info.string(call_info::DEVICE_TYPE), Some("AppleTV14,1"));
        assert_eq!(info.string(call_info::OS_VERSION), Some("24K122"));
        assert_eq!(info.string(call_info::FRAMEWORK_VERSION), Some("2260.15.1"));
    }

    #[test]
    fn a_camera_offer_is_answered_with_its_own_video_settings() {
        // A camera's offer, made up: video settings with the iPhone's SSRC
        // and one payload collection.
        let collection = Proto(vec![(1, Field::Varint(100)), (2, Field::Bytes(vec![0x08, 0x01]))]);
        let video = Proto(vec![
            (video::RTP_SSRC, Field::Varint(42)),
            (video::PAYLOAD_COLLECTIONS, Field::Bytes(collection.encode())),
        ]);
        let blob =
            Proto(vec![(blob::VIDEO_SETTINGS, Field::Bytes(video.encode())), (blob::BLOB_VERSION, Field::Varint(2))]);
        let mut dict = plist::Dictionary::new();
        dict.insert(MODE.into(), plist::Value::Integer(3.into()));
        dict.insert(MEDIA_BLOB.into(), plist::Value::Data(compress(&blob.encode())));
        let info = Proto(vec![(call_info::DEVICE_TYPE, Field::Bytes(b"iPhone18,1".to_vec()))]);
        dict.insert(ENDPOINT_INFO.into(), plist::Value::Data(info.encode()));
        let mut bytes = Vec::new();
        plist::Value::Dictionary(dict).to_writer_binary(&mut bytes).unwrap();
        let offer = Offer::parse(&bytes).unwrap();
        assert!(offer.video().is_some());
        assert!(offer.describe_video().contains("3: {"));

        let settings = AnswerSettings { payload: 101, ssrc: 7, ntp_time: 1, device_type: "x", os_version: "y" };
        let back = Offer::parse(&answer(&offer, &settings).unwrap()).unwrap();
        let video = back.video().unwrap();
        assert_eq!(video.varint(video::RTP_SSRC), Some(7));
        assert_eq!(video.get(video::PAYLOAD_COLLECTIONS), Some(&Field::Bytes(collection.encode())));
        assert!(back.audio().is_none(), "no audio settings in a camera's answer");
    }

    #[test]
    fn payload_flags_map_both_ways() {
        assert_eq!(payload_flag(101), Some(0x1000));
        assert_eq!(payload_flag(112), None);
        assert_eq!(payloads(0x1E7F), [9, 106, 105, 104, 122, 121, 119, 13, 20, 108, 101]);
    }

    #[test]
    fn ntp_time_is_after_2026() {
        // 2026-01-01 is 3_976_214_400 s after 1900.
        assert!(ntp_now() >> 32 > 3_976_214_400);
    }
}
