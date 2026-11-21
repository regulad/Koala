# Koala privacy policy

*Effective 20 November 2026. Publisher: Parker Wahle, regulad@regulad.xyz.*

Koala lets an iPhone be a microphone or camera for this computer, with Warlock carrying the connection between them.

## What the iPhone sends stays on this computer

The iPhone's sound and video go only from the iPhone to this computer, over Wi-Fi directly between the two. Koala doesn't send them anywhere else, and records them only when you ask it to, to a file you choose.

## Diagnostics

To find and fix problems, Koala's releases send crash reports and diagnostics (a build of Koala from its source code sends none) to Sentry (Functional Software, Inc., sentry.io), a service that stores them on our behalf, in the United States:

- **Errors and crash reports:** what went wrong and where in Koala's code.
- **Usage statistics:** when Koala starts and stops, and each connection to the iPhone: its microphones, and its camera if asked for, how long it lasted, and how it ended.
- **This computer:** Koala's and Warlock's versions, the Windows version, the processor, the amount of memory, and whether its Wi-Fi and Bluetooth adapters are present and on.
- **The iPhone:** its model and iOS version only (for example "iPhone18,1, 24B156").

Koala doesn't send names (yours, this computer's or the iPhone's), user or device IDs, network (MAC or IP) addresses, or anything else that identifies you or your devices, and our diagnostics service is set not to store IP addresses. It does note roughly where each report came from (the city, region and country), which it works out from the connection before discarding the address.

This is used only to diagnose and improve Koala. It isn't sold, shared for advertising, or combined with other data. Sentry keeps it for up to 90 days.

## Turning diagnostics off

Set the environment variable `DO_NOT_TRACK` to `1` (any value but empty, `0` or `false` will do), and Koala sends nothing.

## Children

Koala isn't directed at children and doesn't knowingly collect children's personal information.

## Contact

Questions: regulad@regulad.xyz.

## Changes

We'll update this page, and its effective date, if any of this changes.
