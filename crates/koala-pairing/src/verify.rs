//! Pair-verify: two devices already paired prove their identities to each
//! other and agree on a fresh secret (HomeKit Accessory Protocol, "Pair
//! Verify").
//!
//! | Message | From | Carries |
//! |---|---|---|
//! | M1 | client | its ephemeral X25519 key |
//! | M2 | server | its ephemeral X25519 key; its identity, signed, sealed |
//! | M3 | client | its identity, signed, sealed |
//! | M4 | server | nothing: success |
//!
//! Each side signs `own ephemeral key | own id | other's ephemeral key` with
//! its long-term key. The X25519 shared secret is the result, from which the
//! transport derives its keys.

use x25519_dalek::{PublicKey, StaticSecret};

use crate::crypto::{hkdf_sha512, open, seal};
use crate::identity::{Identity, verify};
use crate::setup::{expect_state, refuse};
use crate::tlv8::{Tlv8, encode, kind};
use crate::{PairingError, Refusal, Step};

/// What pair-verify leaves the two sides with.
#[derive(Debug)]
pub struct Verified {
    /// The peer's identifier, whose key the signature was checked against.
    pub peer_id: Vec<u8>,
    /// The X25519 shared secret.
    pub shared_secret: [u8; 32],
}

/// The server (accessory) side.
pub struct VerifyServer {
    identity: Identity,
    state: ServerState,
}

enum ServerState {
    AwaitingM1,
    AwaitingM3 { shared: [u8; 32], ours: [u8; 32], theirs: [u8; 32] },
    Finished,
}

impl VerifyServer {
    pub fn new(identity: Identity) -> Self {
        Self { identity, state: ServerState::AwaitingM1 }
    }

    /// Handles the client's next message. `known` gives the long-term public
    /// key of a paired peer by its identifier.
    pub fn handle(
        &mut self,
        message: &[u8],
        known: impl Fn(&[u8]) -> Option<[u8; 32]>,
    ) -> Result<Step<Verified>, Refusal> {
        let tlv = Tlv8::decode(message).map_err(|_| refuse(PairingError::Malformed("TLV8"), 0))?;
        match std::mem::replace(&mut self.state, ServerState::Finished) {
            ServerState::AwaitingM1 => {
                expect_state(&tlv, 1).map_err(|e| refuse(e, 2))?;
                let theirs = ephemeral_key(&tlv).map_err(|e| refuse(e, 2))?;
                let (shared, ours) = agree(&theirs).map_err(|e| refuse(e, 2))?;
                let signature = self.identity.sign(&[&ours[..], self.identity.id(), &theirs].concat());
                let sealed = seal(
                    &encryption_key(&shared),
                    b"PV-Msg02",
                    &encode(&[(kind::IDENTIFIER, self.identity.id()), (kind::SIGNATURE, &signature)]),
                );
                let m2 = encode(&[(kind::STATE, &[2]), (kind::PUBLIC_KEY, &ours), (kind::ENCRYPTED_DATA, &sealed)]);
                self.state = ServerState::AwaitingM3 { shared, ours, theirs };
                Ok(Step::Send(m2))
            }
            ServerState::AwaitingM3 { shared, ours, theirs } => {
                expect_state(&tlv, 3).map_err(|e| refuse(e, 4))?;
                let resolve = |given: Option<&[u8]>| given.and_then(|id| known(id).map(|key| (id.to_vec(), key)));
                let peer_id =
                    open_signed(&tlv, &shared, b"PV-Msg03", &theirs, &ours, resolve).map_err(|e| refuse(e, 4))?;
                let m4 = encode(&[(kind::STATE, &[4])]);
                Ok(Step::Done { send: Some(m4), result: Verified { peer_id, shared_secret: shared } })
            }
            ServerState::Finished => Err(Refusal { error: PairingError::Finished, reply: None }),
        }
    }
}

/// The client (controller) side.
pub struct VerifyClient {
    identity: Identity,
    secret: StaticSecret,
    expected: Option<(Vec<u8>, [u8; 32])>,
    state: ClientState,
}

enum ClientState {
    Start,
    AwaitingM2,
    AwaitingM4 { verified: Verified },
    Finished,
}

impl VerifyClient {
    pub fn new(identity: Identity) -> Self {
        let mut bytes = [0; 32];
        crate::srp::random(&mut bytes);
        Self { identity, secret: StaticSecret::from(bytes), expected: None, state: ClientState::Start }
    }

    /// The peer to check M2 against when it names none. An iPhone serving a
    /// guest's SessionPaired peer sends no identifier: CoreUtils finds the
    /// peer by trying its keys on the signature ("Resolve identity for
    /// signature", Koala's trial 8, 2026-11-19).
    pub fn with_expected_peer(mut self, id: Vec<u8>, public_key: [u8; 32]) -> Self {
        self.expected = Some((id, public_key));
        self
    }

