//! Runs one side of pair-setup, then pair-verify, over standard input and
//! output, one hex-encoded message per line, so another implementation can
//! drive it (`tests/pyatv_oracle.py` uses pyatv's).
//!
//! ```text
//! pair_stdio server <setup code> <identifier>
//! pair_stdio client <setup code> <identifier>
//! ```
//!
//! It first prints `ID <identifier hex> <long-term public key hex>`. Then,
//! after pair-setup, `PAIRED <peer identifier hex> <peer key hex> <info hex|->`
//! and after pair-verify `VERIFIED <shared secret hex>`. A failure prints
//! `ERROR <reason>`, after the error message a server sends, if any.

use std::io::{BufRead, Write, stdin, stdout};

use koala_pairing::{Identity, Paired, PairingError, SetupClient, SetupServer, Step, VerifyClient, VerifyServer};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [role, code, id] = args.as_slice() else {
        eprintln!("usage: pair_stdio server|client <setup code> <identifier>");
        std::process::exit(2);
    };
    let identity = Identity::generate(id.as_bytes());
    say(&format!("ID {} {}", hex(identity.id()), hex(&identity.public_key())));
    let result = match role.as_str() {
        "server" => server(identity, code),
        "client" => client(identity, code),
        _ => {
            eprintln!("role must be server or client");
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        say(&format!("ERROR {error}"));
    }
}

fn server(identity: Identity, code: &str) -> Result<(), PairingError> {
    let mut setup = SetupServer::new(identity.clone(), code);
    let paired = loop {
        match setup.handle(&read()) {
            Ok(Step::Send(reply)) => say(&hex(&reply)),
            Ok(Step::Done { send, result }) => {
                send.iter().for_each(|m| say(&hex(m)));
                break result;
            }
            Err(refusal) => {
                refusal.reply.iter().for_each(|m| say(&hex(m)));
                return Err(refusal.error);
            }
        }
    };
    let peer = report(paired)?;
    let known = |id: &[u8]| (id == peer.0.as_slice()).then_some(peer.1);
    let mut verify = VerifyServer::new(identity);
    loop {
        match verify.handle(&read(), known) {
            Ok(Step::Send(reply)) => say(&hex(&reply)),
            Ok(Step::Done { send, result }) => {
                send.iter().for_each(|m| say(&hex(m)));
                say(&format!("VERIFIED {}", hex(&result.shared_secret)));
                return Ok(());
            }
            Err(refusal) => {
                refusal.reply.iter().for_each(|m| say(&hex(m)));
                return Err(refusal.error);
            }
        }
    }
}

fn client(identity: Identity, code: &str) -> Result<(), PairingError> {
    let mut setup = SetupClient::new(identity.clone(), code);
    say(&hex(&setup.start()));
    let paired = loop {
        match setup.handle(&read())? {
            Step::Send(message) => say(&hex(&message)),
            Step::Done { result, .. } => break result,
        }
    };
    let peer = report(paired)?;
    let known = |id: &[u8]| (id == peer.0.as_slice()).then_some(peer.1);
    let mut verify = VerifyClient::new(identity);
    say(&hex(&verify.start()));
    loop {
        match verify.handle(&read(), known)? {
            Step::Send(message) => say(&hex(&message)),
            Step::Done { result, .. } => {
                say(&format!("VERIFIED {}", hex(&result.shared_secret)));
                return Ok(());
            }
        }
    }
}

/// Prints `PAIRED …`, and returns the peer's identifier and key.
fn report(paired: Paired) -> Result<(Vec<u8>, [u8; 32]), PairingError> {
    let peer = paired.peer.ok_or(PairingError::Malformed("no identity: transient pairing"))?;
    let info = peer.info.as_deref().map_or("-".to_string(), hex);
    say(&format!("PAIRED {} {} {}", hex(&peer.id), hex(&peer.public_key), info));
    Ok((peer.id, peer.public_key))
}

fn read() -> Vec<u8> {
    let mut line = String::new();
    if stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        std::process::exit(1);
    }
    let line = line.trim();
    (0..line.len()).step_by(2).map(|i| u8::from_str_radix(&line[i..i + 2], 16).expect("hex input")).collect()
}

fn say(line: &str) {
    let mut out = stdout().lock();
    writeln!(out, "{line}").and_then(|()| out.flush()).expect("stdout");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
