//! Diagnostics: crash reports, errors and usage statistics, sent to Sentry,
//! with nothing that identifies the user, this computer or the iPhone
//! (`PRIVACY.md`; Parker, 2026-11-20, after RosellaView's). What they share
//! with the virtual camera's source, which reports to a project of its own,
//! is `koala-diagnostics`'s: the release, the DSN's gating, `DO_NOT_TRACK`,
//! this computer's facts and the scrubbing.
//!
//! - **Only in releases:** a tagged build has the DSN built in, as does one
//!   built with `KOALA_DIAGNOSTICS=1`, for a trial. Any other, from source
//!   or a `testing` run, has none and sends nothing (Parker, 2026-11-21, as
//!   Warlock's #1 asked of Warlock).
//! - **Off** when `DO_NOT_TRACK` is set (to anything but empty, `0` or
//!   `false`). The virtual camera's source doesn't see this environment, so
//!   Koala tells it ([`allowed`]).
//! - **Sent:**
//!   - panics, and the errors that end a run or a session;
//!   - a session per run of a program, for release health;
//!   - a transaction per capture session with the iPhone ([`Timed`]), and
//!     no others: Koala's requests to Warlock aren't transactions, since
//!     Warlock reports its own (Parker, 2026-11-21);
//!   - this computer's Windows version, processor, memory and architecture
//!     (`koala_diagnostics::system`), what Warlock reports of its radio and
//!     Bluetooth ([`warlock`]), and the iPhone's model and iOS build
//!     ([`iphone`]).
//! - **Never sent:** user or device IDs, names (the user's, this computer's,
//!   the iPhone's), network addresses, keys. `server_name`, which the SDK
//!   would set to this computer's name, is the program's. Every event and
//!   breadcrumb passes through `koala_diagnostics::redact` before it leaves,
//!   and modules keep only their file names. Sentry's organization doesn't
//!   store IP addresses.

use std::time::Instant;

use sentry::protocol::{Breadcrumb, Context, Level, Map, SpanStatus, Value};
use sentry::{SessionMode, TransactionContext};
use warlock_client::types::Info;

/// Koala's Sentry project (`regulad/koala`, made by the homelab's
/// `tofu/sentry`, 2026-11-21).
const PROJECT: &str = "https://cfbcf885ffbfc512dacd4b1c446dbac6@o4506961510596608.ingest.us.sentry.io/4512468117225473";

/// The DSN this build sends to: [`PROJECT`] in a release or a trial's build,
/// and none in any other.
pub const DSN: &str = koala_diagnostics::dsn(PROJECT);

/// Diagnostics for this run, until dropped, which sends what's left (for at
/// most 2 s) and ends the run's session.
#[must_use]
pub struct Diagnostics {
    _guard: Option<sentry::ClientInitGuard>,
}

/// Starts diagnostics for `program` (`koala`, `koala-tray`), if this build
/// has a DSN and `DO_NOT_TRACK` doesn't say not to.
pub fn start(program: &'static str) -> Diagnostics {
    if DSN.is_empty() {
        println!("{program}: this build isn't a release, so no diagnostics are sent");
        return Diagnostics { _guard: None };
    }
    if koala_diagnostics::turned_off() {
        println!("{program}: DO_NOT_TRACK is set, so no diagnostics are sent");
        return Diagnostics { _guard: None };
    }
    let guard = sentry::init(options(program, DSN.parse().ok()));
    sentry::configure_scope(|scope| scope.set_context("system", koala_diagnostics::system()));
    Diagnostics { _guard: Some(guard) }
}

/// Whether the user allows diagnostics, as the virtual camera's source is
/// told (`koala_vcam::DIAGNOSTICS_ALLOWED`): `DO_NOT_TRACK` isn't set. Each
/// side's build decides for itself whether it has a DSN.
pub fn allowed() -> bool {
    !koala_diagnostics::turned_off()
}

/// The SDK's options for `program`: the shared ones, with a session per run
/// and every capture session's transaction.
fn options(program: &'static str, dsn: Option<sentry::types::Dsn>) -> sentry::ClientOptions {
    let mut options = koala_diagnostics::options(program, dsn).traces_sample_rate(1.0);
    options.auto_session_tracking = true;
    options.session_mode = SessionMode::Application;
    options
}

/// One step of a run, as a breadcrumb: never anything identifying.
pub fn step(message: &str) {
    sentry::add_breadcrumb(Breadcrumb {
        category: Some("koala".into()),
        message: Some(message.into()),
        level: Level::Info,
        ..Default::default()
    });
}

/// An error that ended a run or a session.
pub fn error(message: &str) {
    sentry::capture_message(message, Level::Error);
}

