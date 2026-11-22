# AGENTS.md

Conventions for anyone, person or agent, changing or running Koala.
`PLAN.md` holds the design and its decisions; this file holds the rules that
apply to every change and every trial.

## Show the guest QR code in the terminal only

`koala guest` prints the QR code to the terminal, and that render is the
only form it takes. Don't save it as an image or open it in another app
(Photos or any viewer); an agent running a trial shows Parker the terminal
render itself, as printed (Parker, 2026-11-20).

**Why:** the iPhone scans the terminal render from the screen. Opening a
bitmap in Photos added a window to every trial and a file to `%TEMP%` per
run, for nothing the terminal didn't already do.

## The virtual camera's source runs installed, not as built

Windows' camera service loads `koala_vcam_source.dll` from where
`tools/vcam.ps1` installed it (`%ProgramData%\Koala`, registered under
`HKEY_LOCAL_MACHINE`), not from `target`. After changing
`crates/koala-vcam-source`, build it (`cargo build --release -p
koala-vcam-source`) and install it again with `sudo pwsh tools/vcam.ps1`,
telling Parker a UAC prompt is coming; the script stops the camera services
so that they let go of the old copy (`docs/virtual-camera.md`).

**Why:** a trial run against the old copy tests nothing that changed, and
says so nowhere.

Koala's package (`koala-msi`) registers the same class, for its copy in
`C:\Program Files\Koala`: whichever registered last is the one the service
loads. `tools/vcam.ps1 -Uninstall` removes the class's keys, an installed
package's too; repairing the package puts them back.

## Before a hardware trial, WSL is off

On WINLITE, a live kernel dump while WSL's virtual machine runs bugchecks
Windows (0x133, microsoft/WSL#10843; 2026-11-21). Check that
`wsl --list --running` lists no distribution and that no `vmmemWSL` runs; ask
Parker before shutting WSL down.

**Why:** a camera stream hung in Stop makes the camera service ask for a
live kernel dump, and with WSL running that dump stops Windows, as in trial
30 (`docs/virtual-camera.md`).

After a crash, copy the dump out of `C:\Windows\Minidump` with sudo and read
it with cdb. Camera_FrameServer event 13 names a hung stream.

## When a trial's scan fails

- **The iPhone never connects:** check `warlockctl peers` (is the iPhone
  listed? who is master?) and the iPhone's AWDLStateDump (`peers:`, `Rx:`)
  before touching Koala. A bystander master can split Warlock from the
  iPhone (crappletv, 2026-11-20).
- **The QR scan times out though rapportd found `_rdlink`:** if "Skip using
  NearbyActionV2" names a device other than the Activate target, and no
  "Bonjour AWDL browser start -- triggered by: Camera" follows, rapportd
  holds a stale session. Ask Parker to restart the iPhone before debugging
  (2026-11-20, 2026-11-21; the trigger is unconfirmed). Trials 36 and 37 in
  `docs/guest-pairing.md` were this.

## Keep the diagnostics anonymous, and `PRIVACY.md` true

What Koala sends to Sentry (`crates/koala/src/diagnostics.rs`, and the
virtual camera's source's, `crates/koala-vcam-source/src/diagnostics.rs`,
both on `crates/koala-diagnostics`) never identifies the user, this
computer or the iPhone: no names (the user's, the
computer's, the iPhone's), user or device IDs, network addresses, keys or
pairing identifiers (Parker, 2026-11-20). `PRIVACY.md` lists what is sent,
and it changes in the same commit as anything that adds to it.

**Why:** the privacy policy promises exactly this, and it's Parker's word to
the people who run Koala. A diagnostic that quietly carries an identifier
breaks the promise without anyone noticing.

**How:**
- Send facts Koala chose, not text it was handed: a breadcrumb or an error
  names the step or what failed, not the iPhone's name or a peer's address.
  Koala's console output (frame dumps, pairing IDs) never goes to Sentry.
- Anything that does go out passes through `redact`
  (`koala-diagnostics`); keep its test cases growing with what you've seen
  leak elsewhere. The virtual camera's source is given its user's SID,
  which `redact` takes out.
- Transactions are capture sessions with the iPhone only, named by their
  kind (`microphone`, `microphone+camera`); Koala's requests to Warlock
  aren't, since Warlock reports its own (Parker, 2026-11-21).
- `DO_NOT_TRACK` turns everything off; nothing may bypass it. The virtual
  camera's source, in Windows' camera service, can't see it: Koala passes
  the user's say in the camera's `DIAGNOSTICS_ALLOWED`, without which the
  source sends nothing.
- Only a release has the DSN built in. A trial that should report builds
  with `KOALA_DIAGNOSTICS=1`; nothing else turns it on.
- `nothing_identifying_leaves`, in `koala` and in `koala-vcam-source`,
  sends through the real SDK: run both, and `KOALA_SHOW_DIAGNOSTICS=1 cargo
  test -p koala nothing_identifying -- --nocapture` shows what would leave.

## Check the open issues with every commit

Before each commit, look through the open issues (`gh issue list`), this
repository's and Warlock's, for any the commit completes or advances, and
name them in its message (Parker, 2026-11-21):

- `Fixes #N` for one it completes: GitHub closes the issue when the commit
  reaches `master`. For one of Warlock's, `Fixes regulad/Warlock#N`.
- `#N` for one it only advances.
- Where it does what the issue asked another way, or leaves part of it, the
  message says how and why, so the issue's closing records it.

**Why:** Warlock's diagnostics did what Warlock's #1 asked, over several
commits, and none of them named it, so it stayed open with nothing tying it
to the work.
