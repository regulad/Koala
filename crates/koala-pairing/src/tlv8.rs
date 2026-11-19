//! TLV8: the type-length-value encoding pairing messages are made of.
//!
//! Each item is a type byte, a length byte and up to 255 bytes of value. A
//! longer value is split into consecutive items of the same type, which a
//! reader joins again. Two items of the same type in a row are therefore one
//! value; a separator item (type 0xff, empty) keeps two values apart, as in a
//! list of pairings.

/// The item types pairing uses (HomeKit Accessory Protocol, "TLV Values";
/// pyatv's `TlvValue` names the same numbers).
pub mod kind {
    pub const METHOD: u8 = 0x00;
    pub const IDENTIFIER: u8 = 0x01;
    pub const SALT: u8 = 0x02;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const PROOF: u8 = 0x04;
    pub const ENCRYPTED_DATA: u8 = 0x05;
    pub const STATE: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    pub const RETRY_DELAY: u8 = 0x08;
    pub const CERTIFICATE: u8 = 0x09;
    pub const SIGNATURE: u8 = 0x0a;
    pub const PERMISSIONS: u8 = 0x0b;
    pub const FRAGMENT_DATA: u8 = 0x0c;
    pub const FRAGMENT_LAST: u8 = 0x0d;
    /// Extra information about a device. Apple's CoreUtils sends an OPACK
    /// dictionary here (name, model, IRK, …); pyatv calls it `Name`.
    pub const INFO: u8 = 0x11;
    pub const FLAGS: u8 = 0x13;
    pub const SEPARATOR: u8 = 0xff;
}

/// The error codes a peer puts in an [`kind::ERROR`] item.
pub mod error {
    pub const UNKNOWN: u8 = 0x01;
    pub const AUTHENTICATION: u8 = 0x02;
    pub const BACKOFF: u8 = 0x03;
    pub const MAX_PEERS: u8 = 0x04;
    pub const MAX_TRIES: u8 = 0x05;
    pub const UNAVAILABLE: u8 = 0x06;
    pub const BUSY: u8 = 0x07;
}

/// Encodes `items`, in order, splitting long values.
pub fn encode(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(kind, value) in items {
        if value.is_empty() {
            out.extend_from_slice(&[kind, 0]);
            continue;
        }
        for chunk in value.chunks(255) {
            out.push(kind);
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
    }
    out
}

/// A decoded message: its items in order, with split values joined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tlv8 {
    items: Vec<(u8, Vec<u8>)>,
}

impl Tlv8 {
    /// Decodes `bytes`. Fails on an item that runs past the end.
    pub fn decode(bytes: &[u8]) -> Result<Self, Truncated> {
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let [kind, len, tail @ ..] = rest else { return Err(Truncated) };
            let len = *len as usize;
            if tail.len() < len {
                return Err(Truncated);
            }
            let (value, tail) = tail.split_at(len);
            match items.last_mut() {
                // The continuation of a value split at 255 bytes.
                Some((last, joined)) if *last == *kind && *kind != kind::SEPARATOR => joined.extend_from_slice(value),
                _ => items.push((*kind, value.to_vec())),
            }
            rest = tail;
        }
        Ok(Self { items })
    }

    /// The first value of type `kind`.
    pub fn get(&self, kind: u8) -> Option<&[u8]> {
        self.items.iter().find(|(k, _)| *k == kind).map(|(_, v)| v.as_slice())
    }

    /// The first value of type `kind`, as one byte.
    pub fn byte(&self, kind: u8) -> Option<u8> {
        match self.get(kind) {
            Some([b]) => Some(*b),
            _ => None,
        }
    }

    /// The first value of type `kind` as a little-endian integer of up to 8
    /// bytes (HAP's flags and retry delays).
    pub fn uint(&self, kind: u8) -> Option<u64> {
        let value = self.get(kind)?;
        if value.len() > 8 {
            return None;
        }
        Some(value.iter().rev().fold(0, |n, &b| (n << 8) | u64::from(b)))
    }

    /// Every item, in order.
    pub fn items(&self) -> impl Iterator<Item = (u8, &[u8])> {
        self.items.iter().map(|(k, v)| (*k, v.as_slice()))
    }
}

/// A TLV8 item ran past the end of its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a TLV8 item runs past the end of the message")]
pub struct Truncated;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_values_are_one_item_each() {
        let bytes = encode(&[(kind::STATE, &[1]), (kind::METHOD, &[0])]);
        assert_eq!(bytes, [0x06, 1, 1, 0x00, 1, 0]);
        let tlv = Tlv8::decode(&bytes).unwrap();
        assert_eq!(tlv.byte(kind::STATE), Some(1));
        assert_eq!(tlv.byte(kind::METHOD), Some(0));
    }

    #[test]
    fn a_long_value_is_split_and_joined_again() {
        let key: Vec<u8> = (0..384u32).map(|i| i as u8).collect();
        let bytes = encode(&[(kind::PUBLIC_KEY, &key), (kind::STATE, &[3])]);
        // 255 + 129, each with its own header, then the state.
        assert_eq!(bytes.len(), 2 + 255 + 2 + 129 + 3);
        assert_eq!(&bytes[..2], &[kind::PUBLIC_KEY, 255]);
        assert_eq!(&bytes[257..259], &[kind::PUBLIC_KEY, 129]);
        let tlv = Tlv8::decode(&bytes).unwrap();
        assert_eq!(tlv.get(kind::PUBLIC_KEY), Some(key.as_slice()));
        assert_eq!(tlv.byte(kind::STATE), Some(3));
    }

    #[test]
    fn an_exact_multiple_of_255_needs_no_empty_tail() {
        let value = [7u8; 510];
        let bytes = encode(&[(kind::CERTIFICATE, &value)]);
        assert_eq!(bytes.len(), 2 * (2 + 255));
        assert_eq!(Tlv8::decode(&bytes).unwrap().get(kind::CERTIFICATE), Some(&value[..]));
    }

    #[test]
    fn separators_keep_values_apart() {
        let bytes = encode(&[(kind::IDENTIFIER, b"a"), (kind::SEPARATOR, &[]), (kind::IDENTIFIER, b"b")]);
        let tlv = Tlv8::decode(&bytes).unwrap();
        let ids: Vec<&[u8]> = tlv.items().filter(|(k, _)| *k == kind::IDENTIFIER).map(|(_, v)| v).collect();
        assert_eq!(ids, [b"a", b"b"]);
    }

    #[test]
    fn integers_are_little_endian() {
        let tlv = Tlv8::decode(&encode(&[(kind::FLAGS, &[0x10, 0x00, 0x00, 0x01])])).unwrap();
        assert_eq!(tlv.uint(kind::FLAGS), Some(0x0100_0010));
    }

    #[test]
    fn a_truncated_item_is_an_error() {
        assert_eq!(Tlv8::decode(&[kind::SALT, 16, 1, 2]), Err(Truncated));
        assert_eq!(Tlv8::decode(&[kind::SALT]), Err(Truncated));
    }
}
