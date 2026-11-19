//! Rapport's frames: a type byte, a 24-bit big-endian length, the payload.

/// A frame's type. The known ones are constants; others pass through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameType(pub u8);

impl FrameType {
    pub const NO_OP: Self = Self(1);
    /// Pair-setup: the client's first message.
    pub const PS_START: Self = Self(3);
    /// Pair-setup: every later message, both ways.
    pub const PS_NEXT: Self = Self(4);
    /// Pair-verify: the client's first message.
    pub const PV_START: Self = Self(5);
    /// Pair-verify: every later message, both ways.
    pub const PV_NEXT: Self = Self(6);
    /// An unencrypted OPACK message.
    pub const U_OPACK: Self = Self(7);
    /// An encrypted OPACK message: requests, responses and events.
    pub const E_OPACK: Self = Self(8);
    pub const P_OPACK: Self = Self(9);
    pub const PA_REQ: Self = Self(10);
    pub const PA_RSP: Self = Self(11);
    pub const SESSION_START_REQUEST: Self = Self(16);
    pub const SESSION_START_RESPONSE: Self = Self(17);
    pub const SESSION_DATA: Self = Self(18);
    pub const FAMILY_IDENTITY_REQUEST: Self = Self(32);
    pub const FAMILY_IDENTITY_RESPONSE: Self = Self(33);
    pub const FAMILY_IDENTITY_UPDATE: Self = Self(34);

    /// Whether this frame carries pairing (setup or verify).
    pub fn is_pairing(self) -> bool {
        matches!(self, Self::PS_START | Self::PS_NEXT | Self::PV_START | Self::PV_NEXT)
    }
}

/// The header's length.
pub const HEADER_LEN: usize = 4;
/// The longest payload a header can describe.
pub const MAX_PAYLOAD: usize = 0xff_ffff;

/// A frame's header, for a payload of `len` bytes (as sent: an encrypted
/// payload's length includes its tag).
pub fn header(kind: FrameType, len: usize) -> [u8; HEADER_LEN] {
    assert!(len <= MAX_PAYLOAD, "a Rapport frame holds at most {MAX_PAYLOAD} bytes");
    let len = (len as u32).to_be_bytes();
    [kind.0, len[1], len[2], len[3]]
}

/// The payload length a header announces.
pub fn payload_len(header: &[u8; HEADER_LEN]) -> usize {
    u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_type_then_big_endian_length() {
        let h = header(FrameType::E_OPACK, 0x01_02_03);
        assert_eq!(h, [8, 1, 2, 3]);
        assert_eq!(payload_len(&h), 0x01_02_03);
    }

    #[test]
    #[should_panic]
    fn a_payload_too_long_for_24_bits_is_refused() {
        header(FrameType::E_OPACK, MAX_PAYLOAD + 1);
    }
}
