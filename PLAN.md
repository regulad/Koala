# Plan: Koala (an iPhone as a Windows microphone), in Rust

## Context

Koala makes an unmodified iPhone work as a microphone on Windows. It impersonates an Apple TV hosting a guest, mic-only Continuity Camera session: tvOS's QR/PIN guest pairing, which gives the iPhone a Rapport `SessionPaired` identity for the host. Reverse engineering showed that guest pairing runs **only over AWDL**, which Windows lacks. [Warlock](https://github.com/regulad/Warlock) (0.1.0, released 2026-11-18) now gives Windows AWDL, so Koala can start.

The research so far is in Warlock's `docs/`, with Koala's parts in `macos-continuity-camera.md` (macOS 27 hosts) and `tvos-ios-continuity-sing.md` (iOS/tvOS 27, the Apple Music Sing path). Those docs move here when Koala is published; until then, this plan points at them.

## Pinned decisions

- **All Rust.** Licence AGPL-3.0-or-later (`LICENSE.md`, the FSF text).
- **AWDL comes from Warlock**, through its control API and `warlock-client` (GPL-2.0-only OR AGPL-3.0-or-later; Koala takes it under the AGPL). Koala links no Warlock internals. Koala's own services are registered with Windows' DNS-SD on the Warlock adapter, and Warlock is asked to advertise their types (Warlock's `docs/service-discovery.md`).
- **Audio goes out as Windows microphones, one per iPhone microphone (Parker, 2026-11-20).** Each of the iPhone's channels is one of its microphones (`docs/guest-pairing.md`, "The microphones"), and each becomes a mono microphone of its own. Koala emulates a USB Audio Class 2 device over USB/IP, attached through [usbip-win2](https://github.com/vadimgrn/usbip-win2) (BSD-2-Clause, Microsoft-signed), so Windows' own class driver makes the microphones. That replaces the VB-Audio cable, whose redistribution terms were in doubt. There is still no audio driver of our own.
- **Video goes out as a Windows camera: a Media Foundation virtual camera (Parker, 2026-11-21).** Its media source is a DLL of Koala's that Windows' camera service loads; Koala decodes the iPhone's HEVC with Windows' decoder where there is one (the HEVC Video Extensions, on the GPU where it can), else with FFmpeg's, and "Force software decoding" makes it FFmpeg's. Chosen over a USB Video Class device on the microphones' USB/IP. Details: [`docs/virtual-camera.md`](docs/virtual-camera.md).
- **Distribution through winget, with what Koala needs as package dependencies (Parker, 2026-11-20).** winget installs these first:
  - `Microsoft.WindowsAppRuntime.2`, minimum 2.6.4: the Windows App Runtime that `koala-tray`'s WinUI needs (Reactor's, at the pinned commit);
  - `vadimgrn.usbip-win2`, minimum 0.9.8.0 (Parker, 2026-11-21): earlier versions can crash Windows when an app closes an emulated microphone's stream ([#181](https://github.com/vadimgrn/usbip-win2/issues/181)). Koala is developed against GitHub's 0.9.8.1. As of 2026-11-21 winget's newest is only 0.9.7.8, so the dependency waits for winget to catch up, and until then the README sends people to GitHub's release. Its install needs administrator rights (drivers) and restarts every USB 3 root hub, whose upper filter it adds. Attaching a device doesn't need them: an unelevated `usbip attach` reaches the server;
  - `Regulad.Warlock`, as Warlock's own plan has it.

  There's no manifest yet: Koala has no installer. Its manifests will be written from its package as Warlock's are (`warlock-msi winget`), with these in `Dependencies` → `PackageDependencies`.
