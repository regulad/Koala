//! Pair-setup: two devices that share a setup code exchange long-term
//! identities (HomeKit Accessory Protocol, "Pair Setup"; Apple's CoreUtils
//! runs the same exchange for Rapport and AirPlay).
//!
//! | Message | From | Carries |
//! |---|---|---|
//! | M1 | client | method, flags |
//! | M2 | server | salt, SRP `B` |
//! | M3 | client | SRP `A`, proof `M1` |
//! | M4 | server | proof `M2` |
//! | M5 | client | its identity, signed, sealed with the SRP key |
//! | M6 | server | its identity, signed, sealed with the SRP key |
//!
//! A *transient* pairing (flag 0x10) stops after M4: the two share a session
//! key and no identities.

use crate::crypto::{hkdf_sha512, open, seal};
use crate::identity::{Identity, Peer, verify};
use crate::srp::{SrpClient, SrpServer};
use crate::tlv8::{Tlv8, encode, error, kind};
use crate::{PairingError, Refusal, Step};

/// The method an M1 asks for: pair-setup.
pub const METHOD_PAIR_SETUP: u8 = 0;
/// M1 flag: a transient pairing, which stops after M4.
pub const FLAG_TRANSIENT: u64 = 0x10;

/// What pair-setup leaves the two sides with.
#[derive(Debug)]
pub struct Paired {
    /// The peer's identity; `None` after a transient pairing.
    pub peer: Option<Peer>,
    /// The SRP session key `K` (64 bytes), from which a transient pairing's
    /// transport keys are derived.
    pub session_key: Vec<u8>,
}

/// The server (accessory) side, which holds the setup code.
pub struct SetupServer {
    identity: Identity,
    code: Vec<u8>,
    m2_extra: Vec<(u8, Vec<u8>)>,
    state: ServerState,
}

enum ServerState {
    AwaitingM1,
    AwaitingM3 { srp: SrpServer, transient: bool },
    AwaitingM5 { key: Vec<u8> },
    Finished,
}

impl SetupServer {
    pub fn new(identity: Identity, code: impl Into<Vec<u8>>) -> Self {
        Self { identity, code: code.into(), m2_extra: Vec::new(), state: ServerState::AwaitingM1 }
    }

    /// Adds `items` to M2, after the salt and `B`. (pyatv's stand-in for an
    /// Apple TV's Rapport server sends item 27 = 1 there, as Apple TVs do.)
    pub fn with_m2_items(mut self, items: Vec<(u8, Vec<u8>)>) -> Self {
        self.m2_extra = items;
        self
    }

    /// Handles the client's next message.
    pub fn handle(&mut self, message: &[u8]) -> Result<Step<Paired>, Refusal> {
        let tlv = Tlv8::decode(message).map_err(|_| refuse(PairingError::Malformed("TLV8"), 0))?;
        match std::mem::replace(&mut self.state, ServerState::Finished) {
            ServerState::AwaitingM1 => {
                expect_state(&tlv, 1).map_err(|e| refuse(e, 2))?;
                if let Some(method) = tlv.byte(kind::METHOD)
                    && method != METHOD_PAIR_SETUP
                {
                    return Err(refuse(PairingError::Malformed("method"), 2));
                }
                let transient = tlv.uint(kind::FLAGS).unwrap_or(0) & FLAG_TRANSIENT != 0;
                let srp = SrpServer::new(&self.code);
                let mut items: Vec<(u8, &[u8])> =
                    vec![(kind::STATE, &[2]), (kind::SALT, srp.salt()), (kind::PUBLIC_KEY, srp.public_key())];
                items.extend(self.m2_extra.iter().map(|(k, v)| (*k, v.as_slice())));
                let m2 = encode(&items);
                self.state = ServerState::AwaitingM3 { srp, transient };
                Ok(Step::Send(m2))
            }
            ServerState::AwaitingM3 { srp, transient } => {
                expect_state(&tlv, 3).map_err(|e| refuse(e, 4))?;
                let a_pub = tlv.get(kind::PUBLIC_KEY).ok_or_else(|| refuse(PairingError::Malformed("A"), 4))?;
                let proof = tlv.get(kind::PROOF).ok_or_else(|| refuse(PairingError::Malformed("proof"), 4))?;
                let (m2, key) = srp.verify(a_pub, proof).map_err(|e| refuse(PairingError::Srp(e), 4))?;
                let m4 = encode(&[(kind::STATE, &[4]), (kind::PROOF, &m2)]);
                if transient {
                    return Ok(Step::Done { send: Some(m4), result: Paired { peer: None, session_key: key } });
                }
                self.state = ServerState::AwaitingM5 { key };
                Ok(Step::Send(m4))
            }
            ServerState::AwaitingM5 { key } => {
                expect_state(&tlv, 5).map_err(|e| refuse(e, 6))?;
                let peer = open_identity(&tlv, &key, b"PS-Msg05", Role::Client).map_err(|e| refuse(e, 6))?;
                let sealed = seal_identity(&self.identity, &key, b"PS-Msg06", Role::Server);
                let m6 = encode(&[(kind::STATE, &[6]), (kind::ENCRYPTED_DATA, &sealed)]);
                Ok(Step::Done { send: Some(m6), result: Paired { peer: Some(peer), session_key: key } })
            }
            ServerState::Finished => Err(Refusal { error: PairingError::Finished, reply: None }),
        }
    }
}

