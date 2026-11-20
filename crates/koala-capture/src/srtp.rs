//! SRTP and SRTCP for the microphone stream: AES_256_CM_HMAC_SHA1_80, RFC
//! 3711's transforms with RFC 6188's AES-256 counter mode and key
//! derivation, and 80-bit HMAC-SHA1 tags.
//!
//! AVConference calls it cipher suite 7. One master key serves both ways:
//! the 32-byte `_streamKey` the host gives in its UDP stream answer, then
//! the first 14 bytes of the session configuration's `sessionID` as the
//! master salt. Settled on the iPhone's own packets (Koala's trial 21,
//! 2026-11-19): with that split, 200 of 200 SRTP tags verified, and none
//! with the other splits or a 32-bit tag. The key derivation rate is 0.

use aes::Aes256;
use ctr::Ctr128BE;
use ctr::cipher::{KeyIvInit, StreamCipher};
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

/// The master key's length.
pub const MASTER_KEY_LEN: usize = 32;
/// The master salt's length.
pub const MASTER_SALT_LEN: usize = 14;
/// The authentication tag's length: 80 bits.
pub const TAG_LEN: usize = 10;

/// RFC 3711's key derivation labels (section 4.3.2).
mod label {
    pub const RTP_ENCRYPTION: u8 = 0;
    pub const RTP_AUTH: u8 = 1;
    pub const RTP_SALT: u8 = 2;
    pub const RTCP_ENCRYPTION: u8 = 3;
    pub const RTCP_AUTH: u8 = 4;
    pub const RTCP_SALT: u8 = 5;
}

/// Why a packet was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SrtpError {
    #[error("too short for {0}")]
    Short(&'static str),
    #[error("the authentication tag doesn't verify")]
    Auth,
}

type HmacSha1 = Hmac<Sha1>;

/// AES-256 in counter mode from `iv`: `n` bytes of keystream XORed into
/// `buffer`, or written as keystream when `buffer` is zeros.
fn aes_cm(key: &[u8; 32], iv: u128, buffer: &mut [u8]) {
    let mut cipher = Ctr128BE::<Aes256>::new(&(*key).into(), &iv.to_be_bytes().into());
    cipher.apply_keystream(buffer);
}

/// RFC 3711 section 4.3.1 with key derivation rate 0: the keystream of AES-CM
/// under the master key, from `(salt XOR label << 48) << 16`.
fn derive<const N: usize>(master_key: &[u8; 32], master_salt: &[u8; 14], label: u8) -> [u8; N] {
    let mut salt = [0u8; 16];
    salt[2..].copy_from_slice(master_salt);
    let x = u128::from_be_bytes(salt) ^ (u128::from(label) << 48);
    let mut out = [0u8; N];
    aes_cm(master_key, x << 16, &mut out);
    out
}

/// One direction's session keys (RTP's or RTCP's).
#[derive(Clone)]
struct Keys {
    encryption: [u8; 32],
    auth: [u8; 20],
    salt: [u8; 14],
}

impl Keys {
    fn derive(master_key: &[u8; 32], master_salt: &[u8; 14], labels: [u8; 3]) -> Self {
        Self {
            encryption: derive(master_key, master_salt, labels[0]),
            auth: derive(master_key, master_salt, labels[1]),
            salt: derive(master_key, master_salt, labels[2]),
        }
    }

    /// The tag over `data`, then `roc` if given (RTP's rollover counter).
    fn tag(&self, data: &[u8], roc: Option<u32>) -> [u8; TAG_LEN] {
        let mut mac = <HmacSha1 as KeyInit>::new_from_slice(&self.auth).expect("HMAC takes any key length");
        mac.update(data);
        if let Some(roc) = roc {
            mac.update(&roc.to_be_bytes());
        }
        let full = mac.finalize().into_bytes();
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&full[..TAG_LEN]);
        tag
    }

    /// The counter-mode IV for `ssrc` and a packet index (RFC 3711 section
    /// 4.1.1): `salt << 16 XOR ssrc << 64 XOR index << 16`.
    fn iv(&self, ssrc: u32, index: u64) -> u128 {
        let mut salt = [0u8; 16];
        salt[..14].copy_from_slice(&self.salt);
        u128::from_be_bytes(salt) ^ (u128::from(ssrc) << 64) ^ (u128::from(index) << 16)
    }
}

