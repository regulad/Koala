# Guest Continuity Camera: how an iPhone pairs with an Apple TV it doesn't own

On tvOS 27, FaceTime's camera picker offers "Other…": the Apple TV shows a QR
code, a guest scans it with their iPhone's camera, and the iPhone becomes the
TV's camera or, in mic-only mode, its microphone. Koala plays the Apple TV's
part, so this document is what Koala has to reproduce.

**Source:** static analysis of tvOS 27.2 (24K122, AppleTV14,1) and iOS 27.2.1
(24B156, iPhone17,1). It covers the dyld shared caches (DeviceDiscoveryUI,
BarcodeSupport, Rapport, CMContinuityCaptureHost, CMContinuityCaptureRemote)
and both standalone `rapportd` binaries, which build from the same Rapport
version (715.2).

- **Confirmed** means read from code; the function, and the instruction or
  string that shows it, are named.
- **Inferred** means a reasoned conclusion that wasn't read directly.
- Addresses are those builds' virtual addresses: shared-cache addresses for
  frameworks, `0x1000…` for `rapportd`.

## Verdict: go

Nothing in the flow needs Apple-signed material. No IDS identity, account,
certificate, MFi, FairPlay, attestation or model allow-list appears on either
side's path. Every value the iPhone checks is one the host chooses:

- three random UUID strings;
- a model string;
- a start reason;
- an Ed25519 key pair and an IRK that the host generates itself.

What remains is protocol work. The open items are listed at the end; the
largest is getting the iPhone onto AWDL for the capture connection.

## The flow at a glance

| | Phase A: pairing | Phase B: capture |
|---|---|---|
| Advertises `_rdlink._tcp` on AWDL | **host**: TXT `rpBA`, `rpAD`, `rpPI`, `rpFl=0x80800`, `rpVr=715.2` | **iPhone**: TXT `rpBA`, `rpAD`, `rpFl=0x800`, `rpVr=715.2`, only after a "needs AWDL" request |
| Connects | iPhone → host | host → iPhone (*Inferred*) |
| Authentication | pair-setup: host is the SRP **server**, `passwordType` 10, PIN = `_pinC` | pair-verify with the Phase A keys; no pair-setup password |
| Rapport service type | `com.apple.ddui.guestpairing` | `com.apple.continuitycapture` |
| Result | each side stores the other as a **SessionPaired** identity | a RemoteDisplay session, with media over AVConference |

## The QR code

**Confirmed.** tvOS builds the QR in `-[_DDUIPeopleGuestPairViewController setPairingData:]`
(DeviceDiscoveryUI, `0x1b9d92c24`) from the pairing info rapportd returns. It
formats `"continuitycamera://connect?%@=%@&%@=%@&%@=%@&model=%@&%@=%@"`:

```
continuitycamera://connect?_pubID=<UUID>&_pinC=<UUID>&_pi=<UUID>&model=<ProductType>&_rdsr=<n>
```

- **Encoding:** UTF-8, nothing percent-encoded, error-correction level L.
- **UUIDs:** each is `-[NSUUID UUIDString]`, 36 characters, uppercase hex with
  hyphens.
- **`model`:** the TV's ProductType, for example `AppleTV14,1`.

| Key | tvOS makes it | The iPhone uses it |
|---|---|---|
| `_pi` | a fresh UUID string, rapportd `_startPairingServerWithPassword:…` (`0x1000e36e0`). It is also the TXT `rpPI`. | must equal the advertiser's `rpPI` exactly (case-sensitive), and `rpPI` must be longer than 15 characters |
| `_pinC` | a fresh UUID string, `remoteDisplayStartPairingServerWithReason:` (`0x1000ec200`); also the pairing server's password, `passwordType` 10 | the SRP password (`setFixedPIN:`); any string |
| `_pubID` | `RPIdentityDaemon.sessionPairingIdentifier`, a random UUID made at each rapportd launch | parsed into a throwaway object and **ignored** (iOS `remoteDisplayActivateSession:`, `0x1000f92cc`–`0x1000f9390`) |
| `model` | `GestaltCopyAnswer("ProductType")` | not read from the QR |
| `_rdsr` | the start reason, `"%hhu"`: 0 None, 1 OnCommunal, 2 OnPersonal, **3 MicOnly**. The picker sends 3 in mic-only mode, otherwise 1. | sets the discovery session's start reason; 3 makes the iPhone mic-only |

**On the iPhone (Confirmed):**

- `+[BCSURLDataParser parseURL:originalString:]` (`0x25a29528c`) accepts any URL
  whose lowercased scheme is `continuitycamera`, as data type 15, and checks
  nothing else.
- `-[BCSContinuityCameraAction performDefaultActionWithCompletionHandler:]`
  (`0x25a2978d8`) turns the query items into a dictionary (the last duplicate
  wins), serializes it as JSON, and opens an `RPRemoteDisplaySession` in the
  scanning process itself:
  - pairing info = that JSON;
  - `passwordType` 10;
  - service type `com.apple.ddui.guestpairing`;
  - a placeholder destination with a random identifier.

  No URL handler, App Clip or universal link is involved.
- rapportd (`remoteDisplayActivateSession:completion:`, `0x1000f8a8c`) refuses
  the session when **the iPhone user's Continuity Camera setting is off**
  ("Continuity Camera disabled."). It also requires the calling process to hold
  `com.apple.rapport.SessionPaired`, which Apple's scanner has.
- It then sets `needsAWDL` and waits for the host to appear on AWDL.

## Phase A: pairing over AWDL

### What the host advertises