/// The client (controller) side, which was given the setup code.
pub struct SetupClient {
    identity: Identity,
    code: Vec<u8>,
    flags: u64,
    state: ClientState,
}

enum ClientState {
    Start,
    AwaitingM2,
    AwaitingM4 { expected: Vec<u8>, key: Vec<u8> },
    AwaitingM6 { key: Vec<u8> },
    Finished,
}

impl SetupClient {
    pub fn new(identity: Identity, code: impl Into<Vec<u8>>) -> Self {
        Self { identity, code: code.into(), flags: 0, state: ClientState::Start }
    }

    /// Sends `flags` in M1 ([`FLAG_TRANSIENT`], …).
    pub fn with_flags(mut self, flags: u64) -> Self {
        self.flags = flags;
        self
    }

    /// M1, the first message.
    pub fn start(&mut self) -> Vec<u8> {
        self.state = ClientState::AwaitingM2;
        let flags = self.flags.to_le_bytes();
        let used = 8 - self.flags.leading_zeros() as usize / 8;
        let mut items: Vec<(u8, &[u8])> = vec![(kind::METHOD, &[METHOD_PAIR_SETUP]), (kind::STATE, &[1])];
        if self.flags != 0 {
            items.push((kind::FLAGS, &flags[..used]));
        }
        encode(&items)
    }

    /// Handles the server's next message.
    pub fn handle(&mut self, message: &[u8]) -> Result<Step<Paired>, PairingError> {
        let tlv = Tlv8::decode(message).map_err(|_| PairingError::Malformed("TLV8"))?;
        match std::mem::replace(&mut self.state, ClientState::Finished) {
            ClientState::AwaitingM2 => {
                expect_state(&tlv, 2)?;
                let salt = tlv.get(kind::SALT).ok_or(PairingError::Malformed("salt"))?;
                let b_pub = tlv.get(kind::PUBLIC_KEY).ok_or(PairingError::Malformed("B"))?;
                let proof = SrpClient::new().respond(&self.code, salt, b_pub)?;
                let m3 =
                    encode(&[(kind::STATE, &[3]), (kind::PUBLIC_KEY, &proof.public_key), (kind::PROOF, &proof.proof)]);
                self.state = ClientState::AwaitingM4 { expected: proof.expected_server_proof, key: proof.key };
                Ok(Step::Send(m3))
            }
            ClientState::AwaitingM4 { expected, key } => {
                expect_state(&tlv, 4)?;
                if tlv.get(kind::PROOF) != Some(expected.as_slice()) {
                    return Err(PairingError::Srp(crate::srp::SrpError::WrongCode));
                }
                if self.flags & FLAG_TRANSIENT != 0 {
                    return Ok(Step::Done { send: None, result: Paired { peer: None, session_key: key } });
                }
                let sealed = seal_identity(&self.identity, &key, b"PS-Msg05", Role::Client);
                let m5 = encode(&[(kind::STATE, &[5]), (kind::ENCRYPTED_DATA, &sealed)]);
                self.state = ClientState::AwaitingM6 { key };
                Ok(Step::Send(m5))
            }
            ClientState::AwaitingM6 { key } => {
                expect_state(&tlv, 6)?;
                let peer = open_identity(&tlv, &key, b"PS-Msg06", Role::Server)?;
                Ok(Step::Done { send: None, result: Paired { peer: Some(peer), session_key: key } })
            }
            ClientState::Start | ClientState::Finished => Err(PairingError::Finished),
        }
    }
}

