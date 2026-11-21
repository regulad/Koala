//! The capture session, on the iPhone's RDLink listener: pair-verify as the
//! paired Apple TV, start a RemoteDisplay session for Continuity Capture, and
//! serve the streams it asks for (`docs/guest-pairing.md`, "Phase B").
//!
//! The RemoteDisplay connection carries the session's events and the
//! iPhone's `_streamStart` requests. Control, Command and Data then get
//! Rapport connections of their own, which the iPhone opens to Koala once it
//! has the answer; the microphone gets a UDP path, where its media arrives.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{SocketAddr, SocketAddrV6};
use std::time::{Duration, Instant};

use apple_opack::Value;
use koala_capture::avc::{AnswerSettings, Offer, answer, ntp_now};
use koala_capture::rtcp::{self, ReceiverStats};
use koala_capture::session::{
    ARGS, Configuration, MESSAGE_TYPE, SELECTOR, SESSION_EVENT, STREAM_EVENT, StreamFormat, control, message,
    session_start, start_stream, stream as name, stream_activated, stream_announce, streams,
};
use koala_capture::srtp::{self, Rollover};
use koala_capture::streams::{
    STREAM_ID, STREAM_KEY, STREAM_START, STREAM_TYPE, TYPE_RAPPORT, TYPE_UDP, rapport_stream_answer,
    rapport_stream_keys, udp_stream_answer,
};
use koala_pairing::{Identity, Peer};
use koala_rapport::auth::{AUTH_TYPE_SESSION_PAIRED, pair_verify};
use koala_rapport::cipher::{Cipher, Role};
use koala_rapport::message::dict;
use koala_rapport::{Connection, FrameType, Message, MessageType};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use crate::camera::{self, Recording};
use crate::guest::{Event, Report};
use crate::mic::{Microphone, Model};

/// This end: the Warlock adapter's link-local address (with its interface
/// as scope) and Warlock's AWDL MAC address.
#[derive(Clone, Copy, Debug)]
pub struct Local {
    pub address: SocketAddrV6,
    pub mac: [u8; 6],
}

/// The ProductType Koala presents in the capture session, and its OS build
/// for AVConference's call info: a real pair, an M1 MacBook Air on macOS
/// 27.0.1. A Mac model (anything containing "Mac") makes the session's device
/// model 1, which skips a TV's placement step; with `_rdsr` 1, not 3, the
/// iPhone shows the Continuity Camera shield, not Sing's (`docs/guest-pairing.md`).
pub(crate) const MODEL: &str = "MacBookAir10,1";
const OS_BUILD: &str = "26A434";
/// The payload types Koala answers the AVC offer with, the first offered
/// winning. tvOS answers 101 (codec 16, which is also what a Mac asks for in
/// 'call' mode); 119 is the iPhone's own preference for 4-channel 'call'.
/// `KOALA_AVC_PAYLOAD` puts another first, for trials.
const PAYLOADS: [u8; 2] = [101, 119];
/// How long the iPhone has to connect a Rapport stream once answered.
const STREAM_CONNECT: Duration = Duration::from_secs(5);
/// How often Koala sends RTCP: about as often as the iPhone does (trial 21).
/// The iPhone ends the session after 12 s without any.
const RTCP_INTERVAL: Duration = Duration::from_secs(1);
/// The microphone's RTP clock: its timestamps advance 480 per 10 ms packet
/// (trial 21).
const MIC_CLOCK_RATE: u32 = 48_000;
/// The camera's RTP clock: 24 kHz, not RTP video's usual 90 (trial 23).
const VIDEO_CLOCK_RATE: u32 = koala_capture::video::CLOCK_RATE;

