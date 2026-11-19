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
- **Dependencies** must be at least two weeks old, AGPL-compatible (Apache-2.0 is fine here, unlike in Warlock; GPL-2.0-only isn't), and vetted for provenance. A `deny.toml` enforces this from the first crate on, with the workspace `Cargo.toml` and Warlock's `rustfmt.toml` (cargo's hooks need a package to run on). Code with no licence, or under GPL-2.0-only, is read for facts only, never copied.
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
- **`tlv8`**: [`hap-tlv8`](https://github.com/phunapps/hap-rust) 1.0.0 (Apache-2.0) or our own ~100 lines; test against pyatv's.
- **`rapport`**: framing (type u8 + length u24), pair-setup client and server (SRP-6a, 3072-bit, SHA-512, Apple's M1), pair-verify (X25519, Ed25519, HKDF-SHA512), ChaCha20-Poly1305 sessions, the OPACK request/response/event layer (`_i`, `_x`, `_t`, `_c`).
  - SRP: RustCrypto `srp` 0.7.0-rc.3 with `new_with_options(true)` (1-byte g in M1, Apple's form; as rairplay does). It still hashes A as received, so about 1 pair-setup in 256 (a leading zero) would fail. Upstream's `EncodingOptions::HOMEKIT` ([PAKEs #313](https://github.com/RustCrypto/PAKEs/pull/313), merged 2026-11-09, tested against HomeKitADK's vectors) fixes it, and may be used from 2026-11-23.
  - Oracles: [pyatv](https://github.com/postlund/pyatv) (MIT) both ways: its client against Koala's server, and Koala's client against pyatv's fake device and the real Apple TV 4K on the LAN. Apple's archived [HomeKitADK](https://github.com/apple/HomeKitADK) (Apache-2.0) for the accessory-side formulas and known-answer tests. [`idevice`](https://github.com/jkcoxson/idevice) (MIT) as a reference for an iPhone pairing with a non-Apple accessory.
- **`ccapture`**: the RemoteDisplay guest-pairing host and the Continuity Capture control protocol (`kCMContinuityCaptureEvent*`).
- **`avc-media`**: the AVConference negotiation blob, RTP/SRTP ([`rtc-srtp`/`rtc-rtp`](https://github.com/webrtc-rs/rtc), MIT/Apache, which take keys directly), Opus (`opus` 0.4.0 on libopus, BSD; pin `opusic-sys` 0.7.6). The Mac asks for AVC codec 16 (inferred: Opus 48 kHz), 4 channels, in "call" mode. If AAC-ELD turns up, an LGPL FFmpeg build (FDK-AAC is GPL-incompatible). A gPTP/TimeSync responder only if captures show it's needed.
- **Audio out:** `wasapi` into "CABLE Input", with `rubato` correcting drift.
- **UI:** deferred. Until then, a CLI that shows the QR in the terminal.

Milestones (exit criteria):

- **K0 go/no-go:** Phase 0's questions answered in `docs/`, the path chosen. *Done 2026-11-18: go, Continuity Camera guest path.*
- **K1 Rapport:** Koala's client pair-sets-up (PIN on the TV) and pair-verifies with the Apple TV 4K over the LAN and exchanges an E_OPACK request; pyatv's client pairs with Koala's server, including a fixed-PIN (`passwordType` 10) SessionPaired pair-setup with the identity info block.
- **K2 (moment of truth):** the iPhone scans Koala's QR, finds Koala's `_rdlink._tcp` over Warlock, and completes guest pair-setup; its Continuity Camera screen opens in mic-only mode.
- **K3:** the iPhone brought onto AWDL (`_rdNeedsAWDL`), Koala connected to its RDLink server and pair-verified, and a mic-only `com.apple.continuitycapture` session negotiated. Needs more RE first: the control messages, the NSKeyedArchiver configuration, the stream descriptors and the AVConference/SRTP keying.
- **K4:** the iPhone's microphone plays through "CABLE Output" with stable latency and drift. The repository is published around here.

## Risks

- The iPhone rejects a host that isn't a genuine Apple TV, or needs something only Apple can sign. Phase 0 found no such check in the code (2026-11-18), but only the iPhone itself can confirm it (K2).
- The iPhone won't come onto AWDL for the capture connection without Apple's BLE or Wi-Fi request, and neither has been tried (K3).
- The AVConference negotiation blob is opaque, and SRTP keying is untraced. Mitigation: RE of macOS's and iOS's AVConference.
- No ground truth from a real guest session: every protocol detail comes from static RE, so the iPhone is the only test of the host side.
- Young, low-adoption crates (`apple-opack`, `hap-tlv8`): review them like our own code, and test them against pyatv.

## Verification

- pre-commit hooks on every commit (`.pre-commit-config.yaml`, as Warlock's): whitespace, line-ending, YAML and TOML checks, and from the first crate rustfmt, Clippy with `-D warnings`, and cargo-deny.
- Unit tests against pyatv's and HomeKitADK's vectors; pyatv as a live peer for both sides of pairing; the Apple TV 4K on the LAN for the client side.
- The iPhone over Warlock for K2 onwards, with Warlock's captures (`warlockctl`, the adapter in Wireshark) and the macOS 11 MacBook as AWDL witnesses.