**Confirmed** from both sides. tvOS's advertiser is
`_serverBonjourAWDLAdvertiserUpdateTXT`; the iPhone's checks are
`_clientBonjourAWDLBrowserFoundDevice:` (`0x1000ec518`) and
`_clientAWDLPairingSessionWithDevice:` (`0x1000ecadc`).

- **Service:** `_rdlink._tcp`, domain `local.`, on AWDL only. tvOS names the
  instance `RDLink-<rotating Bluetooth identifier>`; the iPhone doesn't use the
  name.
- **TXT:**

  | Key | tvOS sends | The iPhone checks |
  |---|---|---|
  | `rpFl` | `"0x80800"` (`"0x%llX"`) while pairing | bit 11 must be set ("Ignoring non-ConnectOnly AWDL device"). Bit 19 sends it down the pairing path, which skips identity resolution (`resolveIdentityForBonjourDevice:typeFlags:0xC02`) and the IDS-device-ID requirement |
  | `rpPI` | `_pi` | equals the QR's `_pi`, longer than 15 characters |
  | `rpVr` | `"715.2"` | not read on this path |
  | `rpBA` | 6 random bytes, `"%.6a"` (MAC-style) | not read on this path |
  | `rpAD` | `"%.3H"`, a 3-byte auth tag of `rpBA` under the TV's own IRK | not read on this path |
  | `rpRF` | absent | must be absent or 0 ("unsupported requirements") |

- **Listener:** tvOS's `CUTCPServer` "RDLink" (`_serverTCPEnsureStarted`) takes
  **one** connection while pairing. It binds IPv6 `::` on an ephemeral port,
  scoped to `awdl0`.
- **Discovery is AWDL-only.** The iPhone looks for the pairing host only
  through its AWDL browser.

So Koala registers `_rdlink._tcp` with that TXT through Windows' DNS-SD on the
Warlock adapter, asks Warlock to advertise the type, and listens on the
adapter's link-local address. This is exactly the shape Warlock's advertising
API was designed for.

### Pair-setup

The iPhone is the client. `netConnectionStartWithDevice:session:error:`
(`0x1000f69a4`) sets:

- control flags `0x240`, plus ForcePairing (bit 39), plus PreferSessionPaired
  (bit 46);
- `passwordType` 10;
- service type `com.apple.ddui.guestpairing`;
- destination: the advertised address.

The host is the server.

- **Confirmed**, tvOS Rapport.framework pair-setup server (`sub_19a03b610`):
  - It reads the client's PS_Start keys `_pwTy` and `_auTy`.
  - With `_pwTy` 10 and no `_auTy`, it treats the auth type as 8,
    **SessionPaired** ("Backwards compatibility authType -> sessionPaired").
  - The SRP secret is the server's password, the `_pinC` string itself
    (`CUPairingSession setFixedPIN:`, `setPinType:5`).
- **Confirmed**, iOS client (`RPConnectionCore` `clientPairSetupStart`,
  `0x1c057b248`): a `CUPairingSession` of type 1 (pair-setup client) with the
  fixed PIN and pair-setup flags 0. No flag or extra item asks the server for an
  MFi certificate or signature.
- *Inferred:* this is the standard HAP-style SRP-6a exchange (3072-bit,
  SHA-512) with Ed25519 identities in M5/M6, as pyatv implements for the
  companion link.

### The identities exchanged

**Confirmed.** Both sides install `_configureForSessionPairing:` handlers. Each
sends a `CUPairingIdentity` with:

- `identifier`: its session-pairing UUID (on the TV, the same UUID as `_pubID`);
- `publicKey`/`secretKey`: its Ed25519 key pair;
- `altIRK`: its IRK;
- `model` and `name`.

The iPhone's `savePeerHandler` (`0x1c05a6ce8`–`0x1c05a6dc4`) stores the host
with `-[RPIdentity initWithPairedPeer:type:13]` as a SessionPaired identity and
sets status bit 37.

- *Inferred:* the host's identifier must be a UUID string, because the iPhone
  takes `peer.identifier.UUIDString`.
- How `altIRK`, `model` and `name` travel inside M5/M6 (CoreUtils' extra-info
  item) isn't decoded. pyatv's companion pair-setup sends the same kind of
  OPACK info block, so it is the oracle.

### What follows on the iPhone

**Confirmed.** Once connected, the iPhone enters a discovery session with the
host's verified identity and the QR's start reason
(`_processDiscoverySessionStateChangeForDevice:startReason:`, `0x1000f5dc0`).

- CMContinuityCaptureRemote's discovery handler sees start reason 3 and logs
  "Entered mic-only session for device %@". It then launches the Continuity
  Camera screen with `micOnly:1` and `model:2` (TV).
- In this state the identity is kept: "Preserving session paired identity for
  guest mic-only teardown".
- The pairing connection itself is closed straight afterwards.

## Phase B: the capture connection

### On the host

**Confirmed.** tvOS host side, `CMContinuityCaptureHost`.

- The TV first asks the iPhone for its camera capabilities. It sends a
  companion-link request `_camCap`, with control flags 6 (BLE + infrastructure
  Wi-Fi), and moves to "in session" only once it has them.
- Capture then runs over Rapport RemoteDisplay. The Network.framework QUIC path
  exists in the binaries, but is *inferred* unused for this. The setup is in
  `-[CMContinuityCaptureRapportClient setupRPDisplaySessionForConfiguration:completion:]`:
  - `RPRemoteDisplaySession` to the iPhone, service type
    `com.apple.continuitycapture`, control flags ForceAWDL + NoiWiFi
    (`0x220000`);
  - an `RPStreamServer` on that session, stream flags 2, traffic flags
    `0x10000` when mic-only;
  - session-start info: an NSKeyedArchiver-encoded configuration
    (`ContinuityCaptureRapportClientPreStartConfigurationKey`) and stream
    descriptors.
  - Media is AVConference, as Warlock's `docs/macos-continuity-camera.md`
    describes for the Mac.

