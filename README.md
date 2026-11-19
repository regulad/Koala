# Koala

An iPhone as a microphone for Windows, with no app on the iPhone.

Apple TVs let a guest's iPhone act as their microphone: the TV shows a QR
code, the iPhone scans it, pairs, and sends its microphone's audio. Koala
plays the Apple TV's part on Windows. It reaches the iPhone over Apple
Wireless Direct Link (AWDL), which [Warlock](https://github.com/regulad/Warlock)
gives Windows, and plays the audio into a virtual audio cable that other
programs record from.

**Status: research.** Nothing works yet. [`PLAN.md`](PLAN.md) has the design
and where each part stands.

## Licence

Koala is free software, under the GNU Affero General Public License, version 3
or (at your option) any later version ([`LICENSE.md`](LICENSE.md)).

Koala is not affiliated with Apple. Apple TV, iPhone, AWDL and Continuity are
Apple's.