/// The session on the iPhone's RDLink listener at `iphone`, followed for
/// `limit` or until it ends. `info` is Koala's `_systemInfo` content for
/// `com.apple.continuitycapture`. Dropping the future ends the session's
/// streams too.
pub async fn run(
    iphone: SocketAddrV6,
    local: Local,
    identity: Identity,
    peer: Peer,
    info: Value,
    limit: Option<Duration>,
    report: Report,
) {
    let port = iphone.port();
    // The session, timed for the diagnostics: its kind, how it went and how
    // long it lasted, and nothing about the iPhone but its model.
    let kind = if camera::RECORD.get().is_some() { "microphone+camera" } else { "microphone" };
    let mut timed = crate::diagnostics::Timed::start(kind, "capture.session");
    let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(iphone)).await else {
        timed.fail("no-connection");
        crate::diagnostics::error("capture: no connection to the iPhone's RDLink listener");
        return println!("capture: no connection to port {port}");
    };
    let mut connection = Connection::new(stream).with_trace(format!("[iPhone port {port}]"));
    let known = |id: &[u8]| (id == peer.id.as_slice()).then_some(peer.public_key);
    let expected = Some((peer.id.clone(), peer.public_key));
    let verify = pair_verify(&mut connection, identity.clone(), known, expected, AUTH_TYPE_SESSION_PAIRED);
    match tokio::time::timeout(Duration::from_secs(5), verify).await {
        Ok(Ok(_)) => {
            crate::diagnostics::step("capture: pair-verified");
            println!("capture: pair-verified");
        }
        Ok(Err(e)) => {
            timed.fail("pair-verify");
            crate::diagnostics::error(&format!("capture: pair-verify failed: {e}"));
            return println!("capture: pair-verify failed: {e}");
        }
        Err(_) => {
            timed.fail("pair-verify");
            crate::diagnostics::error("capture: no answer to pair-verify");
            return println!("capture: no answer to pair-verify");
        }
    }
    let mut request = Message::request("_systemInfo", 1, info);
    if let Value::Dict(pairs) = &mut request {
        pairs.push(("_btHP".into(), false.into()));
    }
    if let Err(e) = connection.send_opack(FrameType::E_OPACK, &request).await {
        timed.fail("system-info");
        crate::diagnostics::error(&format!("capture: sending _systemInfo failed: {e}"));
        return println!("capture: sending _systemInfo failed: {e}");
    }

    // Koala's SSRC, for the AVC answers and its RTCP alike.
    let mut ssrc = [0u8; 4];
    crate::random(&mut ssrc);
    // The microphones, and with `--camera` the camera too, in the one
    // session: each entity its own configuration and session ID, which salts
    // its stream's SRTP key (`docs/guest-pairing.md`, "Both at once").
    let mut configurations = vec![Configuration::microphone(uuid_v4(), 1)];
    if camera::RECORD.get().is_some() {
        configurations.push(Configuration::camera(uuid_v4(), 1, StreamFormat::hd_1080p30()));
    }
    let mut session = Session {
        connection,
        local,
        ssrc: u32::from_be_bytes(ssrc),
        model: Model::default(),
        configurations,
        started: false,
        activated: BTreeSet::new(),
        start_command: None,
        udp_stop: Vec::new(),
        streams: JoinSet::new(),
        report,
        xid: 100,
    };
    let ended = match limit {
        Some(limit) => tokio::time::timeout(limit, session.follow()).await.unwrap_or("time-limit"),
        None => session.follow().await,
    };
    timed.set("streams_up", session.activated.len());
    timed.set("started", session.started);
    if ended == "error" {
        timed.fail(ended);
    } else {
        timed.set("ended", ended);
    }
}

/// A fresh random UUID (version 4), as a configuration's session ID.
fn uuid_v4() -> [u8; 16] {
    let mut id = [0u8; 16];
    crate::random(&mut id);
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    id
}

struct Session {
    connection: Connection<TcpStream>,
    local: Local,
    /// Koala's SSRC in the media session.
    ssrc: u32,
    /// The iPhone's model, from its AVC offer on Control.
    model: Model,
    /// The entities asked for, each with its own configuration: the
    /// microphones first (the one started early), then the camera.
    configurations: Vec<Configuration>,
    started: bool,
    /// Streams whose connection or path is up.
    activated: BTreeSet<String>,
    /// Sends the StartStreams, one per entity, on the Command stream, once
    /// every stream is up.
    start_command: Option<oneshot::Sender<Vec<Value>>>,
    /// Held for the UDP paths' tasks: dropped with the session, they stop.
    udp_stop: Vec<oneshot::Sender<()>>,
    /// The streams' tasks, ended with the session.
    streams: JoinSet<()>,
    report: Report,
    xid: u64,
}