    /// M1, the first message.
    pub fn start(&mut self) -> Vec<u8> {
        self.state = ClientState::AwaitingM2;
        encode(&[(kind::STATE, &[1]), (kind::PUBLIC_KEY, PublicKey::from(&self.secret).as_bytes())])
    }

    /// Handles the server's next message. `known` gives the long-term public
    /// key of a paired peer by its identifier.
    pub fn handle(
        &mut self,
        message: &[u8],
        known: impl Fn(&[u8]) -> Option<[u8; 32]>,
    ) -> Result<Step<Verified>, PairingError> {
        let tlv = Tlv8::decode(message).map_err(|_| PairingError::Malformed("TLV8"))?;
        match std::mem::replace(&mut self.state, ClientState::Finished) {
            ClientState::AwaitingM2 => {
                expect_state(&tlv, 2)?;
                let theirs = ephemeral_key(&tlv)?;
                let ours = PublicKey::from(&self.secret).to_bytes();
                let shared = self.secret.diffie_hellman(&PublicKey::from(theirs));
                if !shared.was_contributory() {
                    return Err(PairingError::Malformed("X25519 public key"));
                }
                let shared = shared.to_bytes();
                let expected = self.expected.clone();
                let resolve = |given: Option<&[u8]>| match given {
                    Some(id) => known(id).map(|key| (id.to_vec(), key)),
                    None => expected.clone(),
                };
                let peer_id = open_signed(&tlv, &shared, b"PV-Msg02", &theirs, &ours, resolve)?;
                let signature = self.identity.sign(&[&ours[..], self.identity.id(), &theirs].concat());
                let sealed = seal(
                    &encryption_key(&shared),
                    b"PV-Msg03",
                    &encode(&[(kind::IDENTIFIER, self.identity.id()), (kind::SIGNATURE, &signature)]),
                );
                self.state = ClientState::AwaitingM4 { verified: Verified { peer_id, shared_secret: shared } };
                Ok(Step::Send(encode(&[(kind::STATE, &[3]), (kind::ENCRYPTED_DATA, &sealed)])))
            }
            ClientState::AwaitingM4 { verified } => {
                expect_state(&tlv, 4)?;
                Ok(Step::Done { send: None, result: verified })
            }
            ClientState::Start | ClientState::Finished => Err(PairingError::Finished),
        }
    }
}

fn encryption_key(shared: &[u8; 32]) -> [u8; 32] {
    hkdf_sha512(shared, b"Pair-Verify-Encrypt-Salt", b"Pair-Verify-Encrypt-Info")
}

fn ephemeral_key(tlv: &Tlv8) -> Result<[u8; 32], PairingError> {
    tlv.get(kind::PUBLIC_KEY).and_then(|k| k.try_into().ok()).ok_or(PairingError::Malformed("X25519 public key"))
}

/// A fresh ephemeral key, and its shared secret with `theirs`.
fn agree(theirs: &[u8; 32]) -> Result<([u8; 32], [u8; 32]), PairingError> {
    let mut bytes = [0; 32];
    crate::srp::random(&mut bytes);
    let secret = StaticSecret::from(bytes);
    let shared = secret.diffie_hellman(&PublicKey::from(*theirs));
    if !shared.was_contributory() {
        return Err(PairingError::Malformed("X25519 public key"));
    }
    Ok((shared.to_bytes(), PublicKey::from(&secret).to_bytes()))
}