/// Which side signs: the HKDF labels differ.
#[derive(Clone, Copy)]
enum Role {
    Client,
    Server,
}

impl Role {
    fn sign_labels(self) -> (&'static [u8], &'static [u8]) {
        match self {
            Role::Client => (b"Pair-Setup-Controller-Sign-Salt", b"Pair-Setup-Controller-Sign-Info"),
            Role::Server => (b"Pair-Setup-Accessory-Sign-Salt", b"Pair-Setup-Accessory-Sign-Info"),
        }
    }
}

fn encryption_key(session_key: &[u8]) -> [u8; 32] {
    hkdf_sha512(session_key, b"Pair-Setup-Encrypt-Salt", b"Pair-Setup-Encrypt-Info")
}

/// What a side signs: `HKDF(K, role labels) | id | long-term public key`.
fn signed_info(session_key: &[u8], role: Role, id: &[u8], public_key: &[u8; 32]) -> Vec<u8> {
    let (salt, info) = role.sign_labels();
    let mut message = hkdf_sha512(session_key, salt, info).to_vec();
    message.extend_from_slice(id);
    message.extend_from_slice(public_key);
    message
}

/// M5's or M6's sealed identity.
fn seal_identity(identity: &Identity, session_key: &[u8], label: &[u8; 8], role: Role) -> Vec<u8> {
    let public_key = identity.public_key();
    let signature = identity.sign(&signed_info(session_key, role, identity.id(), &public_key));
    let mut items: Vec<(u8, &[u8])> =
        vec![(kind::IDENTIFIER, identity.id()), (kind::PUBLIC_KEY, &public_key), (kind::SIGNATURE, &signature)];
    if let Some(info) = identity.info() {
        items.push((kind::INFO, info));
    }
    seal(&encryption_key(session_key), label, &encode(&items))
}

/// Opens and checks M5's or M6's identity.
fn open_identity(tlv: &Tlv8, session_key: &[u8], label: &[u8; 8], role: Role) -> Result<Peer, PairingError> {
    let sealed = tlv.get(kind::ENCRYPTED_DATA).ok_or(PairingError::Malformed("encrypted data"))?;
    let plain = open(&encryption_key(session_key), label, sealed).ok_or(PairingError::Decryption)?;
    let inner = Tlv8::decode(&plain).map_err(|_| PairingError::Malformed("sealed TLV8"))?;
    let id = inner.get(kind::IDENTIFIER).ok_or(PairingError::Malformed("identifier"))?;
    let public_key: [u8; 32] = inner
        .get(kind::PUBLIC_KEY)
        .and_then(|k| k.try_into().ok())
        .ok_or(PairingError::Malformed("long-term public key"))?;
    let signature = inner.get(kind::SIGNATURE).ok_or(PairingError::Malformed("signature"))?;
    if !verify(&public_key, &signed_info(session_key, role, id, &public_key), signature) {
        return Err(PairingError::Signature);
    }
    Ok(Peer { id: id.to_vec(), public_key, info: inner.get(kind::INFO).map(<[u8]>::to_vec) })
}

/// Checks a message's state, and turns a peer's error item into an error.
pub(crate) fn expect_state(tlv: &Tlv8, expected: u8) -> Result<(), PairingError> {
    if let Some(code) = tlv.byte(kind::ERROR) {
        return Err(PairingError::Peer(code));
    }
    match tlv.byte(kind::STATE) {
        Some(state) if state == expected => Ok(()),
        got => Err(PairingError::UnexpectedState { expected, got }),
    }
}

