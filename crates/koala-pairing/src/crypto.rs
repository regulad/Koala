//! The key derivation and authenticated encryption pairing messages use.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use hkdf::Hkdf;
use sha2::Sha512;

/// HKDF-SHA512 of `ikm`, 32 bytes long.
pub fn hkdf_sha512(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0; 32];
    Hkdf::<Sha512>::new(Some(salt), ikm).expand(info, &mut out).expect("32 bytes is a valid HKDF-SHA512 length");
    out
}

/// ChaCha20-Poly1305 with a nonce made of a message label: four zero bytes,
/// then eight ASCII bytes such as `PS-Msg05`.
pub fn seal(key: &[u8; 32], label: &[u8; 8], plaintext: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(&(*key).into())
        .encrypt(&nonce(label).into(), plaintext)
        .expect("ChaCha20-Poly1305 encrypts any message this short")
}

/// The opposite of [`seal`]; `None` when the tag doesn't verify.
pub fn open(key: &[u8; 32], label: &[u8; 8], ciphertext: &[u8]) -> Option<Vec<u8>> {
    ChaCha20Poly1305::new(&(*key).into()).decrypt(&nonce(label).into(), ciphertext).ok()
}

fn nonce(label: &[u8; 8]) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[4..].copy_from_slice(label);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_messages_open_only_with_their_key_and_label() {
        let key = [7; 32];
        let sealed = seal(&key, b"PS-Msg05", b"hello");
        assert_eq!(sealed.len(), 5 + 16);
        assert_eq!(open(&key, b"PS-Msg05", &sealed).as_deref(), Some(&b"hello"[..]));
        assert_eq!(open(&key, b"PS-Msg06", &sealed), None);
        assert_eq!(open(&[8; 32], b"PS-Msg05", &sealed), None);
    }

    /// RFC 5869 has no SHA-512 vectors. The expected value is RFC 5869's
    /// extract-then-expand written out with Python's `hmac` and `hashlib`:
    /// `prk = HMAC(salt, ikm)`, `okm = HMAC(prk, info | 0x01)[..32]`.
    #[test]
    fn hkdf_matches_a_reference() {
        let okm = hkdf_sha512(b"input key material", b"Pair-Setup-Encrypt-Salt", b"Pair-Setup-Encrypt-Info");
        let expected = "61109666a2bef95244246a6c23115714fa3f188caf2794a561b78dca6a2e1b64";
        let hex: String = okm.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, expected);
    }
}