/// Constant-time equality of two tags.
fn tags_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The SRTP and SRTCP keys of one master key and salt.
#[derive(Clone)]
pub struct Context {
    rtp: Keys,
    rtcp: Keys,
}

impl Context {
    pub fn new(master_key: &[u8; MASTER_KEY_LEN], master_salt: &[u8; MASTER_SALT_LEN]) -> Self {
        use label::*;
        Self {
            rtp: Keys::derive(master_key, master_salt, [RTP_ENCRYPTION, RTP_AUTH, RTP_SALT]),
            rtcp: Keys::derive(master_key, master_salt, [RTCP_ENCRYPTION, RTCP_AUTH, RTCP_SALT]),
        }
    }

    /// The microphone stream's: the host's `_streamKey`, salted with the
    /// first 14 bytes of the session ID.
    pub fn for_stream(stream_key: &[u8; 32], session_id: &[u8; 16]) -> Self {
        let mut salt = [0u8; MASTER_SALT_LEN];
        salt.copy_from_slice(&session_id[..MASTER_SALT_LEN]);
        Self::new(stream_key, &salt)
    }

    /// Verifies an SRTP packet sent with rollover counter `roc`, and returns
    /// the RTP packet in the clear (without its tag).
    pub fn unprotect_rtp(&self, packet: &[u8], roc: u32) -> Result<Vec<u8>, SrtpError> {
        let header = rtp_header_len(packet).ok_or(SrtpError::Short("an RTP header"))?;
        if packet.len() < header + TAG_LEN {
            return Err(SrtpError::Short("an SRTP tag"));
        }
        let (body, tag) = packet.split_at(packet.len() - TAG_LEN);
        if !tags_equal(&self.rtp.tag(body, Some(roc)), tag) {
            return Err(SrtpError::Auth);
        }
        let seq = u16::from_be_bytes([packet[2], packet[3]]);
        let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
        let index = (u64::from(roc) << 16) | u64::from(seq);
        let mut clear = body.to_vec();
        aes_cm(&self.rtp.encryption, self.rtp.iv(ssrc, index), &mut clear[header..]);
        Ok(clear)
    }

    /// Protects a (compound) RTCP packet as SRTCP with `index` (31 bits):
    /// everything after its first 8 bytes encrypted, then `E | index` and
    /// the tag (RFC 3711 section 3.4).
    pub fn protect_rtcp(&self, rtcp: &[u8], index: u32) -> Vec<u8> {
        assert!(rtcp.len() >= 8, "an RTCP packet has an 8-byte header");
        let index = index & 0x7fff_ffff;
        let ssrc = u32::from_be_bytes([rtcp[4], rtcp[5], rtcp[6], rtcp[7]]);
        let mut out = rtcp.to_vec();
        aes_cm(&self.rtcp.encryption, self.rtcp.iv(ssrc, u64::from(index)), &mut out[8..]);
        out.extend_from_slice(&(0x8000_0000 | index).to_be_bytes());
        let tag = self.rtcp.tag(&out, None);
        out.extend_from_slice(&tag);
        out
    }

    /// Verifies an SRTCP packet, and returns the RTCP in the clear and its
    /// index.
    pub fn unprotect_rtcp(&self, packet: &[u8]) -> Result<(Vec<u8>, u32), SrtpError> {
        if packet.len() < 8 + 4 + TAG_LEN {
            return Err(SrtpError::Short("SRTCP"));
        }
        let (body, tag) = packet.split_at(packet.len() - TAG_LEN);
        if !tags_equal(&self.rtcp.tag(body, None), tag) {
            return Err(SrtpError::Auth);
        }
        let (rtcp, trailer) = body.split_at(body.len() - 4);
        let e_index = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
        let index = e_index & 0x7fff_ffff;
        let mut clear = rtcp.to_vec();
        if e_index >> 31 == 1 {
            let ssrc = u32::from_be_bytes([rtcp[4], rtcp[5], rtcp[6], rtcp[7]]);
            aes_cm(&self.rtcp.encryption, self.rtcp.iv(ssrc, u64::from(index)), &mut clear[8..]);
        }
        Ok((clear, index))
    }
}