/// Opens the peer's sealed identity and checks its signature over
/// `their ephemeral key | their id | our ephemeral key`. `resolve` gives the
/// peer's identifier and long-term key from the identifier it sent, if any.
/// When it sent none, the signature may also be over the two ephemeral keys
/// alone.
fn open_signed(
    tlv: &Tlv8,
    shared: &[u8; 32],
    label: &[u8; 8],
    theirs: &[u8; 32],
    ours: &[u8; 32],
    resolve: impl Fn(Option<&[u8]>) -> Option<(Vec<u8>, [u8; 32])>,
) -> Result<Vec<u8>, PairingError> {
    let sealed = tlv.get(kind::ENCRYPTED_DATA).ok_or(PairingError::Malformed("encrypted data"))?;
    let plain = open(&encryption_key(shared), label, sealed).ok_or(PairingError::Decryption)?;
    let inner = Tlv8::decode(&plain).map_err(|_| PairingError::Malformed("sealed TLV8"))?;
    let signature = inner.get(kind::SIGNATURE).ok_or(PairingError::Malformed("signature"))?;
    let given = inner.get(kind::IDENTIFIER);
    let (id, public_key) = resolve(given).ok_or(PairingError::UnknownPeer)?;
    let mut signed = vec![[&theirs[..], &id, ours].concat()];
    if given.is_none() {
        signed.push([&theirs[..], ours].concat());
    }
    if !signed.iter().any(|message| verify(&public_key, message, signature)) {
        return Err(PairingError::Signature);
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tlv8::error;

    fn verify_pair(
        client: &Identity,
        server: &Identity,
        client_knows: [u8; 32],
        server_knows: [u8; 32],
    ) -> (Result<Verified, PairingError>, Result<Verified, PairingError>) {
        let mut c = VerifyClient::new(client.clone());
        let mut s = VerifyServer::new(server.clone());
        let (cid, sid) = (client.id().to_vec(), server.id().to_vec());
        let server_lookup = |id: &[u8]| (id == cid.as_slice()).then_some(server_knows);
        let client_lookup = |id: &[u8]| (id == sid.as_slice()).then_some(client_knows);
        let Step::Send(m2) = s.handle(&c.start(), server_lookup).unwrap() else { panic!("M2") };
        let m3 = match c.handle(&m2, client_lookup) {
            Ok(Step::Send(m3)) => m3,
            Ok(_) => panic!("M3"),
            Err(e) => return (Err(e), Err(PairingError::Finished)),
        };
        match s.handle(&m3, server_lookup) {
            Ok(Step::Done { send, result }) => {
                let client = match c.handle(&send.unwrap(), client_lookup).unwrap() {
                    Step::Done { result, .. } => Ok(result),
                    Step::Send(_) => panic!("M4"),
                };
                (client, Ok(result))
            }
            Ok(Step::Send(_)) => panic!("the server wants to go on"),
            Err(refusal) => {
                (c.handle(&refusal.reply.unwrap(), client_lookup).map(|_| unreachable!()), Err(refusal.error))
            }
        }
    }

    #[test]
    fn paired_devices_verify_each_other() {
        let (client, server) = (Identity::generate("client"), Identity::generate("server"));
        let (c, s) = verify_pair(&client, &server, server.public_key(), client.public_key());
        let (c, s) = (c.unwrap(), s.unwrap());
        assert_eq!(c.shared_secret, s.shared_secret);
        assert_eq!(c.peer_id, b"server");
        assert_eq!(s.peer_id, b"client");
    }

    #[test]
    fn a_client_with_another_key_is_refused() {
        let (client, server) = (Identity::generate("client"), Identity::generate("server"));
        let impostor = Identity::generate("client");
        let (c, s) = verify_pair(&impostor, &server, server.public_key(), client.public_key());
        assert_eq!(s.unwrap_err(), PairingError::Signature);
        assert_eq!(c.unwrap_err(), PairingError::Peer(error::AUTHENTICATION));
    }

    /// An M2 with a signature and no identifier, as an iPhone sends to a
    /// SessionPaired peer, signed with or without the server's identifier.
    #[test]
    fn a_server_that_names_no_one_is_checked_against_the_expected_peer() {
        for with_id in [true, false] {
            let (client, server) = (Identity::generate("client"), Identity::generate("server"));
            let mut c = VerifyClient::new(client.clone()).with_expected_peer(b"server".to_vec(), server.public_key());
            let m1 = Tlv8::decode(&c.start()).unwrap();
            let theirs: [u8; 32] = m1.get(kind::PUBLIC_KEY).unwrap().try_into().unwrap();
            let (shared, ours) = agree(&theirs).unwrap();
            let signed: Vec<u8> =
                if with_id { [&ours[..], server.id(), &theirs].concat() } else { [ours, theirs].concat() };
            let sealed =
                seal(&encryption_key(&shared), b"PV-Msg02", &encode(&[(kind::SIGNATURE, &server.sign(&signed))]));
            let m2 = encode(&[(kind::STATE, &[2]), (kind::PUBLIC_KEY, &ours), (kind::ENCRYPTED_DATA, &sealed)]);
            let nobody = |_: &[u8]| None;
            assert!(matches!(c.handle(&m2, nobody), Ok(Step::Send(_))), "signed with the identifier: {with_id}");
        }
        // And a stranger's signature still fails.
        let (client, server, stranger) =
            (Identity::generate("client"), Identity::generate("server"), Identity::generate("server"));
        let mut c = VerifyClient::new(client).with_expected_peer(b"server".to_vec(), server.public_key());
        let m1 = Tlv8::decode(&c.start()).unwrap();
        let theirs: [u8; 32] = m1.get(kind::PUBLIC_KEY).unwrap().try_into().unwrap();
        let (shared, ours) = agree(&theirs).unwrap();
        let signature = stranger.sign(&[ours, theirs].concat());
        let sealed = seal(&encryption_key(&shared), b"PV-Msg02", &encode(&[(kind::SIGNATURE, &signature)]));
        let m2 = encode(&[(kind::STATE, &[2]), (kind::PUBLIC_KEY, &ours), (kind::ENCRYPTED_DATA, &sealed)]);
        assert_eq!(c.handle(&m2, |_: &[u8]| None).unwrap_err(), PairingError::Signature);
    }

    #[test]
    fn a_server_with_another_key_is_refused() {
        let (client, server) = (Identity::generate("client"), Identity::generate("server"));
        let impostor = Identity::generate("server");
        let (c, _) = verify_pair(&client, &impostor, server.public_key(), client.public_key());
        assert_eq!(c.unwrap_err(), PairingError::Signature);
    }
}
