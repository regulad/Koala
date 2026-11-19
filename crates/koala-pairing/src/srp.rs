//! SRP-6a as Apple's pair-setup uses it: the 3072-bit group of RFC 5054,
//! SHA-512, user name `Pair-Setup`, and the setup code as the password.
//!
//! The arithmetic is the `srp` crate's. The encodings are chosen here, once,
//! because implementations differ and Apple's are what count:
//!
//! - `k = H(N | PAD(g))` and `u = H(PAD(A) | PAD(B))`, padded to the length
//!   of `N`, as RFC 5054 says;
//! - `x = H(s | H(I | ":" | P))`;
//! - `K = H(S)`;
//! - `M1 = H(H(N) xor H(g) | H(I) | s | A | B | K)`, with `g` as its one
//!   byte (not padded: Apple's form, which `srp` 0.7's client doesn't offer);
//! - `M2 = H(A | M1 | K)`.
//!
//! `A`, `B` and `S` go into hashes as their minimal big-endian bytes, as
//! pyatv's srptools sends them to real Apple TVs. Whether Apple pads a value
//! that starts with a zero byte (about 1 in 256) isn't settled: RustCrypto's
//! `EncodingOptions::HOMEKIT` (PAKEs #313, tested against HomeKitADK's
//! vectors) answers that, and is due to replace these choices once it may be
//! used (PLAN.md). Until then, our own `A` and `B` are drawn again if they
//! would start with a zero, so only a peer's value can hit the case.

use sha2::{Digest, Sha512};
use srp::bigint::{BoxedUint, Resize};
use srp::groups::G3072;
use srp::utils::{compute_hash, compute_hash_n_xor_hash_g, compute_k, compute_m2, compute_u_padded};
use srp::{Client, Group, Server};

/// The user name Apple's pair-setup uses.
pub const USER: &[u8] = b"Pair-Setup";
/// The length of the 3072-bit modulus, in bytes.
pub const MODULUS_LEN: usize = 384;

type Hash = Sha512;

/// The server's (accessory's) half: it knows the setup code and proves it.
pub struct SrpServer {
    salt: [u8; 16],
    b: Vec<u8>,
    b_pub: Vec<u8>,
    verifier: Vec<u8>,
}

impl SrpServer {
    /// Starts a session for `code`, the setup code (password).
    pub fn new(code: &[u8]) -> Self {
        let mut salt = [0; 16];
        random(&mut salt);
        let verifier = Client::<G3072, Hash>::new().compute_verifier(USER, code, &salt);
        let server = Server::<G3072, Hash>::new();
        let g = G3072::generator();
        let k = compute_k::<Hash>(&g);
        let v = BoxedUint::from_be_slice_vartime(&verifier);
        loop {
            let mut b = vec![0; 32];
            random(&mut b);
            let b_pub = server
                .compute_b_pub(&BoxedUint::from_be_slice_vartime(&b), &k, &v)
                .to_be_bytes_trimmed_vartime()
                .to_vec();
            if b_pub.len() == MODULUS_LEN {
                return Self { salt, b, b_pub, verifier };
            }
        }
    }

    /// The salt, for the client.
    pub fn salt(&self) -> &[u8] {
        &self.salt
    }

    /// `B`, for the client.
    pub fn public_key(&self) -> &[u8] {
        &self.b_pub
    }

    /// Checks the client's `A` and proof `M1`. On success, returns our proof
    /// `M2` and the shared session key `K`.
    pub fn verify(&self, a_pub: &[u8], client_proof: &[u8]) -> Result<(Vec<u8>, Vec<u8>), SrpError> {
        let a = valid_public(a_pub)?;
        let g = G3072::generator();
        let server = Server::<G3072, Hash>::new();
        let u = compute_u_padded::<Hash>(&g, a_pub, &self.b_pub);
        let s = server
            .compute_premaster_secret(
                &a,
                &BoxedUint::from_be_slice_vartime(&self.verifier),
                &u,
                &BoxedUint::from_be_slice_vartime(&self.b),
            )
            .to_be_bytes_trimmed_vartime();
        let key = compute_hash::<Hash>(&s);
        let m1 = compute_m1(&self.salt, a_pub, &self.b_pub, &key);
        if !constant_time_eq(&m1, client_proof) {
            return Err(SrpError::WrongCode);
        }
        let m2 = compute_m2::<Hash>(a_pub, &m1.into(), &key);
        Ok((m2.to_vec(), key.to_vec()))
    }
}

/// The client's (controller's) half: it proves it was given the setup code.
pub struct SrpClient {
    a: Vec<u8>,
    a_pub: Vec<u8>,
}

