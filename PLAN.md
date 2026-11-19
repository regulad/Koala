# Plan: Koala (an iPhone as a Windows microphone), in Rust

## Context

Koala makes an unmodified iPhone work as a microphone on Windows. It impersonates an Apple TV hosting a guest, mic-only Continuity Camera session: tvOS's QR/PIN guest pairing, which gives the iPhone a Rapport `SessionPaired` identity for the host. Reverse engineering showed that guest pairing runs **only over AWDL**, which Windows lacks. [Warlock](https://github.com/regulad/Warlock) (0.1.0, released 2026-11-18) now gives Windows AWDL, so Koala can start.

The research so far is in Warlock's `docs/`, with Koala's parts in `macos-continuity-camera.md` (macOS 27 hosts) and `tvos-ios-continuity-sing.md` (iOS/tvOS 27, the Apple Music Sing path). Those docs move here when Koala is published; until then, this plan points at them.

## Pinned decisions

- **All Rust.** Licence AGPL-3.0-or-later (`LICENSE.md`, the FSF text).
- **AWDL comes from Warlock**, through its control API and `warlock-client` (GPL-2.0-only OR AGPL-3.0-or-later; Koala takes it under the AGPL). Koala links no Warlock internals. Koala's own services are registered with Windows' DNS-SD on the Warlock adapter, and Warlock is asked to advertise their types (Warlock's `docs/service-discovery.md`).
- **Audio goes out through the VB-Audio virtual cable** ("CABLE Input"), fed from user space. No audio driver of our own.
- **Trust path:** impersonate an Apple TV and use its guest QR/PIN pairing. Cloning a same-Apple-Account identity is out.
- **No new hardware (Parker, 2026-11-18).** There is no Apple TV that can host a guest session here (Continuity Camera needs an Apple TV 4K of the 2nd generation or later, the Sing guest mic the 3rd), so the ground truth is static reverse engineering of the iOS and tvOS 27 images, then trials against the iPhone itself. Available: an iPhone on iOS 27; an Apple TV 4K (1st generation, which can't run tvOS 27) on the LAN, which speaks Rapport (`_companion-link._tcp`) but can't host; a jailbroken 3rd-generation Apple TV (iOS 8); a 2015 MacBook on macOS 11 as an AWDL witness; the RE workspace on the lab Mac `overture` (`~/koala-re`, with `ipsw` and Ghidra) and Ghidra on this PC.
- **Local until a working prototype (Parker, 2026-11-18).** The repository goes to GitHub (`regulad/Koala`) only once something works; `.github/` (settings, CI, Dependabot, as in Warlock) comes with that push.
- **Dependencies** must be at least two weeks old, AGPL-compatible (Apache-2.0 is fine here, unlike in Warlock; GPL-2.0-only isn't), and vetted for provenance. `deny.toml` enforces the licences and sources. Age covers transitive crates too: `Cargo.lock` pins older versions where the newest is too young (2026-11-18: bytes 1.12.1, const-oid 0.10.2, crypto-bigint 0.7.10, inout 0.2.5, mio 1.2.4, syn 3.0.9, zeroize 1.9.1). Python test tools are installed with `uv pip install --exclude-newer <date>`. Code with no licence, or under GPL-2.0-only, is read for facts only, never copied.
- Committed docs and fixtures omit personal identifiers (device names, addresses, account details). Captures stay out of the repository.

## Phase 0: the go/no-go study (first)

Everything else is wasted if the iPhone won't accept a host that isn't a real Apple TV, and nobody has published the protocol. So before any protocol code, settle these from the iOS 27.2.1 (24B156) and tvOS 27.2 (24K122) binaries, writing each finding into `docs/` as *Confirmed* (read from code) or *Inferred*:

1. **The guest QR.** What tvOS puts in it (tvOS `TVFaceTimeCore`, `DeviceDiscoveryUICore`, `CMContinuityCaptureHost`, `Rapport`), and how iOS reads it (`QRCodeModule`, `BarcodeSupport`, `DSContinuityPairing`, `CMContinuityCaptureRemote`): scheme, fields, encoding, anything signed.
2. **The roles.** Which side advertises `_rdlink._tcp` (with `rpPI`) and which connects; which side is the pair-setup server; the `passwordType`; what `_pi`, `_pinC`, `_pubID` and `_rdsr` mean. (The docs lean towards the iPhone advertising and the host connecting; Warlock's contract example assumed the opposite.)
3. **What the iPhone checks about the host,** before and after pairing: model or device class (TXT `rpMd`, `rpFl` status flags, AWDL's Version TLV device class, BLE adverts), and anything a non-Apple host can't produce: IDS identities, Apple-signed certificates, attestation, FairPlay.
4. **What starts the mic-only session** once paired: the RemoteDisplay session and Continuity Capture control messages.
5. **Media feasibility only:** whether the AVConference media path needs anything Apple-signed. The details are K3's.
6. **Sing as the fallback.** The same questions for the Apple Music Sing guest mic (QR `ContinuitySing://connect?h=…&pw=…`; publicly it opens a "Continuity Microphone" App Clip), which also needs an Apple Music subscription on the iPhone.

**Go** if the QR and every check the iPhone makes can be met with values Koala chooses. **No-go** if any step needs Apple-signed material Koala can't produce, unless the other path avoids it. The result, and the path chosen, is recorded here.

**Result (2026-11-18): go, on the Continuity Camera guest path.** Details and evidence: [`docs/guest-pairing.md`](docs/guest-pairing.md).

- **No Apple-signed material.** Nothing in the flow needs it on either side. The QR is `continuitycamera://connect?_pubID=<UUID>&_pinC=<UUID>&_pi=<UUID>&model=<ProductType>&_rdsr=3`, made of random UUIDs Koala picks.
- **The roles are the reverse of the old guess.** In pairing, the host advertises `_rdlink._tcp` on AWDL (TXT `rpFl=0x80800`, `rpPI=<_pi>`, `rpVr=715.2`) and is the SRP pair-setup server (`passwordType` 10, PIN `_pinC`). The iPhone connects to it. That `rpFl` takes the iPhone down a path with no identity resolution and no IDS requirement, and both sides store each other as SessionPaired identities built from keys each generates itself. (So Warlock's contract example, `koala._rdlink._tcp`, was right.)
- **Capture, roles swapped.** The iPhone is the server, the host connects and pair-verifies with those keys.
- **The main open question** is how to get the iPhone onto AWDL for the capture connection. Apple's host asks over BLE and over a Wi-Fi companion link at once; the companion-link event `_rdNeedsAWDL` is accepted from SessionPaired peers.
- **iPhone-side precondition:** the user's Continuity Camera setting must be on.
- **Sing stays the fallback** and wasn't needed; it would also require Apple Music.

## Phase 1 onwards: crates and milestones

- **`opack`**: depend on [`apple-opack`](https://gitlab.com/clicker-devs/apple-opack) 1.1.0 (MIT; a port of pyatv's, byte-compatible with it) rather than writing one; test against pyatv's encoder.
- **`koala-pairing`** (*done 2026-11-18*): pair-setup and pair-verify, client and server, as state machines over TLV8 messages with no transport.
  - **Why no transport (Parker, 2026-11-18):** Rapport wraps the messages for Koala, and an AirPlay or AirDrop project could wrap the same crate in HTTP. AirPlay itself isn't on Koala's path, so it isn't built first.
  - **Contents:** TLV8, our own (small; `hap-tlv8` is young and AI-assisted). SRP-6a (3072-bit, SHA-512). Ed25519 identities, with an opaque INFO item (0x11) for CoreUtils' OPACK block. X25519 for pair-verify. HKDF-SHA512 and ChaCha20-Poly1305 with the `PS-Msg05`-style nonces.
  - **SRP** is our own module over RustCrypto `srp` 0.7.0-rc.3's arithmetic. That release's client hard-codes the padded-generator M1, which isn't Apple's, so every encoding is chosen in one place (`src/srp.rs`) to match pyatv's srptools, which works against real Apple TVs. Our own `A` and `B` are drawn again if they'd start with a zero byte. Whether Apple pads such values (about 1 in 256) is for RustCrypto's `EncodingOptions::HOMEKIT` ([PAKEs #313](https://github.com/RustCrypto/PAKEs/pull/313), merged 2026-11-09, tested against HomeKitADK's vectors) to settle; it may be used from 2026-11-23.
  - **Tested against pyatv 0.18.0** (`tests/pyatv_oracle.py`, driving `examples/pair_stdio`): 1000 of 1000 rounds each way, pyatv's client against Koala's server and Koala's client against a server on pyatv's Companion test code, through pair-verify, checking the signatures pyatv skips. pyatv needs **srptools below 2.0**: 2.0.0 made its arguments keyword-only, which pyatv 0.18.0's unbounded `srptools>=0.2.0` doesn't survive.
  - **Seen in pyatv's Companion server, to copy:** M2 carries an extra item 27 = `0x01`, and M6's INFO block is OPACK `{altIRK, accountID, model, wifiMAC, name, mac}`.
- **`koala-rapport`** (*done 2026-11-18*): Rapport connections over any byte stream (tokio). It has five parts:
  - frames: type byte plus 24-bit big-endian length;
  - pairing: PS/PV frames carrying `koala-pairing`'s messages under `_pd`, with `_pwTy`/`_auTy`, client and server;
  - encryption after pair-verify: per-direction HKDF keys `ClientEncrypt-main`/`ServerEncrypt-main` with an empty salt, 12-byte little-endian counter nonces, the header as associated data;
  - OPACK requests, responses and events (`_i`, `_x`, `_t`, `_c`), on [`apple-opack`](https://gitlab.com/clicker-devs/apple-opack) 1.1.0;
  - a server that answers pairing until a pair-verify succeeds, with a store of paired peers.
  - **Tested against pyatv's own Companion client, unmodified** (`tests/pyatv_client.py` against `examples/rapport_server`): 20 of 20 rounds. Each round pair-sets-up, verifies on a new connection, then sends a `_systemInfo` request and gets the encrypted response.
  - **The Apple TV 4K check (Koala's client against a real Apple server) is deferred.** The PC is on eduroam, which keeps clients apart: a scan finds no Apple device on the LAN. Koala's guest flow runs Koala as the server in pair-setup, which K2 tests against the iPhone directly. The client side matters again for K3's pair-verify to the iPhone.
  - Other references: Apple's archived [HomeKitADK](https://github.com/apple/HomeKitADK) (Apache-2.0) for the accessory-side formulas and known-answer tests; [`idevice`](https://github.com/jkcoxson/idevice) (MIT) for an iPhone pairing with a non-Apple accessory.
- **`ccapture`**: the RemoteDisplay guest-pairing host and the Continuity Capture control protocol (`kCMContinuityCaptureEvent*`).
- **`avc-media`**: the AVConference negotiation blob, RTP/SRTP ([`rtc-srtp`/`rtc-rtp`](https://github.com/webrtc-rs/rtc), MIT/Apache, which take keys directly), Opus (`opus` 0.4.0 on libopus, BSD; pin `opusic-sys` 0.7.6). The Mac asks for AVC codec 16 (inferred: Opus 48 kHz), 4 channels, in "call" mode. If AAC-ELD turns up, an LGPL FFmpeg build (FDK-AAC is GPL-incompatible). A gPTP/TimeSync responder only if captures show it's needed.
- **Audio out:** `wasapi` into "CABLE Input", with `rubato` correcting drift.
- **UI:** deferred. Until then, a CLI that shows the QR in the terminal.

Milestones (exit criteria):

- **K0 go/no-go:** Phase 0's questions answered in `docs/`, the path chosen. *Done 2026-11-18: go, Continuity Camera guest path.*
- **K1 Rapport:** Koala's client pair-sets-up (PIN on the TV) and pair-verifies with the Apple TV 4K over the LAN and exchanges an E_OPACK request; pyatv's client pairs with Koala's server, including a fixed-PIN (`passwordType` 10) SessionPaired pair-setup with the identity info block.
- **K2 (moment of truth):** the iPhone scans Koala's QR, finds Koala's `_rdlink._tcp` over Warlock, and completes guest pair-setup; its Continuity Camera screen opens in mic-only mode.
  - *Done 2026-11-18, on the fourth try* (`docs/guest-pairing.md`, "Tried with an iPhone"). The iPhone (iOS 27.2.1) paired over AWDL and entered a mic-only discovery session with Koala as its confirmed device.
  - The tries found two things the reverse engineering hadn't shown: encryption starts straight after pair-setup, and a response must echo `_x` at its sent width.
  - The tool that made the difference was the iPhone's own log over USB: pymobiledevice3 (GPL-3.0, a tool only) in a venv, `syslog live -ei 'rapportd|ContinuityCapture|…'`.
- **K3:** the iPhone brought onto AWDL (`_rdNeedsAWDL`), Koala connected to its RDLink server and pair-verified, and a mic-only `com.apple.continuitycapture` session negotiated. Needs more RE first: the control messages, the NSKeyedArchiver configuration, the stream descriptors and the AVConference/SRTP keying.
- **K4:** the iPhone's microphone plays through "CABLE Output" with stable latency and drift. The repository is published around here.

## Risks

- The iPhone rejects a host that isn't a genuine Apple TV, or needs something only Apple can sign. Phase 0 found no such check in the code (2026-11-18), but only the iPhone itself can confirm it (K2).
- The iPhone won't come onto AWDL for the capture connection without Apple's BLE or Wi-Fi request, and neither has been tried (K3).
- The AVConference negotiation blob is opaque, and SRTP keying is untraced. Mitigation: RE of macOS's and iOS's AVConference.
- No ground truth from a real guest session: every protocol detail comes from static RE, so the iPhone is the only test of the host side.
- Young, low-adoption crates (`apple-opack`, `hap-tlv8`): review them like our own code, and test them against pyatv.

## Verification

- pre-commit hooks on every commit (`.pre-commit-config.yaml`, as Warlock's): rustfmt (`rustfmt.toml`, Warlock's), Clippy with `-D warnings`, cargo-deny (`deny.toml`), whitespace, line-ending, YAML and TOML checks.
- The pyatv oracles, after any change to pairing or Rapport. Use a venv with `pyatv==0.18.0` and `srptools<2`, then:
  - `python -I crates/koala-pairing/tests/pyatv_oracle.py target/release/examples/pair_stdio.exe 1000`
  - `python -I crates/koala-rapport/tests/pyatv_client.py target/release/examples/rapport_server.exe 20`
- Unit tests against pyatv's and HomeKitADK's vectors; pyatv as a live peer for both sides of pairing; the Apple TV 4K on the LAN for the client side.
- The iPhone over Warlock for K2 onwards, with Warlock's captures (`warlockctl`, the adapter in Wireshark) and the macOS 11 MacBook as AWDL witnesses.
