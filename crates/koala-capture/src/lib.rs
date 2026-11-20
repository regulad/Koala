//! Apple's Continuity Capture protocol, as the host: what an Apple TV sends
//! an iPhone it has paired with to use its microphone, over a Rapport
//! RemoteDisplay session (`docs/guest-pairing.md`, "Phase B").
//!
//! - [`archive`]: NSKeyedArchiver archives, which carry the session's
//!   configuration.
//! - [`session`]: the session's events.
//! - [`streams`]: the answers that set up its streams.
//! - [`avc`]: the AVConference negotiation that chooses the microphone's
//!   codec.

pub mod archive;
pub mod avc;
pub mod rtcp;
pub mod session;
pub mod srtp;
pub mod streams;
