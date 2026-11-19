//! The session's streams, as the host serves them: answers to the iPhone's
//! `_streamStart` requests (Rapport's `RPStreamServer`; from tvOS 27.2,
//! `_serverRPConnectionStartRequest:` and `_serverUDPNWPathStartRequest:`).
//!
//! - **Type 2, a Rapport connection** (Control, Command, Data). The request
//!   carries the iPhone's X25519 public key; the host answers with its own
//!   and a TCP port. Both derive a pre-shared key, HKDF-SHA512 of the X25519
//!   secret with salt "Salt" and info "Info", which keys the connection
//!   the iPhone then opens, encrypted from its first frame.
//! - **Type 3, a UDP path** (the microphone's media). The host answers with
//!   its address, MAC address, UDP port and 32 random bytes, which with the
//!   session ID make the SRTP key.

use apple_opack::Value;
use koala_pairing::crypto::hkdf_sha512;
use x25519_dalek::{PublicKey, StaticSecret};

pub const STREAM_START: &str = "_streamStart";
pub const STREAM_ID: &str = "_streamID";
pub const STREAM_TYPE: &str = "_streamType";
pub const STREAM_KEY: &str = "_streamKey";
pub const STREAM_PORT: &str = "_streamPort";
pub const STREAM_ADDRESS: &str = "_streamAddr";
pub const STREAM_MAC: &str = "_streamMACAddr";

/// `_streamType` values.
pub const TYPE_RAPPORT: u64 = 2;
pub const TYPE_UDP: u64 = 3;

/// The host's half of a Rapport stream's key agreement: its public key, for
/// the answer, and the connection's pre-shared key.
pub fn rapport_stream_keys(client_public: &[u8; 32], random: [u8; 32]) -> ([u8; 32], [u8; 32]) {
    let secret = StaticSecret::from(random);
    let shared = secret.diffie_hellman(&PublicKey::from(*client_public));
    let psk = hkdf_sha512(shared.as_bytes(), b"Salt", b"Info");
    (PublicKey::from(&secret).to_bytes(), psk)
}

/// The answer to a Rapport stream's start.
pub fn rapport_stream_answer(host_public: &[u8; 32], port: u16) -> Value {
    dict(vec![(STREAM_KEY, Value::Bytes(host_public.to_vec())), (STREAM_PORT, Value::int(u64::from(port)))])
}

/// The answer to a UDP stream's start. `address` is the host's link-local
/// address, unscoped: the iPhone appends its own interface (`%llw0`, its
/// low-latency WLAN interface over AWDL) and refuses an address that already
/// has one, `kMalformedErr (Bad peer address string 'fe80::…%awdl0%llw0')`
/// (trial 13). Its own request's address is bare too (a ULA there).
pub fn udp_stream_answer(address: &str, mac: [u8; 6], key: &[u8; 32], port: u16) -> Value {
    dict(vec![
        (STREAM_ADDRESS, address.into()),
        (STREAM_MAC, Value::Bytes(mac.to_vec())),
        (STREAM_KEY, Value::Bytes(key.to_vec())),
        (STREAM_PORT, Value::int(u64::from(port))),
    ])
}

/// The SRTP key both ways (cipher suite 5): the host's 32 random bytes,
/// then the first 14 bytes of the session ID.
pub fn srtp_key(stream_key: &[u8; 32], session_id: &[u8; 16]) -> [u8; 46] {
    let mut key = [0u8; 46];
    key[..32].copy_from_slice(stream_key);
    key[32..].copy_from_slice(&session_id[..14]);
    key
}

fn dict(pairs: Vec<(&str, Value)>) -> Value {
    Value::Dict(pairs.into_iter().map(|(k, v)| (Value::from(k), v)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_ends_derive_the_same_stream_key() {
        // The iPhone's side, for the test: its secret, and the same HKDF.
        let client = StaticSecret::from([3; 32]);
        let client_public = PublicKey::from(&client).to_bytes();
        let (host_public, psk) = rapport_stream_keys(&client_public, [5; 32]);
        let shared = client.diffie_hellman(&PublicKey::from(host_public));
        assert_eq!(psk, hkdf_sha512(shared.as_bytes(), b"Salt", b"Info"));
    }

    #[test]
    fn the_srtp_key_is_the_stream_key_then_the_session_id() {
        let key = srtp_key(&[1; 32], &[2; 16]);
        assert_eq!(&key[..32], &[1; 32]);
        assert_eq!(&key[32..], &[2; 14]);
    }
}
