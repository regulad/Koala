//! Playing an Apple TV offering guest pairing: `koala guest`, and the tray's
//! Connect.
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
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;
use warlock_client::{Client, PIPE};

use warlock_client::Lines;
use warlock_client::types::{ServiceRequest, ServiceRequestState};

use crate::capture::Local;
use crate::diagnostics;
use crate::dnssd::Registration;

/// The ProductType Koala claims while pairing: an Apple TV 4K (3rd
/// generation), whose tvOS 27 offers guest pairing. (The capture session
/// presents a Mac: `capture::MODEL`.)
const MODEL: &str = "AppleTV14,1";
/// `_rdsr`, the discovery session's start reason: 1, OnCommunal. Not 3,
/// MicOnly: that sends the iPhone down Apple Music Sing's path, which needs
/// a media route to a real TV and ends in "Unable to connect" (trial 13,
/// `docs/guest-pairing.md`).
const START_REASON: u8 = 1;
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
/// The service Koala hosts, and asks the iPhone for in Phase B.
const RDLINK: &str = "_rdlink._tcp";
/// How long Warlock has to start asking for the iPhone's RDLink.
const ASK_WAIT: Duration = Duration::from_secs(30);
/// How long Phase B waits for the iPhone's RDLink listener to show up.
const PHASE_B_WAIT: Duration = Duration::from_secs(90);
/// "Until cancelled", as a wait: a century.
const FOREVER: Duration = Duration::from_secs(100 * 365 * 24 * 3600);

/// What a run reports as it goes, for the tray.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The QR code to show: its URL.
    Qr(String),
    /// An iPhone paired: its name, from its pair-setup info.
    Paired(Option<String>),
    /// The microphone's audio is arriving.
    Streaming,
    /// The iPhone's user paused the session (`true`) on the phone, or
    /// resumed it (`false`).
    Paused(bool),
    /// The capture side is over: `None` when a session ended, or why there
    /// was none.
    Ended(Option<String>),
}

/// Where a run's [`Event`]s go, if anywhere.
#[derive(Clone, Debug, Default)]
pub struct Report(Option<UnboundedSender<Event>>);

impl Report {
    pub fn to(events: UnboundedSender<Event>) -> Self {
        Self(Some(events))
    }

    pub fn send(&self, event: Event) {
        if let Some(events) = &self.0 {
            let _ = events.send(event);
        }
    }
}

/// How a run goes.
#[derive(Clone, Debug)]
pub struct Options {
    /// How long to wait for an iPhone; `None` until the run is dropped.
    pub wait: Option<Duration>,
    /// How long to follow a capture session; `None` until it ends.
    pub session: Option<Duration>,
    /// In a terminal: print the QR code, and stop on Ctrl+C.
    pub terminal: bool,
    pub report: Report,
}