### On the iPhone

- **Server.** It is the RemoteDisplay server. It starts its RDLink server when
  it holds a SessionPaired identity, even with no IDS peers (`_update`,
  `0x1000246ac`). **Confirmed.**
- **Accepting the host.** Its listener takes pair-verify only; there is no
  password. For a peer whose model is an Apple TV and service
  `com.apple.continuitycapture`, it checks that the verified identity is the
  device from Phase A ("Device in discovery session did not match";
  `_serverHandleSessionStartWithID:netCnx:completion:`, `0x1000eec7c`). Koala's
  key is that device, so the check passes. **Confirmed.**
- **When it advertises.** It advertises `_rdlink._tcp` on AWDL only while
  something asks for it (`_serverBonjourAWDLNeeded`, `0x1000291e4`). Apple's
  host asks both ways at once (`_clientBLENeedsAWDLAdvertiserEnsureStarted`,
  `0x1000e9f6c`):
  - **BLE:** a Nearby-Action advert of action type `0x32`, from a device the
    iPhone identifies (through the IRK it was given).
  - **Wi-Fi:** a companion-link connection with NoAWDL, sending the event
    `_rdNeedsAWDL` `{_i: <host identifier>, _ena: true}`. The iPhone's
    registration for that event explicitly admits SessionPaired peers (status
    flags `0x3000080000`), and arms a 15-second timer per request
    (`_serverReceivedNeedsAWDLEvent:`, `0x1000efbec`).

## Tried with an iPhone (2026-11-18): Phase A works

`koala guest` paired with an iPhone on iOS 27.2.1 (24B156, iPhone18,1)
over Warlock, and the iPhone entered a mic-only discovery session with it.
This is K2. Koala traced every frame. The iPhone's own log, streamed over
USB with pymobiledevice3 (`syslog live`), shows its side.

- **Discovery.** rapportd browsed `_rdlink._tcp` on `awdl0` and found
  Koala's instance from Warlock's frames. It logged `AWDL pairing device:
  awdlPairingID … sessionPairingID …` (the same UUID, from `rpPI` and the
  QR). It resolved the SRV to the computer name and the AAAA to the adapter's
  link-local address, then connected over AWDL. Random `rpBA` and `rpAD`
  were accepted.
- **Pair-verify first.** The iPhone opened with `PV_Start` and `_auTy` 8. It
  failed, as it should with a stranger: `Resolve identity for signature
  failed: 0x4C02 < SameAccountDevices SharedTVUserDevices
  SessionPairedDevices >`. Then `Client PairSetup retry`.
- **Pair-setup.** `PS_Start` carried `_pd` (method 0, state 1, nothing else),
  `_auTy` 8 and `_pwTy` 10. Each `PS_Next` repeated `_auTy` and `_pwTy`.
  - Koala's SRP (`koala-pairing`, pyatv's encodings) was accepted, and so
    was Koala's M6 (`PairSetup client done -- server authenticated`).
  - The iPhone's M5 info block is OPACK `{altIRK, accountID, model, wifiMAC,
    name, mac}`. Its `accountID` equals its pair-setup identifier.
  - Koala's M2 carried item 27 = 1. Whether it's needed wasn't tested.
- **Encrypted from M6.** With no pair-verify, the iPhone sent its next frame
  encrypted (`Open stream 'main'`). The keys come from the SRP session key
  `K`, exactly as pair-verify's come from its shared secret: HKDF-SHA512
  with an empty salt and `ClientEncrypt-main`/`ServerEncrypt-main`.
- **`_systemInfo`.** Its first request (`Client state: CSessionStart ->
  CSessionWait`).
  - The content is `_clSrv` = `com.apple.ddui.guestpairing`, `_i` and
    `_idsID` (its Rapport identifier), `_spID` (its pair-setup identifier),
    `_pubID`, `model`, `name`, `_sv` = `715.2`, `_bf`, `_cf`; the frame also
    has `_btHP` = false.
  - **Koala's answer mirrors it with Koala's values**: model `AppleTV14,1`,
    and `_spID` Koala's pair-setup identifier.
  - **The answer's `_x` must repeat the request's exactly, width included.**
    The iPhone sends an 8-byte integer. Echoed in the 4 bytes that fit a
    value above 2^31, it read back negative: `Received response XID … with
    no request`. Then the connection idled until the scanner gave up after
    about 10 s, and the iPhone's "Connecting" banner just faded.
- **In session.** With the answer matched:
  - `Client state: CSessionWait -> CMsg`, `External state changed: Unknown
    -> Ready`;
  - `Discovery session state OutOfSession -> InSession, peer <Koala's
    pair-setup identifier>, startReason MicOnly`;
  - `Device confirmed changed NULL -> <Koala's identifier>`;
  - `Posting notification for wombat activity change: InDiscoverySession`.

  The iPhone then stopped its pairing browser, and sharingd started BLE
  Nearby Action adverts.
- **Open:** CMContinuityCaptureRemote logged `Could not resolved stream
  format` as it entered the session.

## Phase B: what starts the capture side (static analysis, 2026-11-19)

The trial above left the iPhone InSession with nothing on screen. Two more
studies of the same builds explain why, and what a host does next. Each is
from code; nothing here has been tried yet.