impl Session {
    /// Follows the session until it ends: how, as a word for the diagnostics
    /// (`closed`: the iPhone ended it; `error`: Koala couldn't go on).
    async fn follow(&mut self) -> &'static str {
        loop {
            let value = match self.connection.receive_opack().await {
                Ok((FrameType::E_OPACK, value)) => value,
                Ok(_) => continue,
                Err(e) => {
                    println!("capture: the session ended: {e}");
                    return "closed";
                }
            };
            let Some(message) = Message::parse(value) else { continue };
            if let Err(e) = self.handle(message).await {
                crate::diagnostics::error(&format!("capture: {e}"));
                println!("capture: {e}");
                return "error";
            }
        }
    }

    async fn handle(&mut self, message: Message) -> Result<(), String> {
        match (message.kind, message.id.as_deref()) {
            // The answer to `_systemInfo`: the session's go.
            (MessageType::Response, _) if message.xid == Some(1) && !self.started => {
                self.started = true;
                self.event(SESSION_EVENT, session_start(1, &self.configurations)).await?;
                crate::diagnostics::step("capture: session start sent");
                println!("capture: session start sent");
            }
            (MessageType::Event, Some(SESSION_EVENT)) => {
                let kind = message.content.get(MESSAGE_TYPE).and_then(Value::as_u64);
                println!("capture: session event {kind:?}: {:?}", message.content);
                // Started: announce each stream again, as a tvOS host does.
                if kind == Some(message::SESSION_STARTED) {
                    for (stream, media) in streams(&self.configurations) {
                        self.event(SESSION_EVENT, stream_announce(stream, media)).await?;
                    }
                }
            }
            (MessageType::Request, Some(STREAM_START)) => self.stream_start(&message).await?,
            (MessageType::Request, _) => self.reply(&message, dict(vec![])).await?,
            _ => {}
        }
        Ok(())
    }

    /// Serves one `_streamStart`: answers it, and brings the stream up.
    async fn stream_start(&mut self, message: &Message) -> Result<(), String> {
        let content = &message.content;
        let Some(stream) = content.get(STREAM_ID).and_then(Value::as_str).map(str::to_owned) else {
            return self.reply(message, dict(vec![])).await;
        };
        let mut listen = self.local.address;
        listen.set_port(0);
        match content.get(STREAM_TYPE).and_then(Value::as_u64) {
            Some(TYPE_RAPPORT) => {
                let Some(client) =
                    content.get(STREAM_KEY).and_then(Value::as_bytes).and_then(|k| <[u8; 32]>::try_from(k).ok())
                else {
                    println!("capture: stream {stream}: no 32-byte _streamKey");
                    return self.reply(message, dict(vec![])).await;
                };
                let mut random = [0u8; 32];
                crate::random(&mut random);
                let (host_public, psk) = rapport_stream_keys(&client, random);
                let listener = TcpListener::bind(listen).await.map_err(|e| format!("stream {stream}: {e}"))?;
                let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                println!("capture: stream {stream}: Rapport, port {port}");
                self.reply(message, rapport_stream_answer(&host_public, port)).await?;
                // The iPhone connects as soon as it has the answer.
                match tokio::time::timeout(STREAM_CONNECT, listener.accept()).await {
                    Ok(Ok((tcp, from))) => {
                        println!("capture: stream {stream}: connected from {from}");
                        let (start_tx, start_rx) = oneshot::channel();
                        if stream == name::COMMAND {
                            self.start_command = Some(start_tx);
                        }
                        self.streams.spawn(rapport_stream(
                            stream.clone(),
                            tcp,
                            psk,
                            start_rx,
                            self.ssrc,
                            self.model.clone(),
                        ));
                        self.activated(&stream).await?;
                    }
                    Ok(Err(e)) => println!("capture: stream {stream}: accepting failed: {e}"),
                    Err(_) => println!("capture: stream {stream}: the iPhone didn't connect"),
                }
            }
            Some(TYPE_UDP) => {
                let socket = UdpSocket::bind(listen).await.map_err(|e| format!("stream {stream}: {e}"))?;
                let port = socket.local_addr().map_err(|e| e.to_string())?.port();
                let mut key = [0u8; 32];
                crate::random(&mut key);
                let address = self.local.address.ip().to_string();
                println!(
                    "capture: stream {stream}: UDP, port {port}; the iPhone's end {:?}",
                    content.get("_streamAddr")
                );
                // Salted with its own entity's session ID: the microphone's
                // with the microphone's, the camera's with the camera's.
                let Some(configuration) = self.configurations.iter().find(|c| c.media_stream() == stream) else {
                    println!("capture: stream {stream}: no entity asked for uses it");
                    return self.reply(message, dict(vec![])).await;
                };
                let keys = srtp::Context::for_stream(&key, &configuration.session_id);
                let (stop_tx, stop_rx) = oneshot::channel();
                self.udp_stop.push(stop_tx);
                let (model, report) = (self.model.clone(), self.report.clone());
                self.streams.spawn(udp_stream(stream.clone(), socket, keys, self.ssrc, model, report, stop_rx));
                self.reply(message, udp_stream_answer(&address, self.local.mac, &key, port)).await?;
                self.activated(&stream).await?;
            }
            other => {
                println!("capture: stream {stream}: type {other:?} isn't served");
                self.reply(message, dict(vec![])).await?;
            }
        }
        Ok(())
    }

    async fn activated(&mut self, stream: &str) -> Result<(), String> {
        self.event(SESSION_EVENT, stream_activated(stream)).await?;
        self.activated.insert(stream.to_string());
        let all = streams(&self.configurations).len();
        println!("capture: stream {stream} activated ({} of {all})", self.activated.len());
        if self.activated.len() == all
            && let Some(start) = self.start_command.take()
        {
            // One StartStream per entity, back to back: the iPhone queues
            // them, and starts each when the one before is done.
            let starts = (1..)
                .zip(&self.configurations)
                .map(|(xid, c)| Message::event(STREAM_EVENT, xid, start_stream(c, 0)))
                .collect();
            let _ = start.send(starts);
            crate::diagnostics::step("capture: every stream is up");
            println!("capture: every stream is up: {} StartStream on Command", self.configurations.len());
        }
        Ok(())
    }

    async fn event(&mut self, id: &str, content: Value) -> Result<(), String> {
        self.xid += 1;
        let event = Message::event(id, self.xid, content);
        self.connection.send_opack(FrameType::E_OPACK, &event).await.map_err(|e| e.to_string())
    }

    async fn reply(&mut self, message: &Message, content: Value) -> Result<(), String> {
        self.connection.send_opack(FrameType::E_OPACK, &message.reply(content)).await.map_err(|e| e.to_string())
    }
}

