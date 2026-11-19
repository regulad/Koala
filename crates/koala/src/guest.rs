//! `koala guest`: play an Apple TV offering guest pairing, for K2.
//!
//! Everything here follows `docs/guest-pairing.md`, "What Koala needs":
//! the QR code, the `_rdlink._tcp` advert with the TXT the iPhone checks, and
//! a Rapport server for its pair-setup (password type 10, the QR's PIN).

use std::net::{SocketAddr, SocketAddrV6};
use std::time::{Duration, Instant};

use apple_opack::Value;
use koala_pairing::{Identity, Peer, SetupServer};
use koala_rapport::auth::serve_pairing;
use koala_rapport::message::dict;
use koala_rapport::{Connection, FrameType, Message, MessageType, RapportError};
use qrcode::render::unicode;
use qrcode::{EcLevel, QrCode};
use tokio::net::{TcpListener, TcpStream};
use warlock_client::{Client, PIPE};

use crate::ble::Advert;
use crate::capture::Local;
use crate::dnssd::Registration;
use crate::hci::HciTrace;

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
/// The Rapport service of guest pairing (`_clSrv`).
const GUEST_PAIRING_SERVICE: &str = "com.apple.ddui.guestpairing";
/// The Rapport service of a Continuity Capture session (`_clSrv`), whose
/// RemoteDisplay session start launches the iPhone's capture daemon.
const CAPTURE_SERVICE: &str = "com.apple.continuitycapture";
/// How long Phase B waits for the iPhone's RDLink listener to show up.
const PHASE_B_WAIT: Duration = Duration::from_secs(90);

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

    // Phase B's BLE advert starts now, before pairing. Windows gives an advert
    // its random address only after its data is fixed, so the advert can't be
    // tagged for Koala's IRK; instead Koala reads the address from the HCI
    // trace and chooses the IRK it will pair with to fit the advert's tag.
    let mut tag = [0u8; 3];
    crate::random(&mut tag);
    let needs_awdl = crate::nearby::needs_awdl(&tag);
    let trace = HciTrace::start()?;
    let _advert = Advert::start(&needs_awdl).map_err(|e| format!("BLE: {e}"))?;
    let sent = [&[0xff, 0x4c, 0x00][..], &needs_awdl].concat();
    let (set, ble_address) = trace
        .advert_address(&sent, Duration::from_secs(5))
        .ok_or("BLE: the HCI trace didn't show the advert's address")?;
    drop(trace);
    let irk = crate::nearby::irk_for(&ble_address, &tag);
    let shown = ble_address.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
    let hex: String = needs_awdl.iter().map(|b| format!("{b:02x}")).collect();
    println!("BLE: broadcasting 4c00{hex} from {shown} (advertising set {set}); IRK chosen to match");

    // Who Koala is to the iPhone: a UUID identifier (the QR's `_pubID`), a
    // key pair, and CoreUtils' info block with that IRK, a model and a name.
    let public_id = crate::uuid();
    let info_block =
        dict(vec![("altIRK", Value::Bytes(irk.to_vec())), ("model", MODEL.into()), ("name", "Koala".into())]);
    let info_block = apple_opack::pack(&info_block).map_err(|e| e.to_string())?;
    let identity = Identity::generate(public_id.as_bytes()).with_info(info_block);
    let pairing_id = crate::uuid();
    let pin = crate::uuid();
    // Our Rapport identifier (`_i`, `_idsID`), which an iPhone uses for a
    // session-paired peer in place of an IDS device ID.
    let rapport_id = crate::uuid();

    // Where the iPhone connects: the adapter's link-local address.
    let address = SocketAddr::V6(SocketAddrV6::new(adapter.address, 0, 0, adapter.index));
    // This end, for the capture session's streams: that address, and the
    // AWDL MAC address Warlock's radio uses.
    let mac = info
        .radio
        .address
        .as_deref()
        .and_then(|a| a.split(':').map(|b| u8::from_str_radix(b, 16).ok()).collect::<Option<Vec<u8>>>())
        .and_then(|b| <[u8; 6]>::try_from(b).ok())
        .ok_or("Warlock's radio has no AWDL address yet")?;
    let local = Local { address: SocketAddrV6::new(adapter.address, 0, 0, adapter.index), mac };
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
    let end = tokio::time::Instant::now() + run_for;
    let deadline = tokio::time::sleep_until(end);
    tokio::pin!(deadline);
    let mut last_advertising = String::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, from) = accepted.map_err(|e| e.to_string())?;
                println!("\nconnection from {from}");
                let setup = || Some(SetupServer::new(identity.clone(), pin.as_str()).with_m2_items(vec![(27, vec![1])]));
                // What we answer `_systemInfo` with: the keys the iPhone's own
                // request carries, with our values.
                let system_info = |service: &str| dict(vec![
                    ("_i", rapport_id.as_str().into()),
                    ("_idsID", rapport_id.as_str().into()),
                    ("_pubID", rp_ba.as_str().into()),
                    ("_spID", public_id.as_str().into()),
                    ("_sv", RAPPORT_VERSION.into()),
                    ("_bf", Value::int(0)),
                    ("_cf", Value::int(512)),
                    ("_clSrv", service.into()),
                    ("model", MODEL.into()),
                    ("name", "Koala".into()),
                ]);
                let infos = (system_info(GUEST_PAIRING_SERVICE), system_info(CAPTURE_SERVICE));
                // The iPhone may keep this connection open: the run's end ends it too.
                let conversation = converse(stream, from, &identity, setup, &mut peers, local, infos);
                if tokio::time::timeout_at(end, conversation).await.is_err() {
                    println!("giving up after {seconds} s, in a conversation");
                    break;
                }
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
/// Once the iPhone's `_systemInfo` is answered it is in its mic-only
/// session, and [`phase_b`] starts alongside.
async fn converse(
    stream: TcpStream,
    from: SocketAddr,
    identity: &Identity,
    setup: impl Fn() -> Option<SetupServer>,
    peers: &mut Vec<Peer>,
    local: Local,
    (guest_info, capture_info): (Value, Value),
) {
    let mut connection = Connection::new(stream).with_trace(format!("[{from}]"));
    let known = peers.len();
    let result = serve_pairing(&mut connection, identity, setup, peers).await;
    let iphone = peers[known..].first().cloned();
    let mut phase_b_started = false;
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
        Ok(who) => {
            println!("AUTHENTICATED {} by {:?}: encrypted from here", String::from_utf8_lossy(&who.peer_id), who.method)
        }
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
                    let is_system_info = message.id.as_deref() == Some("_systemInfo");
                    let content = if is_system_info { guest_info.clone() } else { dict(vec![]) };
                    let reply = message.reply(content);
                    if let Err(e) = connection.send_opack(FrameType::E_OPACK, &reply).await {
                        println!("answering failed: {e}");
                        return;
                    }
                    if is_system_info
                        && !phase_b_started
                        && let (SocketAddr::V6(address), Some(iphone)) = (from, iphone.clone())
                    {
                        phase_b_started = true;
                        tokio::spawn(phase_b(address, local, identity.clone(), iphone, capture_info.clone()));
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

/// Phase B (`docs/guest-pairing.md`): with the BLE advert asking the iPhone
/// onto AWDL already running (see [`run`]), watch for its `_rdlink._tcp`
/// listener in its AWDL frames (Warlock's view of it), then start the
/// capture session there.
async fn phase_b(iphone: SocketAddrV6, local: Local, identity: Identity, peer: Peer, info: Value) {
    let warlock = Client::new(PIPE);
    let mac = mac_from_link_local(iphone.ip());
    let started = Instant::now();
    let mut last = String::new();
    while started.elapsed() < PHASE_B_WAIT {
        match warlock.peer(&mac).await {
            Ok(seen) => {
                let records = format!("{:?}", seen.services.records);
                if records != last {
                    println!("the iPhone's AWDL records, {:.1} s in: {records}", started.elapsed().as_secs_f32());
                    last = records;
                }
                let rdlink =
                    seen.services.records.iter().find(|r| r.rtype == "SRV" && r.name.contains("._rdlink._tcp"));
                if let Some(port) = rdlink.and_then(|r| r.port) {
                    println!("the iPhone's RDLink listener: port {port}");
                    let mut listener = iphone;
                    listener.set_port(port);
                    crate::capture::run(listener, local, identity, peer, info).await;
                    return;
                }
            }
            Err(e) => {
                let gone = format!("gone: {e}");
                if gone != last {
                    println!("Warlock doesn't hear the iPhone now ({e})");
                    last = gone;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    println!("no RDLink listener from the iPhone in {} s", PHASE_B_WAIT.as_secs());
}

/// The MAC address in an EUI-64 link-local address, as Warlock names peers.
fn mac_from_link_local(address: &std::net::Ipv6Addr) -> String {
    let o = address.octets();
    let mac = [o[8] ^ 0x02, o[9], o[10], o[13], o[14], o[15]];
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// Prints the QR code (light on dark, as terminals are), saves it as a
/// bitmap, and opens that in Photos.
fn show_qr(url: &str) -> Result<(), String> {
    let code = QrCode::with_error_correction_level(url, EcLevel::L).map_err(|e| e.to_string())?;
    let terminal = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    // A new file each time: Photos keeps the last one open, and locked.
    let mut nonce = [0u8; 4];
    crate::random(&mut nonce);
    let name = format!("koala-guest-qr-{}.bmp", nonce.iter().map(|b| format!("{b:02x}")).collect::<String>());
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, bitmap(&code, 12)).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("\n{terminal}\n");
    println!("Scan with the iPhone's camera ({}): {url}", path.display());
    // On screen, where a camera can see it.
    let photos = format!("ms-photos:viewer?fileName={}", path.display());
    let _ = std::process::Command::new("explorer.exe").arg(photos).spawn();
    Ok(())
}

/// The QR code as a 24-bit BMP, `scale` pixels a module, with the standard
/// four-module quiet zone.
fn bitmap(code: &QrCode, scale: usize) -> Vec<u8> {
    const QUIET: usize = 4;
    let modules = code.width();
    let colors = code.to_colors();
    let side = (modules + 2 * QUIET) * scale;
    let row_len = (side * 3).div_ceil(4) * 4;
    let image_len = row_len * side;
    let mut out = Vec::with_capacity(54 + image_len);
    // BITMAPFILEHEADER, then BITMAPINFOHEADER.
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + image_len) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(side as i32).to_le_bytes());
    out.extend_from_slice(&(side as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(image_len as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    // Rows run bottom to top.
    for y in (0..side).rev() {
        let row_start = out.len();
        for x in 0..side {
            let (mx, my) = ((x / scale).checked_sub(QUIET), (y / scale).checked_sub(QUIET));
            let dark = match (mx, my) {
                (Some(mx), Some(my)) if mx < modules && my < modules => {
                    colors[my * modules + mx] == qrcode::Color::Dark
                }
                _ => false,
            };
            out.extend_from_slice(if dark { &[0, 0, 0] } else { &[255, 255, 255] });
        }
        out.resize(row_start + row_len, 0);
    }
    out
}