/// A server's refusal: the error, and the error message to send, in the state
/// the client expects next.
pub(crate) fn refuse(error: PairingError, reply_state: u8) -> Refusal {
    let code = match error {
        PairingError::Srp(_) | PairingError::Decryption | PairingError::Signature | PairingError::UnknownPeer => {
            error::AUTHENTICATION
        }
        _ => error::UNKNOWN,
    };
    let reply = (reply_state != 0).then(|| encode(&[(kind::STATE, &[reply_state]), (kind::ERROR, &[code])]));
    Refusal { error, reply }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mut client: SetupClient, mut server: SetupServer) -> (Result<Paired, PairingError>, Option<Paired>) {
        let mut to_server = client.start();
        loop {
            let reply = match server.handle(&to_server) {
                Ok(Step::Send(reply)) => reply,
                Ok(Step::Done { send, result }) => {
                    let client_result = match client.handle(&send.unwrap()) {
                        Ok(Step::Done { result, .. }) => Ok(result),
                        Ok(Step::Send(_)) => panic!("the client wants to go on"),
                        Err(e) => Err(e),
                    };
                    return (client_result, Some(result));
                }
                Err(refusal) => {
                    let client_result = match refusal.reply {
                        Some(reply) => client.handle(&reply).map(|_| unreachable!()),
                        None => Err(refusal.error),
                    };
                    return (client_result, None);
                }
            };
            match client.handle(&reply) {
                Ok(Step::Send(next)) => to_server = next,
                Ok(Step::Done { .. }) => panic!("the client finished before the server"),
                Err(e) => return (Err(e), None),
            }
        }
    }

    #[test]
    fn both_sides_learn_each_others_identity() {
        let client_id = Identity::generate("client").with_info(b"client info".to_vec());
        let server_id = Identity::generate("server");
        let (client, server) =
            run(SetupClient::new(client_id.clone(), "3939"), SetupServer::new(server_id.clone(), "3939"));
        let (client, server) = (client.unwrap(), server.unwrap());
        assert_eq!(client.session_key, server.session_key);
        let learnt_server = client.peer.unwrap();
        assert_eq!(learnt_server.id, b"server");
        assert_eq!(learnt_server.public_key, server_id.public_key());
        assert_eq!(learnt_server.info, None);
        let learnt_client = server.peer.unwrap();
        assert_eq!(learnt_client.id, b"client");
        assert_eq!(learnt_client.public_key, client_id.public_key());
        assert_eq!(learnt_client.info.as_deref(), Some(&b"client info"[..]));
    }

    #[test]
    fn a_transient_pairing_stops_after_m4() {
        let (client, server) = run(
            SetupClient::new(Identity::generate("c"), "3939").with_flags(FLAG_TRANSIENT),
            SetupServer::new(Identity::generate("s"), "3939"),
        );
        let (client, server) = (client.unwrap(), server.unwrap());
        assert_eq!(client.session_key, server.session_key);
        assert!(client.peer.is_none() && server.peer.is_none());
    }

    #[test]
    fn the_wrong_code_ends_in_an_authentication_error() {
        let (client, server) =
            run(SetupClient::new(Identity::generate("c"), "1234"), SetupServer::new(Identity::generate("s"), "4321"));
        assert_eq!(client.unwrap_err(), PairingError::Peer(error::AUTHENTICATION));
        assert!(server.is_none());
    }

    #[test]
    fn a_message_out_of_order_is_refused() {
        let mut server = SetupServer::new(Identity::generate("s"), "1");
        let refusal = server.handle(&encode(&[(kind::STATE, &[3])])).err().unwrap();
        assert_eq!(refusal.error, PairingError::UnexpectedState { expected: 1, got: Some(3) });
        assert_eq!(refusal.reply.unwrap(), encode(&[(kind::STATE, &[2]), (kind::ERROR, &[error::UNKNOWN])]));
    }
}