- **Trust path:** impersonate an Apple TV and use its guest QR/PIN pairing. Cloning a same-Apple-Account identity is out.
- **No new hardware (Parker, 2026-11-18).** There is no Apple TV that can host a guest session here (Continuity Camera needs an Apple TV 4K of the 2nd generation or later, the Sing guest mic the 3rd), so the ground truth is static reverse engineering of the iOS and tvOS 27 images, then trials against the iPhone itself. Available: an iPhone on iOS 27; an Apple TV 4K (1st generation, which can't run tvOS 27) on the LAN, which speaks Rapport (`_companion-link._tcp`) but can't host; a jailbroken 3rd-generation Apple TV (iOS 8); a 2015 MacBook on macOS 11 as an AWDL witness; the RE workspace on the lab Mac `overture` (`~/koala-re`, with `ipsw` and Ghidra) and Ghidra on this PC.
- **Local until a working prototype (Parker, 2026-11-18).** The code goes to GitHub only once something works. Its pipeline (`.github/`, "Pipeline" below) is written already and takes effect with that push. The repository, [`regulad/Koala`](https://github.com/regulad/Koala), exists already, empty, for its issues (Parker, 2026-11-20).
- **Dependencies** must be at least two weeks old, AGPL-compatible (Apache-2.0 is fine here, unlike in Warlock; GPL-2.0-only isn't; the WTFPL counts as MIT does, Parker, 2026-11-20), and vetted for provenance. `deny.toml` enforces the licences and sources. Age covers transitive crates too: `Cargo.lock` pins older versions where the newest is too young (2026-11-18: bytes 1.12.1, const-oid 0.10.2, crypto-bigint 0.7.10, inout 0.2.5, mio 1.2.4, syn 3.0.9, zeroize 1.9.1; 2026-11-20: ffmpeg-next 9.0.0, ffmpeg-sys-next 9.0.1, regex 1.13.1, regex-automata 0.4.18, regex-syntax 0.8.11). Python test tools are installed with `uv pip install --exclude-newer <date>`. Code with no licence, or under GPL-2.0-only, is read for facts only, never copied.
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
- **Audio out:** a USB/IP server in Koala, emulating a UAC2 device with one mono input per microphone, named from `koala-capture`'s mic map. Its asynchronous IN endpoints let the device follow the iPhone's clock. Prior art: [network-audio/usbip-audio](https://github.com/network-audio/usbip-audio) (MIT). *Started 2026-11-20:*
  - the mic map (`crates/koala-capture/data/mic-maps.json`, from every iOS 27 iPhone's device tree by `tools/mic_maps.py`);
  - one output per microphone (`crates/koala/src/mic.rs`), to Windows or to a WAV, or both;
  - the microphones in Windows (`crates/koala/src/usb_mics.rs`, 2026-11-21). On usbip-audio (MIT, a git dependency at its 2026-11-07 commit), a device per microphone, one channel at 48 kHz.
    - They're named "Microphone (Parker’s iPhone: Back)" (Parker, 2026-11-21): Windows takes a microphone's name from its device's product string, not its terminal's.
    - They're attached as the iPhone's audio starts, unelevated, and detached when it ends.
    - The clock follows the iPhone's, steered by the samples waiting.
    - The tray's "Stream microphones" (on by default) and `koala guest --usb-mics` turn them on.
    - Trial 34: four microphones, all of which Windows recorded from, attached and detached with the session.
    - The ids: pid.codes' 1209:4B4F, asked for in pidcodes/pidcodes.github.com#1398 (Parker, 2026-11-21). If it's refused, Koala changes it.
  - **Rust prior art (surveyed 2026-11-21).** No published crate gives a Windows UAC2 capture device over USB/IP.
    - References: usbip-audio (UAC2, high speed, iso pacing; its CI records through usbip-win2 0.9.8.1) and [Sebas](https://github.com/sebas-dot-zip/sebas-virtual-usb-device) (MIT OR Apache-2.0; a UAC1 microphone over usbip-win2, whose comments record its quirks).
    - Worth a spike: [vusb](https://github.com/oxrok/vusb) (MIT OR Apache-2.0), the only published crate with isochronous USB/IP and UVC/UAC examples, but young and untried on Windows.
    - Not the [`usbip`](https://github.com/jiegec/usbip) crate: its server drops isochronous packets and can't express alternate settings or IADs.
    - Descriptors by hand, checked against [usbd-uac2](https://github.com/ktims/usbd-uac2) and embassy-usb's UAC2 `io_box`, and parsed back by `nusb` in tests.
  - **usbip-win2's quirks:**
    - Every device is high speed, so 1 ms isochronous is `bInterval` 4.
    - Capture broke in 0.9.8.1 for a server returning `start_frame` 0 ([#200](https://github.com/vadimgrn/usbip-win2/pull/200)), so echo the client's.
    - SET_INTERFACE reaches the server only with its filter driver installed ([#177](https://github.com/vadimgrn/usbip-win2/issues/177)).
  - **Open:**
    - Whether one audio function's several mono streaming interfaces give Windows one microphone each is untried; none of the prior art has more than one capture stream. So the plan is a device per microphone (2026-11-21).
  - **Settled (usbip-win2's minimum):** 0.9.7.8, winget's copy, crashed Windows (bugcheck 0x139 in usbaudio) when an app closed an emulated device's stream after alternate-setting switches ([#181](https://github.com/vadimgrn/usbip-win2/issues/181)). The fix is in 0.9.8.0, which is now the minimum (above).
- **The camera** (trial 23, `docs/guest-pairing.md`, "The camera"): HEVC, natively, over RTP, which Koala puts back into pictures (`koala-capture`'s `video`) and records to a Matroska file with FFmpeg's muxer (`crates/koala/src/mkv.rs`; Matroska so a recording cut short still plays, Parker, 2026-11-20). FFmpeg is linked, libavformat through [ffmpeg-next](https://github.com/zmwangx/rust-ffmpeg) (WTFPL), against BtbN's LGPL (version 3) shared build of FFmpeg 9.0, which `tools/ffmpeg.ps1` fetches and checks; its DLLs go beside the programs (Parker, 2026-11-20). Windows plays the recordings with Microsoft's HEVC Video Extensions; for the virtual camera, libavcodec is to decode where those aren't installed (Parker, 2026-11-20).
  - **The virtual camera** ([`docs/virtual-camera.md`](docs/virtual-camera.md)): *started 2026-11-21*.
    - Built: the decoding (`crates/koala/src/decode.rs`), the media source (`crates/koala-vcam-source`), its shared memory and the camera (`crates/koala-vcam`), `koala guest --vcam`, and the tray's camera: named after the iPhone and there while it's connected, as a Mac has it, and turned on or off by "Stream camera" (on by default; Parker, 2026-11-21). `koala vcam --replay` plays a recording into the camera.
    - The iPhone's camera, live, reaches the Windows Camera app through it (trial 32, and from the tray).
    - **Open:** an installer registering the source; the source's own diagnostics (Parker, 2026-11-21: it runs in Windows' camera service, so it needs a Sentry project of its own, `koala-vcam`, and `DO_NOT_TRACK` passed in through the camera's attributes); formats other than NV12.
- **UI: `koala-tray`, WinUI 3 through windows-rs's [Reactor](https://github.com/microsoft/windows-rs/tree/master/crates/libs/reactor) (Parker, 2026-11-20).** A notification-area icon whose menu shows "Koala", the status ("Warlock service unavailable", "Disconnected", "Connecting to …", "Connected to …"), Connect, Options ("Start on logon": the user's `Run` key, the documented way for an unpackaged app), About and Quit. Connect shows the QR code, 4 inches square on the screen (from the primary monitor's reported size), in the work area's corner nearest the notification area, until the iPhone is connected. Until the code is ready the cursor is Windows' "working in background" one, set system-wide since the mouse is over no window of Koala's, and the window opens only with the code or a failure: no "Getting ready" (Parker, 2026-11-21). An attempt with no code after 20 s fails, so the cursor can't stay up. About shows the licence and every dependency's (`tools/third_party_notices.py`, kept current by a test). `koala` stays the console tool for trials.
  - **Reactor is a preview.** It comes from windows-rs's master at a commit at least two weeks old (its last release, 0.100.0, has no notification icon); its API changes daily.
  - **Its limits, accepted for now:** its menu items are plain text, so the status row is the menu's size, About has no icon, and "Start on logon" shows its state as a check mark in its label. Reactor opens the menu for a right-click only; a left click opens it by posting Reactor's own right-click message. The QR window's place, staying on top and staying out of the taskbar are set on its native window, which Reactor hands over.
  - **It needs the Windows App Runtime (2.6) installed**, Microsoft's, which Koala uses but doesn't ship: winget installs it as a dependency ("Pinned decisions"). Shipping it (`windows-reactor-setup`, self-contained) would need its licence checked first.
  - Tried first and dropped: Win32 menus and GDI, which looked dated and wouldn't follow dark mode (only undocumented uxtheme calls do, and they didn't take on build 26300).
  - **The status row says what Warlock reports missing (Parker, 2026-11-20):** while nothing runs, "No Wi-Fi adapter for Warlock" (`radio.state` `absent`), "No Bluetooth 5 adapter" (no adapter, or no LE extended advertising) or "Bluetooth is off", from `GET /v1/info` (Warlock 0.2.5's `bluetooth`), with Connect greyed out.
- **Diagnostics: Sentry, with nothing that identifies anyone (Parker, 2026-11-20; `PRIVACY.md`, `crates/koala/src/diagnostics.rs`).** Modelled on RosellaView's, not PepperCord's, which sends user IDs.
  - **What:** panics and the errors that end a run or a session; a session per run (release health); a transaction per capture session with the iPhone (its kind, length and end), and no others: Koala's requests to Warlock were transactions too until Parker had them dropped, since Warlock reports its own (2026-11-21); system details (Windows' version, the processor, memory, the architecture, what Warlock reports of its radio and Bluetooth); the iPhone's model and iOS build only.
  - **Not:** names (the user's, the computer's, the iPhone's), IDs, addresses, keys. `server_name` is the program's, not the computer's the SDK would take; every event and breadcrumb is redacted (addresses, UUIDs, long hex, the user's folder) and module paths cut to file names; a test sends through the SDK and checks. The tray's 2 s polling of Warlock isn't traced: a health probe, as PepperCord leaves its own.
  - **Off with `DO_NOT_TRACK`** set to anything but empty, `0` or `false`; neither RosellaView nor PepperCord has an opt-out.
  - **The SDK:** `sentry` 0.49.5 (MIT), over reqwest with Windows' own TLS and certificate store. Not ureq: its native TLS bundles Mozilla's roots under CDLA-Permissive-2.0, which `deny.toml` doesn't allow.
  - **The DSN** is in the source (`regulad/koala`, platform `rust`, made by the homelab's `tofu/sentry`, 2026-11-21). Its code mappings take `D:\a\Koala\Koala\` (GitHub's runner) and `D:\repositories\Koala\` (development) to the repository, and `third_party\warlock\` to Warlock's: Sentry matches them as literal prefixes of frames' paths, so a build that remaps paths needs new ones.
  - **First events (2026-11-21):** a 5 s `koala guest` with `DO_NOT_TRACK=0` for its process only (Parker's account has it at 1, which Koala honoured): its four requests' transactions reached Sentry with no user, no IP address, `server_name` `koala` and `server.address` `warlock`, and the run's release. Releases are `koala@<version>`, `+<commit>` for any build but a tagged one; environments `production` (tagged) and `development`.
  - **Sentry's location is kept (Parker, 2026-11-21: "leave that on"):** Sentry works out where each report came from, to the city, from the connection, before it discards the IP address, even with IP addresses not stored (its docs, "Server-Side Data Scrubbing"; the first events: Raleigh). An Advanced Data Scrubbing rule, `[Remove] [Anything] from [$user.geo.**]`, would drop it. `PRIVACY.md` and the About window say so.
  - **Symbols:** a tag's build uploads both programs and their PDBs to Sentry (`build.yml`'s "Debug files to Sentry", sentry-cli 3.8.2 pinned by its SHA-256, with `SENTRY_AUTH_TOKEN` from the `release` environment), in a job of its own away from both the build and the signing. The release profile keeps line tables (`debug = "line-tables-only"`, as Warlock's) so they name files and lines. A release and deploy in Sentry, as Warlock's build makes, wait for Koala's GitHub releases: the deploy marks a publication, and Koala's build doesn't publish yet.
  - **Only releases send (Parker, 2026-11-21, as Warlock's #1 asked of Warlock):** the DSN is built in for a tagged build, or one built with `KOALA_DIAGNOSTICS=1` for a trial (`build.rs`). A build from source, or a `testing` run, has none and sends nothing.
  - **Open:** uploading the PDBs from tagged builds, so Sentry can symbolicate stacks. `SENTRY_AUTH_TOKEN` is in the `release` environment (an organization token; sentry-cli takes the project, `koala`, from us), so it needs only a job of its own, away from both the build (dependencies' build scripts) and the signing (GitHub's and Microsoft's actions only).

Milestones (exit criteria):

- **K0 go/no-go:** Phase 0's questions answered in `docs/`, the path chosen. *Done 2026-11-18: go, Continuity Camera guest path.*
- **K1 Rapport:** Koala's client pair-sets-up (PIN on the TV) and pair-verifies with the Apple TV 4K over the LAN and exchanges an E_OPACK request; pyatv's client pairs with Koala's server, including a fixed-PIN (`passwordType` 10) SessionPaired pair-setup with the identity info block.
- **K2 (moment of truth):** the iPhone scans Koala's QR, finds Koala's `_rdlink._tcp` over Warlock, and completes guest pair-setup; its Continuity Camera screen opens in mic-only mode.
  - *Done 2026-11-18, on the fourth try* (`docs/guest-pairing.md`, "Tried with an iPhone"). The iPhone (iOS 27.2.1) paired over AWDL and entered a mic-only discovery session with Koala as its confirmed device.
  - The tries found two things the reverse engineering hadn't shown: encryption starts straight after pair-setup, and a response must echo `_x` at its sent width.
  - The tool that made the difference was the iPhone's own log over USB: pymobiledevice3 (GPL-3.0, a tool only) in a venv, `syslog live -ei 'rapportd|ContinuityCapture|…'`.
- **K3:** the iPhone brought onto AWDL (`_rdNeedsAWDL`), Koala connected to its RDLink server and pair-verified, and a mic-only `com.apple.continuitycapture` session negotiated. Needs more RE first: the control messages, the NSKeyedArchiver configuration, the stream descriptors and the AVConference/SRTP keying.
- **K4:** the iPhone's microphones appear in Windows as microphones, one each, with stable latency and drift. The repository is published around here.
  - *Mostly done 2026-11-21* (trial 34): one each, named after the iPhone, recorded from by Windows. Latency and drift over a long session are yet to be measured.

## Pipeline (2026-11-20, from Warlock's)

- **`.github/workflows/check.yml`:** every pull request and push to master runs the pre-commit hooks over the whole tree, then the tests.
- **`.github/workflows/build.yml`:** a `v*` tag, or a run started by hand, builds `koala.exe` and `koala-tray.exe`, then a separate job signs them with Azure Artifact Signing, as Warlock's does. A tag signs from the `release` environment, a run by hand from `testing`.
  - It stops at the signed programs. Packaging, winget's manifests and a GitHub release, as in Warlock's, come with Koala's installer.
- **Warlock is a submodule,** `third_party/warlock`, pinned to the release whose `warlock-client` Koala builds against (now `v0.2.2`); Cargo.toml takes the crate by path from it. Both workflows check it out (`submodules: true`), but not Warlock's own submodules, which only its radio needs. Moving to a new Warlock release is a submodule bump, by hand. They fetch windows-rs's 3 GB history with git's own fetch (`CARGO_NET_GIT_FETCH_WITH_CLI`), the faster one.
- **`.github/dependabot.yml`:** GitHub Actions and the pre-commit hooks, weekly, after two weeks. Not Cargo: its updates can't resolve a path into a submodule (dependabot/dependabot-core#11861). Its security alerts read Cargo.lock, so they still cover the Rust dependencies.
- **`.github/settings.yml`:** the repository's settings and labels, through the Probot Settings app, as Warlock's.
- **Before the first push** (Parker's homelab configuration, as for Warlock): the Settings app installed on `regulad/Koala`, and the `release` and `testing` environments with the signing secrets (the environments and their secrets are in place, 2026-11-20).

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
