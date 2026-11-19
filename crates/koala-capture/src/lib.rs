//! Apple's Continuity Capture protocol, as the host: what an Apple TV sends
//! an iPhone it has paired with to use its microphone, over a Rapport
//! RemoteDisplay session (`docs/guest-pairing.md`, "Phase B").
//!
//! - [`archive`]: NSKeyedArchiver archives, which carry the session's
//!   configuration.
//! - [`session`]: the session's events and streams.

pub mod archive;
pub mod session;
