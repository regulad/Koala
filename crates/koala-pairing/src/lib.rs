//! Apple's pair-setup and pair-verify, without a transport.
//!
//! Apple devices pair with each other the way HomeKit accessories pair with
//! their controllers: an SRP-6a exchange over a shared setup code, in which
//! each side hands the other a long-term Ed25519 identity (pair-setup), and
//! later an X25519 exchange in which they prove those identities and agree on
//! fresh keys (pair-verify). Apple's CoreUtils implements both for Rapport,
//! AirPlay and others; each carries the same TLV8 messages its own way.
//!
//! This crate is those messages and nothing else: each side is a state
//! machine that takes the peer's message and returns its own. The transport
//! (Rapport's frames for Koala) wraps them, and derives its keys from the
//! result.
//!
//! References: the HomeKit Accessory Protocol specification; Apple's
//! HomeKitADK (Apache-2.0) for the accessory side; pyatv (MIT), whose
//! implementation works against real Apple TVs, as a test oracle.

pub mod crypto;
pub mod identity;
pub mod setup;
pub mod srp;
pub mod tlv8;
pub mod verify;

pub use identity::{Identity, Peer};
pub use setup::{Paired, SetupClient, SetupServer};
pub use verify::{Verified, VerifyClient, VerifyServer};

/// What a state machine wants done with the peer's message.
#[derive(Debug)]
pub enum Step<T> {
    /// Send this to the peer, and pass on its answer.
    Send(Vec<u8>),
    /// Finished: send this last message, if there is one.
    Done { send: Option<Vec<u8>>, result: T },
}

/// Why pairing failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    #[error("malformed pairing message: {0}")]
    Malformed(&'static str),
    #[error("expected pairing state {expected}, got {got:?}")]
    UnexpectedState { expected: u8, got: Option<u8> },
    #[error("the peer reported pairing error {0}")]
    Peer(u8),
    #[error(transparent)]
    Srp(#[from] srp::SrpError),
    #[error("a sealed pairing message didn't decrypt")]
    Decryption,
    #[error("the peer's signature doesn't verify")]
    Signature,
    #[error("the peer isn't paired with us")]
    UnknownPeer,
    #[error("pairing has already finished")]
    Finished,
}

/// A server's refusal: why, and the error message to send the client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{error}")]
pub struct Refusal {
    pub error: PairingError,
    pub reply: Option<Vec<u8>>,
}