- **Nothing launched the capture daemon.** The iPhone's side is
  `/usr/libexec/continuitycaptured`, which creates `CMContinuityCaptureDServer`.
  launchd starts it only for a rapport launch event, the cheapest being a
  RemoteDisplay session for `com.apple.continuitycapture`. The InSession
  callback alone launches nothing, which is why its first log line, `Session
  state changed to %d`, never appeared. `Could not resolved stream format`
  only drops a video format from the capabilities, and is harmless.
- **The session start.** The host connects to the iPhone's RDLink listener and
  pair-verifies with the Phase A keys (`_auTy` 8). It then sends `_systemInfo`
  with `_clSrv` = `com.apple.continuitycapture`; there is no other session
  wrapper. rapportd checks that the peer is an Apple TV model and the
  confirmed device, then hands the connection to `continuitycaptured`, which
  launches the mic-only screen before any media negotiation.
- **After that, as a tvOS host does it.**
  1. The event `ContinuityCaptureSessionEventID`, with `MessageType` 0,
     `SessionID`, `PreStartConfiguration` and `StreamsSetup`.
     - `PreStartConfiguration` is an NSKeyedArchiver
       `CMContinuityCaptureConfiguration`: `entity` 3, a `sessionID` UUID,
       `audioDeviceMode`.
     - `StreamsSetup` lists Control, Command and Data as TCP (type 2), and
       `ContinuityCaptureMediaMicrophone` as UDP (type 3).
  2. The iPhone opens each stream with `_streamStart`, and the host answers:
     - TCP streams: `{_streamKey: X25519 public key, _streamPort}`. Each is a
       new Rapport connection, encrypted with keys from that X25519 secret.
     - The microphone: `{_streamAddr, _streamMACAddr, _streamKey: 32 random
       bytes, _streamPort}`.
  3. Per stream, the host sends `{MessageType 3}` once it is up, and
     `{MessageType 2}` again when the iPhone sends `{MessageType 1}`.
  4. On Command: `ContinuityCaptureStreamEventID {ContinuityCaptureSelector
     1 (StartStream), ContinuityCaptureArgs [config, option, 0]}`.
  5. On Control, AVConference negotiation: the iPhone offers (`Selector` 2),
     the host answers.
  6. SRTP key: the host's 32-byte `_streamKey` plus the first 14 bytes of
     the session ID, both ways, cipher suite 5. A tvOS host also runs gPTP
     over UDPv6; whether that's required is open.
- **The obstacle: the listener's port.** The iPhone advertises `_rdlink._tcp`
  on AWDL only when asked. It takes either a BLE Nearby Action advert (action
  type `0x32`, from a device its IRK identifies) or the event `_rdNeedsAWDL
  {_i, _ena: true}` over a companion link, which a tvOS host sends with AWDL
  excluded, so over shared Wi-Fi. A host with AWDL alone can do neither.
- **A port scan didn't find it (2026-11-19, two tries).** During Phase A,
  with the iPhone's AWDL up, Koala tried every TCP port from 49152 to 65535 on
  its link-local address over Warlock. It used 1024 probes at once × 1 s and
  2048 × 0.6 s. **Nothing answered, not even with a refusal**: every probe
  timed out.
  - Either iOS drops connections on its restricted AWDL interface unless the
    socket opted in, or the burst overwhelmed Warlock's AWDL transmit path.
  - TCP from Windows to an Apple listener over Warlock hasn't been checked on
    its own yet.

## Phase B over BLE: the advert, and Windows' address (2026-11-19)