/// One Rapport stream: the iPhone's connection, encrypted from its first
/// frame with the stream's pre-shared key. Logs what it carries, answers
/// requests empty and (on Control) the AVC offer, keeping the iPhone's model
/// from it, and sends `start` (the Command stream's StartStream) when it's
/// given.
async fn rapport_stream(
    stream: String,
    tcp: TcpStream,
    psk: [u8; 32],
    start: oneshot::Receiver<Vec<Value>>,
    ssrc: u32,
    model: Model,
) {
    let mut connection = Connection::new(tcp).with_trace(format!("[{stream}]"));
    connection.encrypt(Cipher::new(&psk, Role::Server));
    // Only Command gets StartStreams (xids 1 on), one per entity; the
    // others' senders are dropped.
    let mut xid = 1;
    if let Ok(starts) = start.await {
        for value in &starts {
            if let Err(e) = connection.send_opack(FrameType::E_OPACK, value).await {
                return println!("capture: stream {stream}: {e}");
            }
        }
        xid = starts.len() as u64;
    }
    loop {
        let value = match connection.receive_opack().await {
            Ok((FrameType::E_OPACK, value)) => value,
            Ok(_) => continue,
            Err(e) => return println!("capture: stream {stream} ended: {e}"),
        };
        let Some(message) = Message::parse(value) else { continue };
        let out = match message.kind {
            MessageType::Request => message.reply(dict(vec![])),
            MessageType::Event if stream == name::CONTROL && message.id.as_deref() == Some(STREAM_EVENT) => {
                let Some(content) = avc_answer(&message.content, ssrc, &model) else { continue };
                xid += 1;
                Message::event(STREAM_EVENT, xid, content)
            }
            _ => continue,
        };
        if let Err(e) = connection.send_opack(FrameType::E_OPACK, &out).await {
            return println!("capture: stream {stream}: {e}");
        }
    }
}

