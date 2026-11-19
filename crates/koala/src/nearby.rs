//! The Continuity Nearby Action advert that asks a paired iPhone onto AWDL
//! (`docs/guest-pairing.md`, "Phase B"; worked out from iOS 27.2.1's
//! Sharing and Rapport code, 2026-11-19).
//!
//! After Apple's company ID the manufacturer data is `0F 05 A0 32 T0 T1 T2`:
//! one Nearby Action (type 0x0F, length 5) with flags 0xA0 (0x80: an auth
//! tag follows; 0x20: needs keyboard, as a tvOS host sends), action type 0x32
//! (RDLink needs AWDL), and a 3-byte auth tag. The iPhone identifies the
//! sender by trying its paired identities' IRKs on the tag; Koala's is the
//! `altIRK` it sent in pair-setup's M6.

/// The auth tag: SipHash-2-4 keyed with the IRK, over the 6-byte address the
/// advert is sent from (most significant byte first), and of the 64-bit
/// result the 3 bytes below its top two, big-endian.
pub fn auth_tag(irk: &[u8; 16], address: &[u8; 6]) -> [u8; 3] {
    let h = siphash24(irk, address).to_be_bytes();
    [h[2], h[3], h[4]]
}

/// The whole advert, after the company ID.
pub fn needs_awdl(irk: &[u8; 16], address: &[u8; 6]) -> Vec<u8> {
    let tag = auth_tag(irk, address);
    vec![0x0f, 0x05, 0xa0, 0x32, tag[0], tag[1], tag[2]]
}

/// SipHash-2-4 (Aumasson and Bernstein), with a 128-bit key.
pub fn siphash24(key: &[u8; 16], message: &[u8]) -> u64 {
    let k0 = u64::from_le_bytes(key[..8].try_into().unwrap());
    let k1 = u64::from_le_bytes(key[8..].try_into().unwrap());
    let mut v = [
        k0 ^ 0x736f_6d65_7073_6575,
        k1 ^ 0x646f_7261_6e64_6f6d,
        k0 ^ 0x6c79_6765_6e65_7261,
        k1 ^ 0x7465_6462_7974_6573,
    ];
    fn round(v: &mut [u64; 4]) {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }
    let compress = |m: u64, v: &mut [u64; 4]| {
        v[3] ^= m;
        round(v);
        round(v);
        v[0] ^= m;
    };
    let (chunks, tail) = message.as_chunks::<8>();
    for chunk in chunks {
        compress(u64::from_le_bytes(*chunk), &mut v);
    }
    let mut last = (message.len() as u64 & 0xff) << 56;
    for (i, &b) in tail.iter().enumerate() {
        last |= u64::from(b) << (8 * i);
    }
    compress(last, &mut v);
    v[2] ^= 0xff;
    for _ in 0..4 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    /// The reference implementation's vectors (key 00..0f, message 00..n-1).
    #[test]
    fn siphash_matches_its_reference_vectors() {
        assert_eq!(siphash24(&KEY, b""), 0x726f_db47_dd0e_0e31);
        let fifteen: Vec<u8> = (0..15).collect();
        assert_eq!(siphash24(&KEY, &fifteen), 0xa129_ca61_49be_45e5);
    }

    /// The worked example from the reverse engineering (IRK 00..0f,
    /// address c0:11:22:33:44:55).
    #[test]
    fn the_advert_carries_the_tag() {
        let address = [0xc0, 0x11, 0x22, 0x33, 0x44, 0x55];
        assert_eq!(auth_tag(&KEY, &address), [0x37, 0xb4, 0x0c]);
        assert_eq!(needs_awdl(&KEY, &address), [0x0f, 0x05, 0xa0, 0x32, 0x37, 0xb4, 0x0c]);
    }
}