- **The advert, from code.** After Apple's company ID, `0F 05 A0 32 T0 T1
  T2`: one Nearby Action with flags `0xA0` (`0x80` an auth tag follows,
  `0x20` needs keyboard), action type `0x32`, and a 3-byte auth tag.
  - The tag is SipHash-2-4 keyed with the host's IRK (Koala's `altIRK` from
    M6), over the 6-byte address the advert is sent from, taking bytes 2-4
    of the big-endian result.
  - The iPhone accepts it only at −60 dBm or stronger.
- **Tried (run 7).** The iPhone heard Koala's advert, at −52 dBm, and parsed
  it as `NeedsRDLink, NeedsAWDL, NeedsKeyboard`. But it couldn't identify
  the sender: `### Unable to decrypt activity level with authTag yes
  identity no error -6727`. Koala had tagged it for the adapter's public
  address. bluetoothd logged the address it actually came from, a random
  one.
- **Windows' advertising address, from an HCI trace.**
  - The trace: ETW provider `Microsoft-Windows-BTH-BTHPORT`
    `{8A1F9517-3A8C-4A9E-A018-4F17A200F277}`, all keywords. Event 402 holds
    the HCI packets as direction, length and bytes. Enabling only the HCI
    channel's keyword gives nothing.
  - For each WinRT advert, Windows creates an extended advertising set. It
    sends LE Set Extended Advertising Data (`0x2037`) first, then LE Set
    Advertising Set Random Address (`0x2035`) with **a new non-resolvable
    random address each time**, then enables the set.
  - Changing the publisher's data while it runs is accepted by WinRT but
    never reaches the controller: no new `0x2037` is sent.
  - So through WinRT a host can't know the address before the data is
    fixed, and the tag can't be right. The way round needs one of: an
    identification that doesn't depend on the address (the NearbyActionV2
    path, not yet decoded), a Bluetooth controller Koala drives itself, or
    another trigger.

## Phase B over BLE: choosing the IRK to fit the advert (2026-11-19, run 8)

The IRK is Koala's to choose, and the iPhone learns it only in pair-setup.
So Koala reverses the order:
1. Start the advert first, with a random tag.
2. Read its random address from the HCI trace.
3. Search for an IRK whose tag for that address is the advert's tag. That's
   about 2^24 SipHash computations, well under a second.
4. Pair with that IRK as `altIRK`, leaving the advert running.

**It works.**
- One second after the iPhone entered its mic-only session, its AWDL frames
  carried its own `_rdlink._tcp` instance: PTR, an SRV with its port and a
  random UUID host, and TXT.
- Koala connected to that port over Warlock, and the iPhone answered
  pair-verify (`_auTy` 8) with M2.
- That M2's sealed part names no one: it has a signature and no identifier.
  CoreUtils resolves a SessionPaired peer by trying its keys on the
  signature. `koala-pairing`'s client now checks such an M2 against the
  peer it expects.

## Phase B: the capture session opens, and wants a sideband (2026-11-19, run 10)

- **Pair-verify on the iPhone's listener.** In both directions, the
  signature covers the two ephemeral keys only, with no identifier sent.
  - The iPhone's M2 is signed that way.
  - Koala's M3, signed the same way, was accepted: `Resolved identity for
    signature: SessionPaired`, then `PairVerify completed server:
    SessionPaired`.
  - Signing over the identifier, as HAP does, fails: `Resolve identity for
    signature failed`.
- **The session starts.** Koala sent `_systemInfo` with `_clSrv` =
  `com.apple.continuitycapture`, and the iPhone answered with its own.
  rapportd buffered the connection for the proxied server, and launchd
  started `continuitycaptured` (`ccd start`). It logged `Session state
  changed to 1`, then `New Connection RPRemoteDisplaySession` and `create
  session`.
  - It created audio and video capture sessions; the audio device is
    entity 3.
  - It launched the mic-only screen (`_launchShieldUIForDeviceIdentifier…
    micOnly`) and played its "pong" sound.
- **Then it wanted a sideband.**
  - The log shows `Wait for sideband identifiers …, cache event`.
  - The daemon tried to send `ContinuityCaptureCompanionSessionEventID` to
    the host over a companion link (CLinkClient, flags `BLE iWiFi BTPipe
    SessionPaired`, never AWDL). rapportd chose an on-demand **BLE
    connection** to the device behind the needs-AWDL advert: `Start
    on-demand connection (NoWiFiP2P)… LT BLE`.
  - Windows' WinRT adverts aren't connectable, so the connection never
    came up. The daemon retried every 5 s, and the screen ended at "Unable
    to connect: make sure the iPhone and Apple TV are on the same Wi-Fi
    network".
  - Koala, for its part, had sent nothing after `_systemInfo`. A tvOS host
    sends its session-start event (`ContinuityCaptureSessionEventID`,
    `MessageType` 0) next.
- **Open:**
  - Does the host's session-start event satisfy the wait for "sideband
    identifiers"?
  - If not, the sideband needs Rapport over BLE (a connectable advert and a
    BLE transport Windows can serve) or a companion link over shared Wi-Fi.

## Phase B: the session start, and the iPhone asks for its streams (2026-11-19, run 11)

Koala sent `ContinuityCaptureSessionEventID` with `MessageType` 0 (from
`koala-capture`) once the iPhone answered `_systemInfo`. The event carried
session number 1, the archived mic-only configuration (entity 3, a new
sessionID, `audioDeviceMode` 'call') and the four streams.

- **The iPhone accepted it.** It sent `MessageType` 1 (session started),
  then a `_streamStart` request for each stream:
  - Control, Command and Data: `{_streamID, _streamType: 2, _streamKey: its
    32-byte X25519 public key, _tf: 0, _streamSrv:
    "com.apple.continuitycapture"}`.
  - The microphone: `{_streamID: "ContinuityCaptureMediaMicrophone",
    _streamType: 3, _streamFlags: 2, _streamAddr: a ULA (fd12:…::1),
    _streamPort, _streamMACAddr: its AWDL MAC, _tf: 0}`.
- **Koala answered each with an empty dictionary.**
  - The iPhone logged `[sessionID:1] create stream (…)` errors.
  - It posted `kCMContinuityCaptureEventStartStream` for the microphone,
    then cached it: `Wait for sideband identifiers`.
  - The screen ended at "Unable to connect" again.
- **"Sideband identifiers" are these streams.** tvOS's
  `_CMContinuityCaptureGetListOfSupportedSidebandIdentifiers` returns
  Control, Command and Data. The iPhone waits for those three Rapport
  streams to come up, not for a companion-link sideband: the BLE attempts
  of run 10 came before the session had started. (Trials 12–13 showed that
  companion link is wanted after all, for something else: the Sing path,
  below.)

## Phase B: the streams come up, and the Sing path ends it (2026-11-19, trials 12–13)

Koala served the streams (`capture.rs`):
- Control, Command and Data: an X25519 answer, a TCP listener, and the
  pre-shared key's cipher.
- The microphone: a UDP socket and `{_streamAddr, _streamMACAddr,
  _streamKey, _streamPort}`.
- Then `MessageType` 3 for each stream, and StartStream on Command once all
  four were up.

Trial 12 collided with a trust prompt. Trial 13 is clean, with the iPhone's
syslog. Its timeline, from the start of the capture session (07:50:35Z):

- **+0.1–0.9 s: all three Rapport streams connect and decrypt.** The iPhone
  connects to each listener within about 250 ms of the answer. Its first
  frames open with the pre-shared key's `ClientEncrypt-main`/
  `ServerEncrypt-main` keys, which confirms them.
- **+0.9 s: the microphone path is refused.** The log reads `### Client start
  response failed: UDP NWPath, kMalformedErr (Bad peer address string
  'fe80::…%awdl0%llw0')`.
  - The iPhone appends its own scope, and that scope is `llw0`, its
    low-latency WLAN interface over AWDL, where it opens the UDP listener.
  - `_streamAddr` must be bare. Its own request's is: a ULA such as
    `fdab:fc94:b7cc::3`, which changes every session.
  - Fixed after trial 13. Untried.
- **+2.0 s: the Control and Command streams carry traffic.**
  - StartStream on Command is answered with `kCMContinuityCaptureEventUserResume`.
  - Control carries SetValue (selector 1) events: keyed archives of
    `CMContinuityCaptureControl` for entity 4, namely SystemPressure,
    BatteryLevel and BatteryState, repeated.
  - The microphone device moves Disabled → (RemoteClientReconnect) Init →
    (StartStream) **Streaming**.
- **+2.0 s: the iPhone offers AVConference.**
  - `CMContinuityCaptureAVCStreamCoordinator … creating stream (entity:3
    isHost:0 … networkClockID:…)`, then `AVCMediaStreamNegotiator
    initWithMode: 4`, then `sending negotiation offer (mode:4 size:470)`.
  - The offer is Control selector 2, `ContinuityCaptureArgs` = `[3,
    bplist]`. The bplist is a plain dictionary:
    - `avcMediaStreamNegotiatorMode` 4;
    - `avcMediaStreamOptionCallID` (a UUID string);
    - `avcMediaStreamOptionRemoteEndpointInfo`: protobuf `{1: 0, 2: 1, 3:
      "iPhone18,1", 4: "2260.15.1", 5: "24B156"}`;
    - `avcMediaStreamNegotiatorMediaBlob`: zlib over a 204-byte protobuf.
      Its fields are 1: 1; 2: 1; 3: {six varints}; 6: "Viceroy 1.7.0/GK";
      8: 0; fifteen repeated field-9 entries of two or three varints
      (bitrate-like values); 13, 14 and 16. There are no audio codec
      fields, so the codecs must come in the host's answer.
  - Then `[sessionID:1] not ready (negotiationDataReceived:0 …)`: it waits
    for the host's answer. Koala has none yet.
- **The Sing path, from +0.1 s, is what fails.**
  - DServer logs `session started … deviceModel 2`, UI configuration
    `clientDeviceModel=2 … micOnly=1`.
  - On that it launches `com.apple.ContinuitySingShieldUI`, not the
    Continuity Camera shield.
  - It also activates a CompanionLink client to the host (`CF < BLE iWiFi
    BTPipe BLEScreenOff SharedTVUser SessionPaired >`) and sends
    `ContinuityCaptureCompanionSessionEventID` (4 keys) to
    `rapport:rdid:DirectPeer`.
  - rapportd's only route to Koala is BLE L2CAP, PSM 0x81, to Koala's
    advert. Windows doesn't serve it, and the client is invalidated at
    +5 s.
  - At +7.8 s: `failed to find rapport device for sing session endpoint` →
    `presentShieldError: 1`. This is the "Unable to connect: make sure the
    iPhone and Apple TV are on the same Wi-Fi network" screen.
  - Dismissing it gives `disconnectSession: reason User disconnect`, and
    with it `kCMContinuityCaptureEventUserDisconnect` on Command and the
    streams' close.

So the iPhone treats a mic-only session with an Apple TV as Apple Music
**Sing**, which also wants a CompanionLink connection to the TV. Whether a
session can avoid that path is the next question: a host that presents as
another model, or a flag in the configuration. If not, the iPhone has to
reach Koala over CompanionLink. The AVC answer is needed either way.

## Phase B: off the Sing path, the iPhone sends audio (2026-11-19, trials 16–17)

**Static analysis of iOS 27.2.1 (`continuitycaptured`):**
- The Sing path is micOnly alone. micOnly is rapportd's discovery start
  reason 3, which comes from the QR's `_rdsr=3`; the model plays no part.
  `-[CCDShieldUISession bundleIdentifier]` picks ContinuitySingShieldUI on
  it.
- The Sing app's `setupSingSessionFromURL:remoteDisplayIdentifier:` needs a
  sideband CompanionLink device with a media route, which only a real
  AirPlay output has.
- The device model is the `model` in the capture connection's
  `_systemInfo` (`_CMContinuityCaptureDeviceModelFromModelString`): any
  string containing "Mac" gives 1, "AppleTV" 2, "iPad" 3, "iPhone" 4.

**What changed in Koala:**
- The QR says `_rdsr=1` (OnCommunal).
- The capture `_systemInfo` presents `MacBookAir10,1`. Pairing still presents
  the Apple TV.
- Koala answers the AVC offer as tvOS's `initWithOffer:` does
  (`koala-capture`'s `avc`): payload 101, its own SSRC, the offer's
  bandwidth settings mirrored.
- The microphone's `_streamAddr` is a bare link-local address.

**What the iPhone did (trial 17, with its syslog and a capture on the Warlock
adapter):**
- **No Sing.** It showed the Continuity Camera shield, reported connected,
  and brought up all four streams. The `llw0` path was accepted.
- **It took the answer.** Its stream configuration reads
  `TxPayloadType = 101`, `CodecType = 19`, 4 channels, `ptime` 10 ms,
  900 kbps, SRTP and SRTCP cipher suite 7. The send key equals the receive
  key and is 46 bytes: 32 + 14, as from `_streamKey` ‖ sessionID.
- **It sent audio:** `VCAudioTransmitter … audioTxBitrate=919kbps`, over a UDP
  connection to Koala's microphone port on `llw0`. **None of it reached
  Windows.** Warlock carries only AWDL-encapsulated data (Apple's LLC/SNAP
  under the AWDL BSSID), and `llw0` is a separate `802.11` interface on the
  iPhone. That its frames are encapsulated otherwise is an inference until a
  raw capture shows them.
- **It sent a clock.** On Command, selector 4 every 60 ms: `[sample time,
  +2880 a message (48 kHz); host time in ns; clock ID]`.
- **It sent PTP.** As gPTP grandmaster (domain 0, priority1 250), over
  unicast UDP 319/320 to Koala's link-local address: Announce, Sync,
  Follow_Up, and Signaling with Apple org-extension TLVs (`00:0d:93`). That
  is about 15 packets a second, and Koala answered none.
- **It gave up at +12 s:** `VCMediaStream checkRTCPPacketTimeout… Last RTCP
  packet receive time: nan … rtcpTimeoutInterval=12.0`, then `stream RTCP
  timed out` (`-1007`) and `kCMContinuityCaptureEventConnectionDisconnect`.
  The host must send RTCP back over that path.

## Phase B: the audio never reaches the air (2026-11-19, trial 19)

Trial 17 again, with Warlock 0.2.0's capture of the radio running throughout
(`warlockctl capture --tx`, all frames) and the iPhone's syslog. (Trial 18
was voided: scanned before the syslog was attached.)

**The same session:** pair-verify, four streams, the AVC answer accepted,
`VCAudioTransmitter … audioTxBitrate=907kbps` from 19:51:09, and `stream
RTCP timed out` at 19:51:21. The microphone stream's iPhone end was its
`llw0` ULA, `fde7:e5a2:37cd::7`.

**What the radio heard from the iPhone (`76:56:82:58:57:70`):**
- QoS Data frames to Warlock's MAC under the AWDL BSSID, unprotected,
  AWDL-encapsulated (Apple's LLC/SNAP), all from its **`awdl0`** address
  `fe80::7456:82ff:fe58:5770`:
  - gPTP, UDP 319/320, at TID 6 (voice);
  - the Rapport TCP streams at TID 0, 41 of those frames as A-MSDUs, which
    Warlock drops (it takes no A-MSDU);
  - mDNS and ICMPv6.
- Not one packet from its `llw0` address, and no UDP to Koala's
  microphone port, on the channels the radio followed (149, 161, 6). The
  iPhone's own frames came at 10–80 a second then; 10 ms audio would be 100.
- 610 RTS frames to Warlock's MAC. No CTS or ACK appears in the capture,
  from anyone: the chip may not pass them up.
- Its AWDL frames carry two TLVs Warlock's don't: **17**, the 802.11
  container (HT/VHT capabilities), and **33**, which Wireshark doesn't know,
  14 bytes `01 00 00 00 78 79 01 21 86 00 00 00 00 00` in every frame.

**What the iPhone said:**
- avconferenced's microphone connection to `fe80::605a:37ff:fefb:de77%llw0`
  port 59866 was `ready channel-flow` on `llw0[802.11]` from 19:51:09; the
  kernel's `netif flowstats (remove): if llw0 … src fde7:e5a2:37cd::7, dst
  fe80::605a:37ff:fefb:de77` at the end shows the flow existed.
- **No LLW peer was attached:** the only `AppleWLANDataPeerManager` LLW line
  in the trial is `detachAllLLWPeer` (19:50:05, the voided trial's
  teardown). At 19:51:21: `setInterfaceEnable … llw0 disable`, `Link Down on
  llw0`.
- `wifip2pd`: `WiFiInterface<P2PController<AppleIO80211Driver>>[llw0]<76:56:82:58:57:70>`:
  `llw0` has the AWDL MAC.

**So:** the audio is held on the iPhone, most likely for want of an LLW peer.
LLW is not a new encapsulation of the AWDL data we already carry; it is a
separate interface whose peer the iPhone's Wi-Fi stack sets up, and nothing
Warlock does sets it up.

**Prior art, the 39C3 talk** (Henri Jäger, "Cracking open what makes Apple's
Low-Latency WiFi so fast", subtitles read 2026-11-19): LLW is standard
802.11 configured for latency: EDCA access category voice, A-MSDU and
A-MPDU, an 80 MHz channel (channel 52 in its example), and RSDB, a second
simultaneous link. avconferenced opens `llw0` (`channel_open`,
IOSkywalkFamily `setInterfaceEnable`); `wifip2pd` configures it through the
IO80211 DriverKit's `configureHPPLowLatencyWiFiParameters`. Asked about a
schedule: "there's a channel sequence … I guess it's the same as AWDL". The
talk doesn't say how a peer is set up.

## LLW: what attaches a peer (static analysis, 2026-11-19)

From iOS 27.2.1 (24B156): the IPSW's DriverKit shared cache (`ipsw extract
--dyld --driverkit`; the Wi-Fi drivers moved out of the kernelcache, which
has only `IOSkywalkFamily`), `wifip2pd` and the
`com.apple.DriverKit-AppleBCMWLAN` dext, all unpacked into overture's
`~/koala-re/re/tmp/llw`. Also minintosh's Big Sur `IO80211Family`, which
keeps its symbols.

- **LLW peers exist over AWDL as well as NAN:**
  `ACIAwdlAdapter::add_llw_peer(apple80211_llw_params*, …)` beside
  `ACINanAdapter::add_llw_peer`, and `start_llw_session`,
  `AppleWLANInterfaceAWDL::setupLLWPeer(apple80211_llw_params*)`,
  `AppleWLANLowLatencyInterface::setLLWPeerPktFilter(ether_addr*)`. The driver
  logs `llwAddPeer success, peerId:%d …` and `Peer … not found in
  fLocalPeerMacs, not an LLW peer`.
- **The driver adds one only when told:** `AppleWLAN80211AWDL::setLLW_PARAMS`
  (`APPLE80211_IOC_LLW_PARAMS`, from user space) leads to `setupLLWPeer`.
- **`wifip2pd` tells it for a "Continuity Real Time Group" (RTG):**
  `continuityRealTimeGroupStart`/`Stop`, `rtgID`, `rtgPeersCount`,
  `realtimeStartRequest`, `AWDL_REALTIME_TRAFFIC_START`, "Continuity Real
  Time Group RTG Sync Election", "Creating companion low latency interface",
  and RTG statistics for Sidecar, Universal Control, remote camera and TV
  remote camera.
- **In trial 19 none of it started.** `wifip2pd` logged only `Realtime mode
  updated false` and `AWDL schedule changed in non-RTG mode`; the driver
  logged no `llwAddPeer`. And `continuitycaptured` (CoreUtils) logged
  `Register traffic: [fe80::605a:37ff:fefb:de77%llw0]:59866, 0x0 < >`, a
  traffic registration for Warlock with no flags, beside its session's
  `Flags 0x2 < LowLatency >`.

So the gate is upstream of the driver: what makes that registration carry
realtime flags, or `wifip2pd` start a realtime group, for a peer.

## What Koala needs, from this

- **QR:** `continuitycamera://connect?_pubID=<UUID>&_pinC=<UUID>&_pi=<UUID>&model=AppleTV14,1&_rdsr=1`.
  Not `_rdsr=3`: that is the Sing path (trials 12–13, 16–17).
