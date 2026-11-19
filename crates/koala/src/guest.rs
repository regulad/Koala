//! `koala guest`: play an Apple TV offering guest pairing, for K2.
//!
//! Everything here follows `docs/guest-pairing.md`, "What Koala needs":
//! the QR code, the `_rdlink._tcp` advert with the TXT the iPhone checks, and
//! a Rapport server for its pair-setup (password type 10, the QR's PIN).

use std::net::{SocketAddr, SocketAddrV6};
use std::time::Duration;

use apple_opack::Value;
use koala_pairing::{Identity, Peer, SetupServer};
use koala_rapport::auth::serve_pairing;
use koala_rapport::message::dict;
use koala_rapport::{Connection, FrameType, Message, MessageType, RapportError};
use qrcode::render::{svg, unicode};
use qrcode::{EcLevel, QrCode};
use tokio::net::{TcpListener, TcpStream};
use warlock_client::{Client, PIPE};

use crate::dnssd::Registration;

/// The ProductType Koala claims: an Apple TV 4K (3rd generation), whose
/// tvOS 27 offers guest pairing.
const MODEL: &str = "AppleTV14,1";
/// `_rdsr`: start the discovery session as mic-only.
const MIC_ONLY: u8 = 3;
/// `rpFl` while pairing: bit 11 (the iPhone ignores devices without it) and
/// bit 19 (pairing mode).
const PAIRING_FLAGS: &str = "0x80800";
/// The Rapport version tvOS 27's rapportd advertises.
const RAPPORT_VERSION: &str = "715.2";

/// Runs for `seconds`, then withdraws the advert and returns.
pub async fn run(seconds: u64) -> Result<(), String> {
    let run_for = Duration::from_secs(seconds);
    let warlock = Client::new(PIPE);
    let info = warlock.info().await.map_err(|e| format!("Warlock: {e}"))?;
    let adapter = info.adapter.ok_or("Warlock has no network adapter yet")?;
    println!(
        "Warlock {}: {} (interface {}) at {}, radio {:?}",
        info.version, adapter.name, adapter.index, adapter.address, info.radio.state
    );

    // Who Koala is to the iPhone: a UUID identifier (the QR's `_pubID`), a
    // key pair, and CoreUtils' info block with an IRK, model and name.
    let public_id = crate::uuid();
    let mut irk = vec![0; 16];
    crate::random(&mut irk);
    let info_block = dict(vec![("altIRK", Value::Bytes(irk)), ("model", MODEL.into()), ("name", "Koala".into())]);
    let info_block = apple_opack::pack(&info_block).map_err(|e| e.to_string())?;
    let identity = Identity::generate(public_id.as_bytes()).with_info(info_block);
    let pairing_id = crate::uuid();
    let pin = crate::uuid();

    // Where the iPhone connects: the adapter's link-local address.
    let address = SocketAddr::V6(SocketAddrV6::new(adapter.address, 0, 0, adapter.index));
    let listener = TcpListener::bind(address).await.map_err(|e| format!("listening on {address}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();

    let mut ba = [0u8; 6];
    crate::random(&mut ba);
    let mut ad = [0u8; 3];
    crate::random(&mut ad);
    let rp_ba = ba.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
    let rp_ad: String = ad.iter().map(|b| format!("{b:02x}")).collect();
    let instance = format!("RDLink-{}._rdlink._tcp.local", rp_ba.replace(':', ""));
    let host = format!("{}.local", info.host_name);
    let txt = [
        ("rpBA", rp_ba.as_str()),
        ("rpAD", rp_ad.as_str()),
        ("rpPI", pairing_id.as_str()),
        ("rpFl", PAIRING_FLAGS),
        ("rpVr", RAPPORT_VERSION),
    ];
    let _registration = Registration::register(adapter.index, &instance, &host, adapter.address, port, &txt)?;
    println!("registered {instance} at {host}:{port}, TXT {txt:?}");

    let mut advertising = warlock.advertise("_rdlink._tcp").await.map_err(|e| format!("Warlock: {e}"))?;
    let mut hold = warlock.hold("Koala guest pairing").await.map_err(|e| format!("Warlock: {e}"))?;

    let url = format!(
        "continuitycamera://connect?_pubID={public_id}&_pinC={pin}&_pi={pairing_id}&model={MODEL}&_rdsr={MIC_ONLY}"
    );
    show_qr(&url)?;

    let mut peers: Vec<Peer> = Vec::new();
    let deadline = tokio::time::sleep(run_for);
    tokio::pin!(deadline);
    let mut last_advertising = String::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, from) = accepted.map_err(|e| e.to_string())?;
                println!("\nconnection from {from}");
                let setup = || Some(SetupServer::new(identity.clone(), pin.as_str()).with_m2_items(vec![(27, vec![1])]));
                converse(stream, from, &identity, setup, &mut peers).await;
            }
            line = advertising.next_value() => match line {
                Some(Ok(state)) => {
                    let state = state.to_string();
                    if state != last_advertising {
                        println!("Warlock advertising: {state}");
                        last_advertising = state;
                    }
                }
                Some(Err(e)) => return Err(format!("Warlock: {e}")),
                None => return Err("Warlock closed the advertising lock".into()),
            },
            line = hold.next_value() => if line.is_none() {
                return Err("Warlock closed the hold".into());
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = &mut deadline => {
                println!("giving up after {seconds} s");
                break;
            }
        }
    }
    Ok(())
}

