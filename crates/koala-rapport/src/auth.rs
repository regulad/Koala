//! Pairing over a Rapport connection: `koala-pairing`'s TLV8 messages, each
//! carried in an OPACK dictionary under `_pd`.
//!
//! - Pair-setup: the client sends `PS_Start`, then `PS_Next`, each with
//!   `_pwTy` (the setup code's type: 1 for a 4-digit PIN, 10 for the guest
//!   QR's); the server answers `PS_Next`.
//! - Pair-verify: the client sends `PV_Start` with `_auTy` (4 for an ordinary
//!   paired peer), then `PV_Next`; the server answers `PV_Next`. Then both
//!   switch on encryption.
//!
//! (pyatv's `protocols/companion/auth.py`; for the guest flow,
//! `docs/guest-pairing.md`.)

use apple_opack::Value;
use koala_pairing::{Identity, Paired, Peer, SetupClient, SetupServer, Step, Verified, VerifyClient, VerifyServer};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::RapportError;
use crate::cipher::{Cipher, Role};
use crate::connection::Connection;
use crate::frame::FrameType;
use crate::message::dict;

/// The `_pwTy` for a 4-digit PIN.
pub const PASSWORD_TYPE_PIN: u64 = 1;
/// The `_pwTy` for the guest QR code's password.
pub const PASSWORD_TYPE_QR_CODE: u64 = 10;
/// The `_auTy` pyatv sends in `PV_Start`.
pub const AUTH_TYPE_PAIRED: u64 = 4;

/// The peers a server has paired with.
pub trait PeerStore {
    /// A paired peer's long-term public key.
    fn key(&self, id: &[u8]) -> Option<[u8; 32]>;
    /// Remembers a newly paired peer.
    fn add(&mut self, peer: Peer);
}

impl PeerStore for Vec<Peer> {
    fn key(&self, id: &[u8]) -> Option<[u8; 32]> {
        self.iter().find(|p| p.id == id).map(|p| p.public_key)
    }

    fn add(&mut self, peer: Peer) {
        self.retain(|p| p.id != peer.id);
        self.push(peer);
    }
}

/// Pair-setup as the client, with the setup `code` the server shows.
pub async fn pair_setup<S: AsyncRead + AsyncWrite + Unpin>(
    connection: &mut Connection<S>,
    identity: Identity,
    code: &str,
    password_type: u64,
) -> Result<Paired, RapportError> {
    let mut setup = SetupClient::new(identity, code);
    let mut kind = FrameType::PS_START;
    let mut message = setup.start();
    loop {
        let body = dict(vec![("_pd", message.into()), ("_pwTy", Value::int(password_type))]);
        connection.send_opack(kind, &body).await?;
        let reply = expect(connection, FrameType::PS_NEXT).await?;
        match setup.handle(&reply)? {
            Step::Send(next) => (kind, message) = (FrameType::PS_NEXT, next),
            Step::Done { result, .. } => return Ok(result),
        }
    }
}

/// Pair-verify as the client, then encryption. `known` gives a paired peer's
/// long-term key by its identifier.
pub async fn pair_verify<S: AsyncRead + AsyncWrite + Unpin>(
    connection: &mut Connection<S>,
    identity: Identity,
    known: impl Fn(&[u8]) -> Option<[u8; 32]>,
) -> Result<Verified, RapportError> {
    let mut verify = VerifyClient::new(identity);
    let first = dict(vec![("_pd", verify.start().into()), ("_auTy", Value::int(AUTH_TYPE_PAIRED))]);
    connection.send_opack(FrameType::PV_START, &first).await?;
    loop {
        let reply = expect(connection, FrameType::PV_NEXT).await?;
        match verify.handle(&reply, &known)? {
            Step::Send(next) => connection.send_opack(FrameType::PV_NEXT, &dict(vec![("_pd", next.into())])).await?,
            Step::Done { result, .. } => {
                connection.encrypt(Cipher::new(&result.shared_secret, Role::Client));
                return Ok(result);
            }
        }
    }
}

/// Answers a client's pairing as the server until a pair-verify succeeds,
/// then switches on encryption. Each pair-setup gets a server from `setup`,
/// and is refused when it gives none; peers it pairs are added to `peers`.
pub async fn serve_pairing<S: AsyncRead + AsyncWrite + Unpin>(
    connection: &mut Connection<S>,
    identity: &Identity,
    setup: impl Fn() -> Option<SetupServer>,
    peers: &mut impl PeerStore,
) -> Result<Verified, RapportError> {
    enum Running {
        Nothing,
        Setup(SetupServer),
        Verify(VerifyServer),
    }
    let mut running = Running::Nothing;
    loop {
        let (kind, body) = connection.receive_opack().await?;
        let message = pairing_data(&body)?;
        running = match (kind, running) {
            (FrameType::PS_START, _) => Running::Setup(setup().ok_or(RapportError::Protocol("pair-setup isn't open"))?),
            (FrameType::PV_START, _) => Running::Verify(VerifyServer::new(identity.clone())),
            (FrameType::PS_NEXT, setup @ Running::Setup(_)) | (FrameType::PV_NEXT, setup @ Running::Verify(_)) => setup,
            _ => return Err(RapportError::Protocol("a pairing frame out of order")),
        };
        let step = match &mut running {
            Running::Setup(setup) => setup.handle(message).map(|step| match step {
                Step::Send(reply) => (reply, None),
                Step::Done { send, result } => {
                    if let Some(peer) = result.peer {
                        peers.add(peer);
                    }
                    (send.unwrap_or_default(), None)
                }
            }),
            Running::Verify(verify) => verify.handle(message, |id| peers.key(id)).map(|step| match step {
                Step::Send(reply) => (reply, None),
                Step::Done { send, result } => (send.unwrap_or_default(), Some(result)),
            }),
            Running::Nothing => unreachable!(),
        };
        // Answers go in the exchange's *_Next frame.
        let answer = if matches!(kind, FrameType::PS_START | FrameType::PS_NEXT) {
            FrameType::PS_NEXT
        } else {
            FrameType::PV_NEXT
        };
        match step {
            Ok((reply, verified)) => {
                connection.send_opack(answer, &dict(vec![("_pd", reply.into())])).await?;
                if let Some(verified) = verified {
                    connection.encrypt(Cipher::new(&verified.shared_secret, Role::Server));
                    return Ok(verified);
                }
            }
            Err(refusal) => {
                if let Some(reply) = refusal.reply.clone() {
                    connection.send_opack(answer, &dict(vec![("_pd", reply.into())])).await?;
                }
                return Err(refusal.error.into());
            }
        }
    }
}

/// The next frame, which must be `kind`; its `_pd`.
async fn expect<S: AsyncRead + AsyncWrite + Unpin>(
    connection: &mut Connection<S>,
    kind: FrameType,
) -> Result<Vec<u8>, RapportError> {
    let (got, body) = connection.receive_opack().await?;
    if got != kind {
        return Err(RapportError::Protocol("an unexpected frame during pairing"));
    }
    Ok(pairing_data(&body)?.to_vec())
}

fn pairing_data(body: &Value) -> Result<&[u8], RapportError> {
    body.get("_pd").and_then(Value::as_bytes).ok_or(RapportError::Protocol("a pairing frame without _pd"))
}