- **Advert:** through Warlock, `_rdlink._tcp` with TXT `rpFl=0x80800`,
  `rpPI=<_pi>` and `rpVr=715.2`, plus plausible `rpBA`/`rpAD`. One TCP listener
  on the Warlock adapter.
- **Rapport server:** pair-setup with password type 10, PIN `_pinC`, and auth
  type SessionPaired. Koala's identity is a UUID, an Ed25519 key pair, an IRK,
  model `AppleTV14,1` and a name. Keep the iPhone's identity (its Ed25519 key
  and IRK) for Phase B.
- **Then:**
  1. get the iPhone onto AWDL;
  2. connect to its `_rdlink._tcp`;
  3. pair-verify;
  4. open a `com.apple.continuitycapture` RemoteDisplay session in mic-only
     mode;
  5. negotiate AVConference.

## Open

1. **`llw0` through Warlock** (trials 17, 19): the iPhone never attaches
   an LLW peer for Warlock, so no `llw0` frame reaches the air. What makes
   it attach one: TLV 33, TLV 17 (HT/VHT), something in the AVC or
   `wifip2pd` negotiation, or a link on another channel (RSDB)? Then
   carrying its frames both ways, including to its `llw0` ULA
   (`fde7:e5a2:37cd::7` in trial 19), which isn't derived from its MAC.
