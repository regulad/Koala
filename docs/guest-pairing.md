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

## What Koala needs, from this

- **QR:** `continuitycamera://connect?_pubID=<UUID>&_pinC=<UUID>&_pi=<UUID>&model=AppleTV14,1&_rdsr=3`.
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

1. **Getting the iPhone onto AWDL for Phase B.** Neither way has been tried.
   - Over Wi-Fi, the PC connects to the iPhone's companion link as its
     SessionPaired peer and sends `_rdNeedsAWDL`. Unverified: that the iPhone's
     companion-link server accepts a SessionPaired peer, and over which
     network. The iPhone also advertises `_companion-link._tcp` over AWDL, so
     the request may not need the LAN at all.
   - The BLE alternative needs Windows to advertise Apple manufacturer data,
     which may be restricted.
2. **`_camCap`.** Whether the iPhone needs it before accepting the capture
   session, or only the TV does.
3. **The capture protocol:** the control messages after connecting, the
   NSKeyedArchiver configuration's classes, the stream descriptors, and how the
   AVConference/SRTP keys are set up. This is K3.
4. **CoreUtils' extra-info layout** in M5/M6 (`altIRK`, `model`, `name`).
5. **Which iOS process runs the scanner action.** The iOS file system wasn't
   extracted, so entitlements weren't read. This doesn't affect Koala.
6. **A possible oracle:** rapportd's `rdPairingServer` preference lets the
   pairing server run on a device that isn't a TV, a Mac included. Untried; the
   lab Mac is kept read-only.

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