/// The length of an RTP packet's header: fixed part, CSRCs and extension.
pub fn rtp_header_len(packet: &[u8]) -> Option<usize> {
    let first = *packet.first()?;
    if first >> 6 != 2 || packet.len() < 12 {
        return None;
    }
    let mut len = 12 + 4 * usize::from(first & 0x0f);
    if first & 0x10 != 0 {
        let words = u16::from_be_bytes([*packet.get(len + 2)?, *packet.get(len + 3)?]);
        len += 4 + 4 * usize::from(words);
    }
    (len <= packet.len()).then_some(len)
}

/// Whether a packet on a multiplexed RTP/RTCP port is RTCP (RFC 5761 section
/// 4: packet types 200 to 204 in the second byte).
pub fn is_rtcp(packet: &[u8]) -> bool {
    packet.len() >= 2 && packet[0] >> 6 == 2 && (200..=204).contains(&packet[1])
}

/// The receiver's packet index estimate (RFC 3711 section 3.3.1 and
/// appendix A): the rollover counter for each sequence number, kept from
/// the highest one authenticated.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rollover {
    /// The rollover counter and highest sequence number so far.
    latest: Option<(u32, u16)>,
}

impl Rollover {
    /// The rollover counter a packet with `seq` was most likely sent with.
    pub fn guess(&self, seq: u16) -> u32 {
        let Some((roc, s_l)) = self.latest else { return 0 };
        if s_l < 0x8000 {
            if seq > s_l && seq - s_l > 0x8000 { roc.wrapping_sub(1) } else { roc }
        } else if s_l - 0x8000 > seq {
            roc.wrapping_add(1)
        } else {
            roc
        }
    }