/// What the client sends, and what it expects back.
pub struct ClientProof {
    /// `A`, for the server.
    pub public_key: Vec<u8>,
    /// `M1`, for the server.
    pub proof: Vec<u8>,
    /// The `M2` the server must answer with.
    pub expected_server_proof: Vec<u8>,
    /// The shared session key `K`, once the server's proof checks out.
    pub key: Vec<u8>,
}

impl SrpClient {
    pub fn new() -> Self {
        let client = Client::<G3072, Hash>::new();
        loop {
            let mut a = vec![0; 32];
            random(&mut a);
            let a_pub = client.compute_public_ephemeral(&a);
            if a_pub.len() == MODULUS_LEN {
                return Self { a, a_pub };
            }
        }
    }

    /// Answers the server's salt and `B` with `A` and `M1` for `code`.
    pub fn respond(&self, code: &[u8], salt: &[u8], b_pub: &[u8]) -> Result<ClientProof, SrpError> {
        let b = valid_public(b_pub)?;
        let g = G3072::generator();
        let client = Client::<G3072, Hash>::new();
        let u = compute_u_padded::<Hash>(&g, &self.a_pub, b_pub);
        let k = compute_k::<Hash>(&g);
        let identity = Client::<G3072, Hash>::compute_identity_hash(USER, code);
        let x = Client::<G3072, Hash>::compute_x(&identity, salt);
        let s = client
            .compute_premaster_secret(&b, &k, &x, &BoxedUint::from_be_slice_vartime(&self.a), &u)
            .to_be_bytes_trimmed_vartime();
        let key = compute_hash::<Hash>(&s);
        let m1 = compute_m1(salt, &self.a_pub, b_pub, &key);
        let m2 = compute_m2::<Hash>(&self.a_pub, &m1.into(), &key);
        Ok(ClientProof {
            public_key: self.a_pub.clone(),
            proof: m1.to_vec(),
            expected_server_proof: m2.to_vec(),
            key: key.to_vec(),
        })
    }
}

impl Default for SrpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// SRP failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SrpError {
    /// The peer's public value is 0 modulo `N`, which would fix the key.
    #[error("the peer's SRP public value is invalid")]
    BadPublicKey,
    /// The proofs don't match: the peer used another setup code.
    #[error("wrong setup code")]
    WrongCode,
}

/// `M1 = H(H(N) xor H(g) | H(I) | s | A | B | K)`.
fn compute_m1(salt: &[u8], a_pub: &[u8], b_pub: &[u8], key: &[u8]) -> [u8; 64] {
    let g = G3072::generator();
    let mut d = Hash::new();
    d.update(compute_hash_n_xor_hash_g::<Hash>(&g));
    d.update(compute_hash::<Hash>(USER));
    d.update(salt);
    d.update(a_pub);
    d.update(b_pub);
    d.update(key);
    d.finalize().into()
}

/// A peer's `A` or `B`, refused if it is 0 modulo `N`.
fn valid_public(bytes: &[u8]) -> Result<BoxedUint, SrpError> {
    let value = BoxedUint::from_be_slice_vartime(bytes);
    let g = G3072::generator();
    let n = g.params().modulus().as_nz_ref();
    if bool::from(((&value).resize(n.bits_precision()) % n).is_zero()) {
        return Err(SrpError::BadPublicKey);
    }
    Ok(value)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn random(buf: &mut [u8]) {
    getrandom::fill(buf).expect("the system's random number generator failed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_and_server_agree_on_the_key() {
        let server = SrpServer::new(b"1111");
        let client = SrpClient::new();
        let proof = client.respond(b"1111", server.salt(), server.public_key()).unwrap();
        let (m2, key) = server.verify(&proof.public_key, &proof.proof).unwrap();
        assert_eq!(m2, proof.expected_server_proof);
        assert_eq!(key, proof.key);
        assert_eq!(key.len(), 64);
    }

    #[test]
    fn the_wrong_code_is_refused() {
        let server = SrpServer::new(b"1111");
        let proof = SrpClient::new().respond(b"2222", server.salt(), server.public_key()).unwrap();
        assert_eq!(server.verify(&proof.public_key, &proof.proof), Err(SrpError::WrongCode));
    }

    #[test]
    fn a_zero_public_value_is_refused() {
        let server = SrpServer::new(b"1111");
        assert_eq!(server.verify(&[0], &[0; 64]), Err(SrpError::BadPublicKey));
        let n = G3072::generator().params().modulus().to_be_bytes();
        assert_eq!(server.verify(&n, &[0; 64]), Err(SrpError::BadPublicKey));
    }

    #[test]
    fn our_public_values_are_full_length() {
        assert_eq!(SrpServer::new(b"x").public_key().len(), MODULUS_LEN);
        assert_eq!(SrpClient::new().a_pub.len(), MODULUS_LEN);
    }
}
