//! The source's diagnostics (`PRIVACY.md`, `koala-diagnostics`): its
//! panics, and the errors that fail its camera or black it out, to Sentry's
//! `regulad/koala-vcam` (made by the homelab's `tofu/sentry`, 2026-11-21).
//! Nothing else: no sessions, so no release health, and no transactions.
//!
//! - **Only in a release's build**, or a trial's (`KOALA_DIAGNOSTICS=1`), as
//!   Koala's.
//! - **Only when the camera's user allows them.** The camera service doesn't
//!   see the user's `DO_NOT_TRACK`, so Koala says, in the camera's
//!   [`koala_vcam::DIAGNOSTICS_ALLOWED`]. They start with the first source
//!   made for a camera that allows them. Once a camera doesn't, this process
//!   sends nothing more until it ends: the camera service serves every
//!   user's cameras, and the SDK's client is the process's.
//! - **Started there, not in `DllMain`,** which runs under the loader lock.
//!   The DLL is then pinned ([`crate::pin`]): the SDK's threads run its code
//!   until the process ends, so it must never be unloaded under them.
//! - **A panic aborts the camera service** (it can't unwind through COM),
//!   but the SDK's panic hook sends it first.
//! - The SID the source is given is redacted with the rest
//!   (`koala_diagnostics::redact`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Once};

use sentry::protocol::{Context, Level};

/// The source's Sentry project.
const PROJECT: &str = "https://1ea16e97f9cb6a93579e709213833ef4@o4506961510596608.ingest.us.sentry.io/4512472940544000";
/// The DSN this build sends to: [`PROJECT`] in a release or a trial's
/// build, and none in any other.
const DSN: &str = koala_diagnostics::dsn(PROJECT);
/// What it reports as, in `server_name`.
const PROGRAM: &str = "koala-vcam-source";

static STARTED: Once = Once::new();
/// Set once a camera's user doesn't allow diagnostics.
static REFUSED: AtomicBool = AtomicBool::new(false);
static SYSTEM: LazyLock<Context> = LazyLock::new(koala_diagnostics::system);

/// What a camera allows, as a source is made for it: starts the
/// diagnostics, or stops them for good.
pub fn camera(allowed: bool) {
    if !allowed {
        if !REFUSED.swap(true, Ordering::SeqCst) {
            crate::log("diagnostics: a camera's user doesn't allow them, so none are sent");
        }
        return;
    }
    if DSN.is_empty() || REFUSED.load(Ordering::SeqCst) {
        return;
    }
    STARTED.call_once(|| {
        crate::pin();
        let guard = sentry::init(options(DSN.parse().ok()));
        // On the process's hub, whichever thread this is: every other
        // thread's hub takes its client from there.
        if let Some(client) = sentry::Hub::current().client() {
            sentry::Hub::main().bind_client(Some(client));
        }
        // Kept for the process's life: dropping it would close the client.
        std::mem::forget(guard);
        crate::log("diagnostics: on");
    });
}

/// The SDK's options: the shared ones, with this computer's facts on every
/// event, and nothing sent once a camera has refused.
fn options(dsn: Option<sentry::types::Dsn>) -> sentry::ClientOptions {
    let mut options = koala_diagnostics::options(PROGRAM, dsn);
    let scrub = options.before_send.take();
    options.before_send = Some(Arc::new(move |mut event| {
        if REFUSED.load(Ordering::SeqCst) {
            return None;
        }
        event.contexts.insert("system".into(), SYSTEM.clone());
        match &scrub {
            Some(scrub) => scrub(event),
            None => Some(event),
        }
    }));
    options
}

/// An error that fails the source or blacks out its camera: `what` failed,
/// for `why`, which is redacted before it leaves.
pub fn error(what: &str, why: &dyn std::fmt::Display) {
    if STARTED.is_completed() {
        sentry::capture_message(&format!("{what}: {why}"), Level::Error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What would leave, end to end, through the SDK with these options
    /// (without the panic hook, which would take the other tests' panics):
    /// an error naming the shared memory carries neither the user's SID nor
    /// a user, and names the source, not this computer.
    #[test]
    fn nothing_identifying_leaves() {
        let mut options = sentry::apply_defaults(options(None));
        options.integrations.retain(|integration| integration.name() != "panic");
        let envelopes = sentry::test::with_captured_envelopes_options(
            || {
                sentry::capture_message(
                    r"the shared memory: Global\Koala.Camera.1.S-1-5-21-3623811015-3361044348-30300820-1013: Access is denied.",
                    Level::Error,
                );
            },
            options,
        );
        let text: String = envelopes
            .iter()
            .map(|e| {
                let mut bytes = Vec::new();
                e.to_writer(&mut bytes).unwrap();
                String::from_utf8_lossy(&bytes).to_lowercase()
            })
            .collect();
        assert!(text.contains("\"server_name\":\"koala-vcam-source\""), "the source's name: {text}");
        assert!(!text.contains("s-1-5-21") && text.contains("<sid>"), "the SID: {text}");
        assert!(text.contains("\"system\""), "this computer's facts: {text}");
        assert!(!text.contains("\"user\""), "a user: {text}");
        if let Ok(name) = std::env::var("COMPUTERNAME")
            && name.len() >= 2
        {
            assert!(!text.contains(&name.to_lowercase()), "this computer's name: {text}");
        }
    }
}