/// A capture session with the iPhone, as a transaction: finished, with its
/// length, when dropped. Koala's only transactions.
pub struct Timed {
    transaction: sentry::Transaction,
    started: Instant,
    status: SpanStatus,
}

impl Timed {
    /// Starts `name` (`microphone`, `camera`) of `op` (`capture.session`).
    pub fn start(name: &str, op: &str) -> Self {
        Self {
            transaction: sentry::start_transaction(TransactionContext::new(name, op)),
            started: Instant::now(),
            status: SpanStatus::Ok,
        }
    }

    /// Records a non-identifying fact about it.
    pub fn set(&self, key: &str, value: impl Into<Value>) {
        self.transaction.set_data(key, value.into());
    }

    /// It ended badly: `reason`, a fixed word such as `no-connection`.
    pub fn fail(&mut self, reason: &str) {
        self.status = SpanStatus::InternalError;
        self.set("ended", reason);
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        self.transaction.set_data("seconds", ((self.started.elapsed().as_secs_f64() * 10.0).round() / 10.0).into());
        self.transaction.set_status(self.status);
        self.transaction.clone().finish();
    }
}

/// What Warlock reports of itself, for the events that follow: its version,
/// its radio's state, and this computer's Bluetooth.
pub fn warlock(info: &Info) {
    let mut map = Map::new();
    map.insert("version".into(), info.version.clone().into());
    map.insert("radio".into(), format!("{:?}", info.radio.state).to_lowercase().into());
    if let Some(width) = info.radio.width_mhz {
        map.insert("width_mhz".into(), width.into());
    }
    if let Some(b) = info.bluetooth {
        map.insert("bluetooth".into(), format!("{:?}", b.state).to_lowercase().into());
        map.insert("bluetooth_extended_advertising".into(), b.extended_advertising.into());
    }
    sentry::configure_scope(|scope| scope.set_context("warlock", Context::Other(map)));
}

/// The iPhone's model and iOS build (`iPhone18,1`, `24B156`): all Koala
/// sends of it.
pub fn iphone(model: Option<&str>, os_build: Option<&str>) {
    let mut map = Map::new();
    if let Some(model) = model {
        map.insert("model".into(), model.into());
    }
    if let Some(build) = os_build {
        map.insert("os_build".into(), build.into());
    }
    sentry::configure_scope(|scope| {
        if let Some(model) = model {
            scope.set_tag("iphone.model", model);
        }
        scope.set_context("iphone", Context::Other(map));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_release_or_a_trial_has_the_dsn() {
        assert_eq!(DSN.is_empty(), !koala_diagnostics::TAGGED && !koala_diagnostics::TRIAL);
    }

    /// What would leave, end to end, through the SDK with these options and
    /// the integrations `sentry::init` adds (contexts, debug images with
    /// their modules' paths; not the panic hook, which would take the other
    /// tests' panics): an error and a capture session's transaction carry
    /// neither this computer's name nor the user's, nor an address, nor a
    /// user.
    #[test]
    fn nothing_identifying_leaves() {
        let mut options = sentry::apply_defaults(options("koala-test", None));
        options.integrations.retain(|integration| integration.name() != "panic");
        let envelopes = sentry::test::with_captured_envelopes_options(
            || {
                sentry::configure_scope(|scope| scope.set_context("system", koala_diagnostics::system()));
                step("paired with c6:45:45:ba:cd:d3");
                error(&format!(
                    r"opening C:\Users\{}\x.mkv on {} from fe80::1%49",
                    std::env::var("USERNAME").unwrap_or_default(),
                    std::env::var("COMPUTERNAME").unwrap_or_default()
                ));
                let mut session = Timed::start("microphone", "capture.session");
                session.set("streams_up", 1);
                session.fail("pair-verify");
                drop(session);
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
        // `KOALA_SHOW_DIAGNOSTICS=1 cargo test -p koala nothing_identifying -- --nocapture`
        // shows what would leave.
        if std::env::var_os("KOALA_SHOW_DIAGNOSTICS").is_some() {
            eprintln!("{text}");
        }
        assert!(text.contains("\"server_name\":\"koala-test\""), "the program's name, not the computer's: {text}");
        for var in ["COMPUTERNAME", "USERNAME"] {
            if let Ok(name) = std::env::var(var)
                && name.len() >= 2
            {
                assert!(!text.contains(&name.to_lowercase()), "{var} leaked: {text}");
            }
        }
        assert!(!text.contains("c6:45:45:ba:cd:d3") && !text.contains("fe80::1"), "an address leaked: {text}");
        assert!(!text.contains("\"user\""), "a user: {text}");
        assert!(text.contains("<addr>") && text.contains("<user>"), "redacted: {text}");
    }
}