/// One connection: pairing, then whatever the peer sends, until it closes.
async fn converse(
    stream: TcpStream,
    from: SocketAddr,
    identity: &Identity,
    setup: impl Fn() -> Option<SetupServer>,
    peers: &mut Vec<Peer>,
) {
    let mut connection = Connection::new(stream).with_trace(format!("[{from}]"));
    let known = peers.len();
    let result = serve_pairing(&mut connection, identity, setup, peers).await;
    for peer in &peers[known..] {
        println!("PAIRED with {}", String::from_utf8_lossy(&peer.id));
        if let Some(info) = &peer.info {
            match apple_opack::unpack(info) {
                Ok((value, _)) => println!("  its info: {value:?}"),
                Err(_) => println!("  its info ({} bytes) isn't OPACK", info.len()),
            }
        }
    }
    match result {
        Ok(verified) => println!("VERIFIED {}: encrypted from here", String::from_utf8_lossy(&verified.peer_id)),
        Err(RapportError::Closed) => {
            println!("closed during pairing");
            return;
        }
        Err(e) => {
            println!("pairing failed: {e}");
            return;
        }
    }
    loop {
        match connection.receive_opack().await {
            Ok((FrameType::E_OPACK, value)) => match Message::parse(value) {
                Some(message) if message.kind == MessageType::Request => {
                    let reply = Message::response(message.xid.unwrap_or(0), dict(vec![]));
                    if let Err(e) = connection.send_opack(FrameType::E_OPACK, &reply).await {
                        println!("answering failed: {e}");
                        return;
                    }
                }
                _ => {}
            },
            Ok(_) => {}
            Err(RapportError::Closed) => {
                println!("closed");
                return;
            }
            Err(e) => {
                println!("connection failed: {e}");
                return;
            }
        }
    }
}

/// Prints the QR code (light on dark, as terminals are) and saves it as SVG.
fn show_qr(url: &str) -> Result<(), String> {
    let code = QrCode::with_error_correction_level(url, EcLevel::L).map_err(|e| e.to_string())?;
    let terminal = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    let path = std::env::temp_dir().join("koala-guest-qr.svg");
    let image = code.render::<svg::Color<'_>>().min_dimensions(480, 480).build();
    std::fs::write(&path, image).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("\n{terminal}\n");
    println!("Scan with the iPhone's camera ({}): {url}", path.display());
    // On screen, where a camera can see it.
    let _ = std::process::Command::new("explorer.exe").arg(&path).spawn();
    Ok(())
}
