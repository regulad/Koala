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

## Keep the diagnostics anonymous, and `PRIVACY.md` true

What Koala sends to Sentry (`crates/koala/src/diagnostics.rs`) never
identifies the user, this computer or the iPhone: no names (the user's, the
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
- Anything that does go out passes through `redact`; keep its test cases
  growing with what you've seen leak elsewhere.
- Transactions are capture sessions with the iPhone only, named by their
  kind (`microphone`, `camera`); Koala's requests to Warlock aren't, since
  Warlock reports its own (Parker, 2026-11-21).
- `DO_NOT_TRACK` turns everything off; nothing may bypass it.
- `nothing_identifying_leaves` sends through the real SDK: run it, and
  `KOALA_SHOW_DIAGNOSTICS=1 cargo test -p koala nothing_identifying --
  --nocapture` shows what would leave.