/// Waits for an iPhone for `options.wait`, then withdraws the advert and
/// returns. Dropping the run ends everything it started, a capture session
/// included.
pub async fn run(options: Options) -> Result<(), String> {
    let run_for = options.wait.unwrap_or(FOREVER);
    let warlock = Client::new(PIPE);
    let info = warlock.info().await.map_err(|e| format!("Warlock: {e}"))?;
    diagnostics::warlock(&info);
    if !info.requestable.iter().any(|s| s == RDLINK) {
        return Err(format!(
            "Warlock {} can't ask an iPhone onto AWDL; Koala needs Warlock 0.2.4 or later",
            info.version
        ));
    }
    let adapter = info.adapter.ok_or("Warlock has no network adapter yet")?;
    println!(
        "Warlock {}: {} (interface {}) at {}, radio {:?}",
        info.version, adapter.name, adapter.index, adapter.address, info.radio.state
    );

    // Phase B's ask starts now, before pairing: Warlock asks over Bluetooth
    // LE for the iPhone's RDLink on AWDL (`POST /v1/request`), in an advert
    // whose sender the IRK it gives back identifies. Koala pairs with that
    // IRK (`altIRK`), and holds the request for the run.
    // Without the needs-keyboard flag, which an iPhone doesn't need (trial 25).
    let ask = ServiceRequest { service: RDLINK.into(), reason: "Koala guest pairing".into(), ..Default::default() };
    let mut request = warlock.request(&ask).await.map_err(|e| format!("Warlock: {e}"))?;
    let irk_hex = asked_identity(&mut request).await?;
    let irk = parse_irk(&irk_hex).ok_or("Warlock gave an IRK Koala can't read")?;
    diagnostics::step("guest: Warlock is asking for the iPhone's RDLink");
    println!("Warlock is asking for the iPhone's RDLink over Bluetooth LE; pairing with the IRK it chose");

    // Who Koala is to the iPhone: a UUID identifier (the QR's `_pubID`), a
    // key pair, and CoreUtils' info block with that IRK, a model and a name.
    // The name is the PC's (Windows' computer name, as Warlock reports it):
    // the iPhone shows it as the device it's connected to.
    let name = info.host_name.clone();
    let public_id = crate::uuid();
    let info_block =
        dict(vec![("altIRK", Value::Bytes(irk.to_vec())), ("model", MODEL.into()), ("name", name.as_str().into())]);
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

    let mut advertising = warlock.advertise(RDLINK).await.map_err(|e| format!("Warlock: {e}"))?;
    let mut hold = warlock.hold("Koala guest pairing").await.map_err(|e| format!("Warlock: {e}"))?;

    let url = format!(
        "continuitycamera://connect?_pubID={public_id}&_pinC={pin}&_pi={pairing_id}&model={MODEL}&_rdsr={START_REASON}"
    );
    if options.terminal {
        show_qr(&url)?;
    }
    diagnostics::step("guest: the QR code is shown");
    options.report.send(Event::Qr(url));

    let mut peers: Vec<Peer> = Vec::new();
    // Phase B, once an iPhone is in session: ended with the run.
    let mut phase_b = JoinSet::new();
    let end = tokio::time::Instant::now() + run_for;
    let deadline = tokio::time::sleep_until(end);
    tokio::pin!(deadline);
    let mut last_advertising = String::new();
    let mut last_request = String::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, from) = accepted.map_err(|e| e.to_string())?;
                println!("\nconnection from {from}");
                let setup = || Some(SetupServer::new(identity.clone(), pin.as_str()).with_m2_items(vec![(27, vec![1])]));
                // What we answer `_systemInfo` with: the keys the iPhone's own
                // request carries, with our values. The capture session's
                // `model` is a Mac's: the iPhone takes the session's device
                // model from it, and a TV gets a placement step.
                let system_info = |service: &str, model: &str| dict(vec![
                    ("_i", rapport_id.as_str().into()),
                    ("_idsID", rapport_id.as_str().into()),
                    ("_pubID", rp_ba.as_str().into()),
                    ("_spID", public_id.as_str().into()),
                    ("_sv", RAPPORT_VERSION.into()),
                    ("_bf", Value::int(0)),
                    ("_cf", Value::int(512)),
                    ("_clSrv", service.into()),
                    ("model", model.into()),
                    ("name", name.as_str().into()),
                ]);
                let infos =
                    (system_info(GUEST_PAIRING_SERVICE, MODEL), system_info(CAPTURE_SERVICE, crate::capture::MODEL));
                // The iPhone may keep this connection open: the run's end ends it too.
                let conversation = converse(stream, from, &identity, setup, &mut peers, local, infos, &mut phase_b, &options);
                if tokio::time::timeout_at(end, conversation).await.is_err() {
                    println!("giving up after {} s, in a conversation", run_for.as_secs());
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
            line = request.next::<ServiceRequestState>() => match line {
                Some(Ok(state)) => {
                    let now = state.identity.as_ref().and_then(|i| i.irk.as_deref()) == Some(irk_hex.as_str());
                    let said = format!("asking {}, {}", state.asking, state.message.as_deref().unwrap_or("no message"));
                    if !now && said != last_request {
                        println!("Warlock's Bluetooth ask changed ({said}): an iPhone paired before no longer knows Koala");
                    }
                    last_request = said;
                }
                Some(Err(e)) => return Err(format!("Warlock: {e}")),
                None => return Err("Warlock closed the request".into()),
            },
            _ = tokio::signal::ctrl_c(), if options.terminal => break,
            _ = &mut deadline => {
                println!("giving up after {} s", run_for.as_secs());
                break;
            }
        }
    }
    Ok(())
}

/// Waits for Warlock to be asking, and gives the IRK it chose (hex).
async fn asked_identity(request: &mut Lines) -> Result<String, String> {
    let mut said = None;
    let wait = async {
        while let Some(line) = request.next::<ServiceRequestState>().await {
            let state = line.map_err(|e| format!("Warlock: {e}"))?;
            if state.message.is_some() && state.message != said {
                println!("Warlock: {}", state.message.as_deref().unwrap_or_default());
                said = state.message.clone();
            }
            if let Some(irk) = state.identity.and_then(|i| i.irk).filter(|_| state.asking) {
                return Ok(irk);
            }
        }
        Err("Warlock closed the request".to_string())
    };
    tokio::time::timeout(ASK_WAIT, wait)
        .await
        .map_err(|_| format!("Warlock didn't start asking over Bluetooth within {} s", ASK_WAIT.as_secs()))?
}

/// An IRK in hex.
fn parse_irk(hex: &str) -> Option<[u8; 16]> {
    let bytes: Vec<u8> =
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect::<Option<_>>()?;
    bytes.try_into().ok()
}