/// Koala's answer to the iPhone's AVConference offer (Control selector 2,
/// `[entity, offer]`), as the stream event's content: selector 2 again, with
/// `[entity, answer]`. `None` for any other Control event. Keeps the
/// iPhone's model, which names its microphones. A camera's offer (one with
/// video settings) is answered with its own video settings, and kept beside
/// the recording.
fn avc_answer(content: &Value, ssrc: u32, model: &Model) -> Option<Value> {
    if content.get(SELECTOR).and_then(Value::as_u64) != Some(control::AVC_NEGOTIATION) {
        return None;
    }
    let Some(Value::Array(args)) = content.get(ARGS) else {
        println!("capture: an AVC offer without arguments");
        return None;
    };
    let (Some(entity), Some(data)) = (args.first().and_then(Value::as_u64), args.get(1).and_then(Value::as_bytes))
    else {
        println!("capture: an AVC offer that isn't [entity, data]");
        return None;
    };
    let offer = Offer::parse(data).map_err(|e| println!("capture: the AVC offer: {e}")).ok()?;
    if let Some(m) = offer.model() {
        let _ = model.set(m.to_owned());
    }
    // Its model and OS build, and nothing else of it (the call info also
    // has its name): the diagnostics' only facts about the iPhone.
    crate::diagnostics::iphone(offer.model(), offer.os_build());
    let payload = if offer.video().is_some() {
        camera::save_offer(data);
        println!(
            "capture: AVC offer for entity {entity}, the camera: mode {:?}, model {:?}, video settings {}",
            offer.mode,
            offer.model(),
            offer.describe_video()
        );
        // Unused: the camera's answer mirrors its video settings.
        0
    } else {
        let offered = offer.payloads();
        println!(
            "capture: AVC offer for entity {entity}: mode {:?}, payloads {offered:?}, the iPhone's SSRC {:?}, model {:?}",
            offer.mode,
            offer.ssrc(),
            offer.model()
        );
        let wanted = std::env::var("KOALA_AVC_PAYLOAD").ok().and_then(|p| p.parse::<u8>().ok());
        let Some(payload) = wanted.into_iter().chain(PAYLOADS).find(|p| offered.contains(p)) else {
            println!("capture: none of the payloads Koala answers with was offered");
            return None;
        };
        payload
    };
    let settings = AnswerSettings { payload, ssrc, ntp_time: ntp_now(), device_type: MODEL, os_version: OS_BUILD };
    let bytes = answer(&offer, &settings).map_err(|e| println!("capture: the AVC answer: {e}")).ok()?;
    let chosen =
        if offer.video().is_some() { "the camera's settings mirrored".into() } else { format!("payload {payload}") };
    println!("capture: AVC answer: {chosen}, Koala's SSRC {:#010x}, {} bytes", settings.ssrc, bytes.len());
    Some(dict(vec![
        (SELECTOR, Value::int(control::AVC_NEGOTIATION)),
        (ARGS, Value::Array(vec![Value::int(entity), Value::Bytes(bytes)])),
    ]))
}

/// What a media stream's packets go to.
enum Media {
    Microphone(Microphone),
    Camera(Box<Recording>),
}

impl Media {
    fn for_stream(stream: &str, model: Model) -> Self {
        if stream == name::DEFAULT_VIDEO {
            Self::Camera(Box::new(Recording::open()))
        } else {
            Self::Microphone(Microphone::new(Microphone::output(), model))
        }
    }

    fn clock_rate(&self) -> u32 {
        match self {
            Self::Microphone(_) => MIC_CLOCK_RATE,
            Self::Camera(_) => VIDEO_CLOCK_RATE,
        }
    }

    /// One decrypted RTP packet. True for the first that makes media.
    fn packet(&mut self, seq: u16, clear: &[u8]) -> bool {
        match self {
            Self::Microphone(mic) => {
                let header = srtp::rtp_header_len(clear).unwrap_or(clear.len());
                let before = mic.decoded;
                mic.packet(seq, &clear[header..]);
                before == 0 && mic.decoded == 1
            }
            Self::Camera(video) => {
                video.packet(clear);
                video.packets == 1
            }
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::Microphone(mic) => {
                format!("audio: {} decoded, {} concealed, {} errors", mic.decoded, mic.concealed, mic.errors)
            }
            Self::Camera(video) => video.summary(),
        }
    }
}

