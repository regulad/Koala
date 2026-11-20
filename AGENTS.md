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
