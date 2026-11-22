# Koala

<img height="270" alt="Screenshot 2026-11-21 204316" src="https://github.com/user-attachments/assets/8b16b101-d77a-4533-b1b7-54468f1500e2" />
<img height="600" alt="Screenshot 2026-11-21 204255" src="https://github.com/user-attachments/assets/4f4f5a5b-ea0c-4ab4-8b84-ece6a3a13708" />
<img height="600" alt="IMG_0445" src="https://github.com/user-attachments/assets/48dca47c-fb26-4f67-9f34-038eb152d096" />
<img height="500" alt="Screenshot 2026-11-21 210945" src="https://github.com/user-attachments/assets/ae8caafb-0305-4100-b2e2-099bf5714d68" />

<br>

A full open-source implementation of Apple's closed-source Wombat feature, 
better known as Continuity Camera. Koala's latency is on-par with Wombat.

Koala provides a standard Microphone and Camera device that other Windows
applications can use. Koala can be ported to other platforms; care has 
been taken to limit the amount of code that ties it to Windows in the core.

Koala works with Warlock to connect to your iPhone the same way a Mac 
or Apple TV would. It doesn't need to contact Apple to provision an 
identity, and all communication is encrypted over-the-air.

## About

Apple TVs let a guest's iPhone act as their microphone: the TV shows a QR
code, the iPhone scans it, pairs, and sends its microphone's audio. Koala
plays the Apple TV's part on Windows. It reaches the iPhone over Apple
Wireless Direct Link (AWDL), which [Warlock](https://github.com/regulad/Warlock)
gives Windows. Each of the iPhone's microphones becomes a Windows
microphone of its own, which other programs record from.

Koala needs Warlock 0.2.4 or later, and what Warlock needs, a Bluetooth LE
adapter included: Warlock asks the iPhone onto AWDL over Bluetooth LE. With
Warlock 0.2.5 or later, Koala's menu also says when that adapter is missing,
off, or without the Bluetooth 5 features it needs.

For the microphones, Koala needs [usbip-win2](https://github.com/vadimgrn/usbip-win2)
0.9.8.0 or later, whose driver attaches them to Windows. Earlier versions
can crash Windows when an app closes such a microphone
([#181](https://github.com/vadimgrn/usbip-win2/issues/181)). (As of 21
November 2026, the newest on winget is only 0.9.7.8: install GitHub's
release.)

## Privacy

Koala's releases send crash reports and diagnostics to Sentry, without
anything that identifies you, this computer or the iPhone:
[`PRIVACY.md`](PRIVACY.md) says what. Set `DO_NOT_TRACK=1` and it sends
nothing.

## Build

You need Rust (stable), PowerShell 7 and LLVM (for libclang, which
generates the FFmpeg bindings). Warlock comes in as a submodule, for its
client crate; its own submodules aren't needed. Koala links FFmpeg, whose
build `tools/ffmpeg.ps1` fetches; the FFmpeg DLLs end up beside the
programs.

```text
git clone https://github.com/regulad/Koala
cd Koala
git submodule update --init
pwsh tools/ffmpeg.ps1
cargo build --release
cargo test --workspace
```

A build from source sends no diagnostics: only a release's has Sentry's
address built in. To try them, build with `KOALA_DIAGNOSTICS=1` set.

## Licence

Koala is free software, under the GNU Affero General Public License, version 3
or (at your option) any later version ([`LICENSE.md`](LICENSE.md)).

Koala is not affiliated with Apple. Apple TV, iPhone, AWDL and Continuity are
Apple's.
