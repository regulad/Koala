//! Apple's Rapport protocol, which Apple calls the companion link: how Apple
//! devices nearby talk to each other once paired.
//!
//! A connection carries frames ([`frame`]). The first ones authenticate the
//! two devices ([`auth`], on `koala-pairing`): pair-setup with a setup code
//! the first time, pair-verify after that. Either leaves every later frame
//! encrypted ([`cipher`]), and the two exchange OPACK requests, responses and
//! events ([`message`]).
//!
//! pyatv (MIT), which speaks it to Apple TVs, is the reference and the test
//! oracle; `docs/guest-pairing.md` covers what Koala's guest pairing adds.

pub mod auth;
pub mod cipher;
pub mod connection;
pub mod frame;
pub mod message;

pub use connection::Connection;
pub use frame::FrameType;
pub use message::{Message, MessageType};

/// Why a Rapport connection failed.
#[derive(Debug, thiserror::Error)]
pub enum RapportError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the peer closed the connection")]
    Closed,
    #[error(transparent)]
    Decryption(#[from] cipher::BadTag),
    #[error("malformed OPACK: {0}")]
    Opack(#[from] apple_opack::Error),
    #[error(transparent)]
    Pairing(#[from] koala_pairing::PairingError),
    #[error("protocol error: {0}")]
    Protocol(&'static str),
}

#[cfg(test)]
mod tests {
    use apple_opack::Value;
    use koala_pairing::{Identity, Peer, SetupServer};
    use tokio::net::TcpStream;

    use super::*;
    use crate::auth::{AUTH_TYPE_PAIRED, Method, PASSWORD_TYPE_PIN, pair_setup, pair_verify, serve_pairing};
    use crate::message::dict;

    /// Answers one `_ping` request.
    async fn pong(c: &mut Connection<TcpStream>) {
        let (kind, request) = c.receive_opack().await.unwrap();
        assert_eq!(kind, FrameType::E_OPACK);
        let request = Message::parse(request).unwrap();
        assert_eq!(request.id.as_deref(), Some("_ping"));
        let reply = Message::response(request.xid.unwrap(), dict(vec![("pong", true.into())]));
        c.send_opack(FrameType::E_OPACK, &reply).await.unwrap();
    }

    /// Sends a `_ping` request and checks the answer.
    async fn ping(c: &mut Connection<TcpStream>, xid: u64) {
        assert!(c.is_encrypted());
        c.send_opack(FrameType::E_OPACK, &Message::request("_ping", xid, dict(vec![]))).await.unwrap();
        let (_, reply) = c.receive_opack().await.unwrap();
        let reply = Message::parse(reply).unwrap();
        assert_eq!(reply.kind, MessageType::Response);
        assert_eq!(reply.xid, Some(xid));
        assert_eq!(reply.content.get("pong"), Some(&Value::Bool(true)));
    }

    /// Pair-setup, then an encrypted request on the same connection; then
    /// pair-verify on a new one, and another request.
    #[tokio::test]
    async fn a_client_pairs_verifies_and_talks_to_a_server() {
        let (server_id, client_id) = (Identity::generate("server"), Identity::generate("client"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let server = tokio::spawn({
            let server_id = server_id.clone();
            async move {
                let mut peers: Vec<Peer> = Vec::new();
                let (stream, _) = listener.accept().await.unwrap();
                let mut c = Connection::new(stream);
                let setup = || Some(SetupServer::new(server_id.clone(), "1234"));
                let who = serve_pairing(&mut c, &server_id, setup, &mut peers).await.unwrap();
                assert_eq!((who.peer_id.as_slice(), who.method), (&b"client"[..], Method::Setup));
                pong(&mut c).await;

                let (stream, _) = listener.accept().await.unwrap();
                let mut c = Connection::new(stream);
                let who = serve_pairing(&mut c, &server_id, || None, &mut peers).await.unwrap();
                assert_eq!((who.peer_id.as_slice(), who.method), (&b"client"[..], Method::Verify));
                pong(&mut c).await;
            }
        });

        let mut c = Connection::new(TcpStream::connect(address).await.unwrap());
        let paired = pair_setup(&mut c, client_id.clone(), "1234", PASSWORD_TYPE_PIN).await.unwrap();
        let server_peer = paired.peer.unwrap();
        assert_eq!(server_peer.public_key, server_id.public_key());
        ping(&mut c, 40).await;
        drop(c);

        let known = |id: &[u8]| (id == server_peer.id.as_slice()).then_some(server_peer.public_key);
        let mut c = Connection::new(TcpStream::connect(address).await.unwrap());
        pair_verify(&mut c, client_id, known, None, AUTH_TYPE_PAIRED).await.unwrap();
        ping(&mut c, 41).await;
        server.await.unwrap();
    }
}
