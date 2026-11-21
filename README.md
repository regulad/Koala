# Koala

An iPhone as a microphone for Windows, with no app on the iPhone.

Apple TVs let a guest's iPhone act as their microphone: the TV shows a QR
code, the iPhone scans it, pairs, and sends its microphone's audio. Koala
plays the Apple TV's part on Windows. It reaches the iPhone over Apple
Wireless Direct Link (AWDL), which [Warlock](https://github.com/regulad/Warlock)
gives Windows. Each of the iPhone's microphones becomes a Windows
microphone of its own, which other programs record from.

Koala needs Warlock 0.2.4 or later, and what Warlock needs, a Bluetooth LE
adapter included: Warlock asks the iPhone onto AWDL over Bluetooth LE.

## Privacy

Koala sends crash reports and diagnostics to Sentry, without anything that
identifies you, this computer or the iPhone: [`PRIVACY.md`](PRIVACY.md) says
what. Set `DO_NOT_TRACK=1` and it sends nothing.

**Status: research.** Nothing works yet. [`PLAN.md`](PLAN.md) has the design
and where each part stands.

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

## Licence

Koala is free software, under the GNU Affero General Public License, version 3
or (at your option) any later version ([`LICENSE.md`](LICENSE.md)).

Koala is not affiliated with Apple. Apple TV, iPhone, AWDL and Continuity are
Apple's.