/// One connection: pairing, then whatever the peer sends, until it closes.
/// Once the iPhone's `_systemInfo` is answered it is in its mic-only
/// session, and [`phase_b`] starts alongside, in `tasks`.
#[allow(clippy::too_many_arguments)]
async fn converse(
    stream: TcpStream,
    from: SocketAddr,
    identity: &Identity,
    setup: impl Fn() -> Option<SetupServer>,
    peers: &mut Vec<Peer>,
    local: Local,
    (guest_info, capture_info): (Value, Value),
    tasks: &mut JoinSet<()>,
    options: &Options,
) {
    let mut connection = Connection::new(stream).with_trace(format!("[{from}]"));
    let known = peers.len();
    let result = serve_pairing(&mut connection, identity, setup, peers).await;
    let iphone = peers[known..].first().cloned();
    let mut phase_b_started = false;
    for peer in &peers[known..] {
        println!("PAIRED with {}", String::from_utf8_lossy(&peer.id));
        let info = peer.info.as_deref().map(apple_opack::unpack);
        match &info {
            Some(Ok((value, _))) => println!("  its info: {value:?}"),
            Some(Err(_)) => println!("  its info isn't OPACK"),
            None => {}
        }
        // Its name, as its owner set it: what the tray shows.
        let name = match info {
            Some(Ok((value, _))) => value.get("name").and_then(Value::as_str).map(str::to_owned),
            _ => None,
        };
        // Not its name: that's the tray's to show, and no diagnostics'.
        diagnostics::step("guest: paired");
        options.report.send(Event::Paired(name));
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
                        let (session, report) = (options.session, options.report.clone());
                        tasks.spawn(phase_b(
                            address,
                            local,
                            identity.clone(),
                            iphone,
                            capture_info.clone(),
                            session,
                            report,
                        ));
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
/// capture session there, following it for `session` (or until it ends).
async fn phase_b(
    iphone: SocketAddrV6,
    local: Local,
    identity: Identity,
    peer: Peer,
    info: Value,
    session: Option<Duration>,
    report: Report,
) {
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
                    diagnostics::step("guest: the iPhone's RDLink listener is up");
                    println!("the iPhone's RDLink listener: port {port}");
                    let mut listener = iphone;
                    listener.set_port(port);
                    // Its microphone streams over Low-Latency Wi-Fi only in a
                    // realtime group, which the host starts: one is held for
                    // the session (`docs/guest-pairing.md`, trials 20–21).
                    let _realtime = RealtimeGroup::hold(&warlock, &mac).await;
                    crate::capture::run(listener, local, identity, peer, info, session, report.clone()).await;
                    report.send(Event::Ended(None));
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
    let why = format!("the iPhone didn't open its capture side in {} s", PHASE_B_WAIT.as_secs());
    diagnostics::error(&format!("guest: {why}"));
    report.send(Event::Ended(Some(why)));
}

/// A Warlock realtime lock on the iPhone (`POST /v1/realtime`), held until
/// dropped. Its stream is read, as the contract asks, and its changes
/// logged: `joined` is the iPhone saying it's in the group.
struct RealtimeGroup(tokio::task::JoinHandle<()>);

impl RealtimeGroup {
    /// `None` if Warlock refuses, as it does while another program holds a
    /// group with another peer: the session then runs without LLW.
    async fn hold(warlock: &Client, mac: &str) -> Option<Self> {
        match warlock.realtime(mac, "Koala microphone").await {
            Ok(mut lines) => Some(Self(tokio::spawn(async move {
                let mut last = String::new();
                while let Some(Ok(state)) = lines.next_value().await {
                    let state = state.to_string();
                    if state != last {
                        println!("Warlock realtime group: {state}");
                        last = state;
                    }
                }
            }))),
            Err(e) => {
                println!("Warlock refused a realtime group with {mac}: {e}");
                None
            }
        }
    }
}

impl Drop for RealtimeGroup {
    fn drop(&mut self) {
        // Ending the task closes the lock's connection: Warlock asks the
        // iPhone to stop.
        self.0.abort();
    }
}

/// The MAC address in an EUI-64 link-local address, as Warlock names peers.
fn mac_from_link_local(address: &std::net::Ipv6Addr) -> String {
    let o = address.octets();
    let mac = [o[8] ^ 0x02, o[9], o[10], o[13], o[14], o[15]];
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// Prints the QR code, light on dark as terminals are. The terminal render
/// is all: the iPhone scans it from the screen (Parker, 2026-11-20).
fn show_qr(url: &str) -> Result<(), String> {
    let code = QrCode::with_error_correction_level(url, EcLevel::L).map_err(|e| e.to_string())?;
    let terminal = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    println!("\n{terminal}\n");
    println!("Scan with the iPhone's camera: {url}");
    Ok(())
}
