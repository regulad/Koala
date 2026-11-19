//! A Rapport server for testing clients against: it pairs with a setup code,
//! verifies, and answers every request with its name.
//!
//! ```text
//! rapport_server <port> <setup code>
//! ```
//!
//! It prints `LISTENING <port>`, then one line per event: `PAIRED`,
//! `VERIFIED`, `REQUEST <name>`, `EVENT <name>`, `CLOSED`, `ERROR <reason>`.

use apple_opack::Value;
use koala_pairing::{Identity, Peer, SetupServer};
use koala_rapport::auth::serve_pairing;
use koala_rapport::message::dict;
use koala_rapport::{Connection, FrameType, Message, MessageType, RapportError};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [port, code] = args.as_slice() else {
        eprintln!("usage: rapport_server <port> <setup code>");
        std::process::exit(2);
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port.parse::<u16>().expect("port"))).await.unwrap();
    println!("LISTENING {}", listener.local_addr().unwrap().port());
    let identity = Identity::generate("koala-test-server");
    let mut peers: Vec<Peer> = Vec::new();
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        let mut connection = Connection::new(stream);
        let known = peers.len();
        match serve(&mut connection, &identity, code, &mut peers, known).await {
            Ok(()) | Err(RapportError::Closed) => println!("CLOSED"),
            Err(e) => println!("ERROR {e}"),
        }
    }
}

async fn serve(
    connection: &mut Connection<tokio::net::TcpStream>,
    identity: &Identity,
    code: &str,
    peers: &mut Vec<Peer>,
    known: usize,
) -> Result<(), RapportError> {
    let setup = || Some(SetupServer::new(identity.clone(), code));
    let verified = match serve_pairing(connection, identity, setup, peers).await {
        Ok(verified) => verified,
        // pyatv closes after pair-setup, before verifying.
        Err(RapportError::Closed) if peers.len() > known => {
            println!("PAIRED");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    println!("VERIFIED {}", String::from_utf8_lossy(&verified.peer_id));
    loop {
        let (kind, value) = connection.receive_opack().await?;
        if kind != FrameType::E_OPACK {
            continue;
        }
        let Some(message) = Message::parse(value) else { continue };
        match message.kind {
            MessageType::Request => {
                let name = message.id.unwrap_or_default();
                println!("REQUEST {name}");
                let content = dict(vec![("name", Value::from("Koala test server")), ("_i", Value::from(name))]);
                let reply = Message::response(message.xid.unwrap_or(0), content);
                connection.send_opack(FrameType::E_OPACK, &reply).await?;
            }
            MessageType::Event => println!("EVENT {}", message.id.unwrap_or_default()),
            MessageType::Response => {}
        }
    }
}