2. **RTCP from the host** within 12 s (SRTCP, cipher suite 7), and how the
   46-byte key splits into master key and salt.
3. **PTP:** whether the host must answer the iPhone's gPTP (its Apple
   Signaling TLVs), or only follow it. Prior art: shairport-sync's NQPTP.
4. **The audio itself:** SRTP decryption, codec type 19 (4 channels,
   48 kHz, 10 ms), and the clock messages on Command.
5. **CoreUtils' extra-info layout** in M5/M6 (`altIRK`, `model`, `name`).
6. **Which iOS process runs the scanner action.** The iOS file system wasn't
   extracted, so entitlements weren't read. This doesn't affect Koala.
7. **A possible oracle:** rapportd's `rdPairingServer` preference lets the
   pairing server run on a device that isn't a TV, a Mac included. Untried; the
   lab Mac is kept read-only.

Runs 8–17 settled what was open here before:
- getting the iPhone onto AWDL for Phase B (BLE, runs 8–10);
- `_camCap`: the session gets as far as streaming without it;
- the capture protocol up to the streams;
- the Sing path (avoided with `_rdsr=1`, trials 16–17);
- the AVC answer (accepted, trial 17).

## Reproducing

All on the lab Mac's RE workspace (`~/koala-re`). Tools:

- `ipsw` 3.1.739: `dyld str`, `dyld disass --symbol … --cache <a2s>`,
  `macho disass`, `class-dump --re`;
- the helper scripts in `re/tmp/host/` and `re/tmp/guest/`.

```sh
TV=~/koala-re/out/tvos/24K122__AppleTV14,1/System/Library/Caches/com.apple.dyld/dyld_shared_cache_arm64e
IOS=~/koala-re/out/ios/24B156__iPhone17,1/dyld_shared_cache_arm64e
ipsw dyld str $TV "continuitycamera://"               # the format string, in DeviceDiscoveryUI
ipsw dyld symaddr $TV _GuestPairInfoKeyPin            # the key constants: _pubID _pi _pinC _rdsr
ipsw dyld disass $IOS --symbol '+[BCSURLDataParser parseURL:originalString:]' --cache ~/koala-re/re/ios.a2s
ipsw dyld disass $IOS --symbol '-[BCSContinuityCameraAction performDefaultActionWithCompletionHandler:]' --cache ~/koala-re/re/ios.a2s
ipsw macho disass ~/koala-re/re/bin/rapportd.ios -x __TEXT.__text --no-color > rapportd.ios.dis   # then the rapportd addresses above
```
