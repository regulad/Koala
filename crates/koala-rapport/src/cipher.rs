//! The encryption a connection switches on after pair-verify.
//!
//! Each direction has its own ChaCha20-Poly1305 key, HKDF-SHA512 of the
//! pair-verify shared secret with an empty salt and the info
//! `ClientEncrypt-main` (client to server) or `ServerEncrypt-main`. The nonce
//! is a per-direction message counter, 12 bytes little-endian, from 0. The
//! frame's header, whose length counts the 16-byte tag, is the associated
//! data. Empty payloads stay unencrypted. (pyatv's `companion/connection.py`
//! and `protocol.py`.)

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use koala_pairing::crypto::hkdf_sha512;

use crate::frame::{FrameType, HEADER_LEN, header};

/// The tag each encrypted payload carries.
pub const TAG_LEN: usize = 16;

/// Which end of the connection we are: the keys are mirrored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Client,
    Server,
}

/// A connection's two keys and counters.
pub struct Cipher {
    send: ChaCha20Poly1305,
    receive: ChaCha20Poly1305,
    sent: u64,
    received: u64,
}

impl Cipher {
    /// The cipher for our `role`, from pair-verify's shared secret.
    pub fn new(shared_secret: &[u8; 32], role: Role) -> Self {
        let client = hkdf_sha512(shared_secret, b"", b"ClientEncrypt-main");
        let server = hkdf_sha512(shared_secret, b"", b"ServerEncrypt-main");
        let (send, receive) = match role {
            Role::Client => (client, server),
            Role::Server => (server, client),
        };
        Self {
            send: ChaCha20Poly1305::new(&send.into()),
            receive: ChaCha20Poly1305::new(&receive.into()),
            sent: 0,
            received: 0,
        }
    }

    /// Seals `payload` as a frame of type `kind`, header included.
    pub fn seal(&mut self, kind: FrameType, payload: &[u8]) -> Vec<u8> {
        if payload.is_empty() {
            return header(kind, 0).to_vec();
        }
        let header = header(kind, payload.len() + TAG_LEN);
        let nonce = counter_nonce(self.sent);
        self.sent += 1;
        let sealed = self
            .send
            .encrypt(&nonce.into(), Payload { msg: payload, aad: &header })
            .expect("ChaCha20-Poly1305 encrypts any frame");
        [&header[..], &sealed].concat()
    }

    /// Opens a received frame's payload. Fails when the tag doesn't verify:
    /// the connection can't go on after that.
    pub fn open(&mut self, header: &[u8; HEADER_LEN], payload: &[u8]) -> Result<Vec<u8>, BadTag> {
        if payload.is_empty() {
            return Ok(Vec::new());
        }
        let nonce = counter_nonce(self.received);
        self.received += 1;
        self.receive.decrypt(&nonce.into(), Payload { msg: payload, aad: header }).map_err(|_| BadTag)
    }
}

/// A received frame didn't decrypt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a Rapport frame didn't decrypt")]
pub struct BadTag;

fn counter_nonce(counter: u64) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[..8].copy_from_slice(&counter.to_le_bytes());
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::payload_len;

    #[test]
    fn each_side_opens_what_the_other_seals() {
        let secret = [9; 32];
        let (mut client, mut server) = (Cipher::new(&secret, Role::Client), Cipher::new(&secret, Role::Server));
        for message in [&b"first"[..], b"second"] {
            let frame = client.seal(FrameType::E_OPACK, message);
            let header: [u8; 4] = frame[..4].try_into().unwrap();
            assert_eq!(payload_len(&header), message.len() + TAG_LEN);
            assert_eq!(server.open(&header, &frame[4..]).unwrap(), message);
        }
        let frame = server.seal(FrameType::E_OPACK, b"reply");
        assert_eq!(client.open(&frame[..4].try_into().unwrap(), &frame[4..]).unwrap(), b"reply");
    }

    #[test]
    fn a_frame_out_of_order_fails() {
        let secret = [9; 32];
        let (mut client, mut server) = (Cipher::new(&secret, Role::Client), Cipher::new(&secret, Role::Server));
        let _first = client.seal(FrameType::E_OPACK, b"first");
        let second = client.seal(FrameType::E_OPACK, b"second");
        assert_eq!(server.open(&second[..4].try_into().unwrap(), &second[4..]), Err(BadTag));
    }

    #[test]
    fn empty_payloads_stay_empty() {
        let mut client = Cipher::new(&[1; 32], Role::Client);
        assert_eq!(client.seal(FrameType::NO_OP, b""), [1, 0, 0, 0]);
    }
}