    /// Takes in an authenticated packet.
    pub fn update(&mut self, seq: u16, roc: u32) {
        let index = (u64::from(roc) << 16) | u64::from(seq);
        if self.latest.is_none_or(|(r, s)| index > (u64::from(r) << 16) | u64::from(s)) {
            self.latest = Some((roc, seq));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn array<const N: usize>(s: &str) -> [u8; N] {
        hex(s).try_into().unwrap()
    }

    /// RFC 6188 section 7.2: AES_256_CM_PRF key derivation.
    #[test]
    fn key_derivation_matches_rfc_6188() {
        let key = array("f0f04914b513f2763a1b1fa130f10e2998f6f6e43e4309d1e622a0e332b9f1b6");
        let salt = array("3b04803de51ee7c96423ab5b78d2");
        let k = Keys::derive(&key, &salt, [0, 1, 2]);
        assert_eq!(k.encryption.to_vec(), hex("5ba1064e30ec51613cad926c5a28ef731ec7fb397f70a960653caf06554cd8c4"));
        assert_eq!(k.salt.to_vec(), hex("fa31791685ca444a9e07c6c64e93"));
        assert_eq!(k.auth.to_vec(), hex("fd9c32d39ed5fbb5a9dc96b30818454d1313dc05"));
    }

    /// RFC 6188 section 7.1: the AES_256_CM keystream's first blocks.
    #[test]
    fn the_keystream_matches_rfc_6188() {
        let key = array("57f82fe3613fd170a85ec93c40b1f0922ec4cb0dc025b58272147cc438944a98");
        let mut stream = [0u8; 48];
        aes_cm(&key, u128::from_be_bytes(array("f0f1f2f3f4f5f6f7f8f9fafbfcfd0000")), &mut stream);
        let want = "92bdd28a93c3f52511c677d08b5515a49da71b2378a854f67050756ded165bac63c4868b7096d88421b563b8c94c9a31";
        assert_eq!(stream.to_vec(), hex(want));
    }

    /// The iPhone's own packets from Koala's trial 21, with the session's
    /// throwaway keys: the stream's first SRTP packet and its first SRTCP.
    fn trial_21() -> Context {
        let stream_key = array("97ac9e7f7ce0260f0b90f9549766be40605c45b769e73f896db4e588c8b61a41");
        Context::for_stream(&stream_key, &array("c185707500b14c6fa4352eab23a163b8"))
    }

    #[test]
    fn the_iphones_srtp_verifies() {
        let packet = hex("80e5468b00000000970b76abb95732ac277f49575fe6bcd0e49c4d5d6d0e3065397548046fcd7fa3035d");
        let clear = trial_21().unprotect_rtp(&packet, 0).unwrap();
        assert_eq!(&clear[..12], &packet[..12], "the header stays in the clear");
        assert_eq!(clear.len(), packet.len() - TAG_LEN);
        let mut tampered = packet.clone();
        tampered[20] ^= 1;
        assert_eq!(trial_21().unprotect_rtp(&tampered, 0), Err(SrtpError::Auth));
        assert_eq!(trial_21().unprotect_rtp(&packet, 1), Err(SrtpError::Auth), "the rollover counter is authenticated");
    }

    /// Its SRTCP: a sender report about Koala's SSRC and an SDES, index 1.
    #[test]
    fn the_iphones_srtcp_verifies_and_decrypts() {
        let packet = srtcp_from_trial_21();
        let (clear, index) = trial_21().unprotect_rtcp(&packet).unwrap();
        assert_eq!(index, 1);
        assert_eq!(&clear[..8], &hex("81c8000c970b76ab")[..], "SR, one report block, the iPhone's SSRC");
        assert_eq!(&clear[28..32], &hex("1ed21a64")[..], "about Koala's SSRC");
        assert_eq!(&clear[52..], &hex("81ca0002970b76ab01000000")[..], "an SDES with an empty CNAME");
    }

    /// The capture's first RTCP packet from the iPhone, whole: 78 bytes,
    /// ending `E | index` 0x80000001 and the tag.
    fn srtcp_from_trial_21() -> Vec<u8> {
        hex(concat!(
            "81c8000c970b76abdf527a9dc2f2342cd511904d9e03cd7c01f31dc69e87e28949167e0a854253e79bbd6c086a",
            "077962ddda205973d6c2e65f68210581f081cf800000012ebd0b3b4b355b7d4096",
        ))
    }

    #[test]
    fn srtcp_round_trips_and_refuses_tampering() {
        let c = Context::new(&[7; 32], &[9; 14]);
        let rtcp = hex("81c90007000000010000000200000000000000000000000000000000");
        let packet = c.protect_rtcp(&rtcp, 5);
        assert_eq!(packet.len(), rtcp.len() + 4 + TAG_LEN);
        assert_eq!(&packet[..8], &rtcp[..8], "the first 8 bytes stay in the clear");
        assert_ne!(&packet[8..rtcp.len()], &rtcp[8..], "the rest is encrypted");
        assert_eq!(c.unprotect_rtcp(&packet), Ok((rtcp.clone(), 5)));
        let mut tampered = packet.clone();
        tampered[9] ^= 1;
        assert_eq!(c.unprotect_rtcp(&tampered), Err(SrtpError::Auth));
    }

    #[test]
    fn rollover_follows_the_sequence() {
        let mut r = Rollover::default();
        assert_eq!(r.guess(65530), 0);
        r.update(65530, 0);
        assert_eq!(r.guess(3), 1, "wrapped forward");
        r.update(3, 1);
        assert_eq!(r.guess(65534), 0, "a late packet from before the wrap");
        assert_eq!(r.guess(10), 1);
    }

    #[test]
    fn rtcp_and_rtp_are_told_apart() {
        assert!(is_rtcp(&hex("81c8000c")));
        assert!(!is_rtcp(&hex("8065468c")));
        assert!(!is_rtcp(&hex("80e5468b")));
    }
}