/// A media stream's UDP path. Verifies and decrypts the iPhone's SRTP and
/// keeps receiver statistics on it, reads its SRTCP sender reports, and
/// answers once a second with an SRTCP receiver report, without which the
/// iPhone ends the session at 12 s. RTP and RTCP share the port, both ways.
/// The first media is reported as [`Event::Streaming`].
/// The reports leave from this socket, bound to Warlock's link-local
/// address: left to choose, Windows sent to the iPhone's `llw0` address
/// from another adapter's ULA (trial 21).
async fn udp_stream(
    stream: String,
    socket: UdpSocket,
    keys: srtp::Context,
    ssrc: u32,
    model: Model,
    report: Report,
    mut stop: oneshot::Receiver<()>,
) {
    let mut buffer = vec![0u8; 2048];
    let (mut packets, mut refused, mut reports_in) = (0u64, 0u64, 0u64);
    let mut reports_out = 0u32;
    let mut peer: Option<SocketAddr> = None;
    let mut stats: Option<ReceiverStats> = None;
    // Each SSRC's sequence numbers roll over on their own.
    let mut rollovers: BTreeMap<u32, Rollover> = BTreeMap::new();
    let mut media = Media::for_stream(&stream, model);
    let mut tick = tokio::time::interval(RTCP_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // The session is over: so are its reports, and the recording.
            _ = &mut stop => {
                return println!("capture: stream {stream} closed: {}", media.summary());
            }
            received = socket.recv_from(&mut buffer) => {
                let (len, from) = match received {
                    Ok(r) => r,
                    Err(e) => return println!("capture: stream {stream}: {e}"),
                };
                let data = &buffer[..len];
                peer = Some(from);
                if srtp::is_rtcp(data) {
                    match keys.unprotect_rtcp(data) {
                        Ok((rtcp, index)) => {
                            reports_in += 1;
                            for (sender, ntp) in rtcp::sender_reports(&rtcp) {
                                if let Some(s) = stats.as_mut().filter(|s| s.ssrc == sender) {
                                    s.sender_report(ntp, Instant::now());
                                }
                            }
                            if reports_in <= 2 || reports_in.is_power_of_two() {
                                println!("capture: stream {stream}: SRTCP {index} from {from}, {} bytes", rtcp.len());
                            }
                        }
                        Err(e) => println!("capture: stream {stream}: SRTCP from {from} refused: {e}"),
                    }
                    continue;
                }
                packets += 1;
                if len < 12 {
                    continue;
                }
                let seq = u16::from_be_bytes([data[2], data[3]]);
                let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                let source = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
                let rollover = rollovers.entry(source).or_default();
                let roc = rollover.guess(seq);
                match keys.unprotect_rtp(data, roc) {
                    Ok(clear) => {
                        rollover.update(seq, roc);
                        let clock_rate = media.clock_rate();
                        let s = stats.get_or_insert_with(|| ReceiverStats::new(source, clock_rate, seq));
                        if s.ssrc == source {
                            s.received(seq, timestamp, Instant::now());
                        }
                        // The camera's every packet is kept, whatever its
                        // SSRC; the microphone's from the first SSRC only.
                        let first = match &media {
                            Media::Camera(_) => true,
                            Media::Microphone(_) => s.ssrc == source,
                        };
                        if first && media.packet(seq, &clear) {
                            crate::diagnostics::step("capture: the first media arrived");
                            report.send(Event::Streaming);
                        }
                        if packets <= 3 || packets.is_power_of_two() {
                            println!(
                                "capture: stream {stream}: packet {packets} from {from}: SSRC {source:#010x}, seq {seq}, \
                                 {} bytes decrypted",
                                clear.len()
                            );
                        }
                    }
                    Err(e) => {
                        refused += 1;
                        if refused <= 3 || refused.is_power_of_two() {
                            println!("capture: stream {stream}: packet {packets} from {from} refused ({refused} so far): {e}");
                        }
                    }
                }
            }
            _ = tick.tick() => {
                let (Some(to), Some(s)) = (peer, stats.as_mut()) else { continue };
                reports_out += 1;
                let report = rtcp::receiver_report(ssrc, &[s.report_block(Instant::now())]);
                let packet = keys.protect_rtcp(&report, reports_out);
                if let Err(e) = socket.send_to(&packet, to).await {
                    println!("capture: stream {stream}: sending SRTCP to {to}: {e}");
                    continue;
                }
                if reports_out <= 3 || reports_out.is_power_of_two() {
                    println!(
                        "capture: stream {stream}: SRTCP {reports_out} to {to}: receiver report, {} packets received; {}",
                        s.packets(),
                        media.summary()
                    );
                }
            }
        }
    }
}
