//! Who a pairing is between: each side's long-term Ed25519 identity.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Our long-term identity: an identifier, an Ed25519 key pair, and optional
/// extra information sent alongside them in pair-setup.
#[derive(Clone)]
pub struct Identity {
    id: Vec<u8>,
    key: SigningKey,
    info: Option<Vec<u8>>,
}

impl Identity {
    /// A new identity with a fresh key pair.
    pub fn generate(id: impl Into<Vec<u8>>) -> Self {
        let mut seed = [0; 32];
        crate::srp::random(&mut seed);
        Self::from_seed(id, seed)
    }

    /// An identity whose key pair comes from a stored 32-byte seed.
    pub fn from_seed(id: impl Into<Vec<u8>>, seed: [u8; 32]) -> Self {
        Self { id: id.into(), key: SigningKey::from_bytes(&seed), info: None }
    }

    /// The same identity, sending `info` in pair-setup's INFO item. Apple's
    /// devices put an OPACK dictionary there; this crate doesn't look inside.
    pub fn with_info(mut self, info: Vec<u8>) -> Self {
        self.info = Some(info);
        self
    }

    pub fn id(&self) -> &[u8] {
        &self.id
    }

    pub fn info(&self) -> Option<&[u8]> {
        self.info.as_deref()
    }

    /// The seed to store, to make this identity again with [`Self::from_seed`].
    pub fn seed(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.sign(message).to_bytes()
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key.
        f.debug_struct("Identity")
            .field("id", &String::from_utf8_lossy(&self.id))
            .field("public_key", &Hex(&self.public_key()))
            .finish_non_exhaustive()
    }
}

/// A peer we're paired with, as learnt in pair-setup.
#[derive(Clone, PartialEq, Eq)]
pub struct Peer {
    pub id: Vec<u8>,
    pub public_key: [u8; 32],
    /// What the peer sent in the INFO item, if anything.
    pub info: Option<Vec<u8>>,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("id", &String::from_utf8_lossy(&self.id))
            .field("public_key", &Hex(&self.public_key))
            .field("info", &self.info.as_ref().map(|i| i.len()))
            .finish()
    }
}

/// Checks `signature` over `message` by `public_key`.
pub(crate) fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(public_key) else { return false };
    let Ok(signature) = <[u8; 64]>::try_from(signature) else { return false };
    key.verify(message, &Signature::from_bytes(&signature)).is_ok()
}

struct Hex<'a>(&'a [u8]);

impl std::fmt::Debug for Hex<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}
