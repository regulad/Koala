//! Diagnostics: crash reports, errors and usage statistics, sent to Sentry,
//! with nothing that identifies the user, this computer or the iPhone
//! (`PRIVACY.md`; Parker, 2026-11-20, after RosellaView's).
//!
//! - **Off** when `DO_NOT_TRACK` is set (to anything but empty, `0` or
//!   `false`), and while [`DSN`] is empty.
//! - **Sent:**
//!   - panics, and the errors that end a run or a session;
//!   - a session per run of a program, for release health;
//!   - a transaction per request to Warlock ([`request`]), except the
//!     tray's polling of `GET /v1/info` while nothing runs, a health probe
//!     every 2 s (as PepperCord leaves its health probes untraced), and one
//!     per capture session ([`Timed`]);
//!   - this computer's Windows version, processor, memory and architecture
//!     ([`system`]), what Warlock reports of its radio and Bluetooth
//!     ([`warlock`]), and the iPhone's model and iOS build ([`iphone`]).
//! - **Never sent:** user or device IDs, names (the user's, this computer's,
//!   the iPhone's), network addresses, keys. `server_name`, which the SDK
//!   would set to this computer's name, is the program's. Every event and
//!   breadcrumb passes through [`redact`] before it leaves, and modules keep
//!   only their file names. Sentry's organization doesn't store IP addresses.

use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use regex_lite::Regex;
use sentry::protocol::{Breadcrumb, Context, DebugImage, Event, Level, Map, SpanStatus, Stacktrace, Value};
use sentry::{SessionMode, TransactionContext};
use warlock_client::ClientError;
use warlock_client::types::Info;

/// Koala's Sentry project (`regulad/koala`). A DSN only lets a client send
/// events, so it isn't a secret (as RosellaView and PepperCord hold). Empty
/// until the project exists: nothing is sent.
pub const DSN: &str = "";

/// The commit Koala was built from (`build.rs`), if known.
const COMMIT: Option<&str> = option_env!("KOALA_COMMIT");
/// Whether this is a tagged release build (`build.rs`).
const TAGGED: bool = option_env!("KOALA_TAGGED").is_some();

/// `koala@<version>`, with `+<commit>` for any build but a tagged one.
pub fn release() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match COMMIT {
        Some(commit) if !TAGGED => format!("koala@{version}+{commit}"),
        _ => format!("koala@{version}"),
    }
}

/// `production` for a tagged release build, `development` otherwise.
pub fn environment() -> &'static str {
    if TAGGED { "production" } else { "development" }
}

/// Whether a `DO_NOT_TRACK` of `value` asks for no tracking: set to anything
/// but empty, `0` or `false`.
pub fn do_not_track(value: Option<&str>) -> bool {
    value.map(str::trim).is_some_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
}

/// Diagnostics for this run, until dropped, which sends what's left (for at
/// most 2 s) and ends the run's session.
#[must_use]
pub struct Diagnostics {
    _guard: Option<sentry::ClientInitGuard>,
}

/// Starts diagnostics for `program` (`koala`, `koala-tray`), unless
/// `DO_NOT_TRACK` says not to or there's no DSN.
pub fn start(program: &'static str) -> Diagnostics {
    if do_not_track(std::env::var("DO_NOT_TRACK").ok().as_deref()) {
        println!("{program}: DO_NOT_TRACK is set, so no diagnostics are sent");
        return Diagnostics { _guard: None };
    }
    if DSN.is_empty() {
        return Diagnostics { _guard: None };
    }
    let guard = sentry::init(options(program, DSN.parse().ok()));
    sentry::configure_scope(|scope| scope.set_context("system", system()));
    Diagnostics { _guard: Some(guard) }
}

/// The SDK's options for `program`.
fn options(program: &'static str, dsn: Option<sentry::types::Dsn>) -> sentry::ClientOptions {
    // Field by field: the struct is non-exhaustive.
    let mut options = sentry::ClientOptions::default().traces_sample_rate(1.0);
    options.dsn = dsn;
    options.release = Some(release().into());
    options.environment = Some(environment().into());
    // The program's name: without one, the SDK reports this computer's.
    options.server_name = Some(program.into());
    options.send_default_pii = false;
    options.before_send = Some(Arc::new(|event| Some(scrub_event(event))));
    options.before_breadcrumb = Some(Arc::new(|breadcrumb| Some(scrub_breadcrumb(breadcrumb))));
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

/// A request to Warlock, `method path` (the path as the contract names it,
/// `/v1/peers/{address}`, so no address), as a transaction of its own: how
/// long it took and how it went.
pub async fn request<T>(
    method: &str,
    path: &str,
    call: impl Future<Output = Result<T, ClientError>>,
) -> Result<T, ClientError> {
    let transaction = sentry::start_transaction(TransactionContext::new(&format!("{method} {path}"), "http.client"));
    transaction.set_data("http.request.method", method.into());
    transaction.set_data("server.address", "warlock".into());
    let result = call.await;
    let (status, code) = match &result {
        Ok(_) => (SpanStatus::Ok, Some(200)),
        Err(ClientError::Api { status, .. }) => (span_status(status.as_u16()), Some(status.as_u16())),
        Err(ClientError::Connect { .. }) => (SpanStatus::Unavailable, None),
        Err(_) => (SpanStatus::UnknownError, None),
    };
    if let Some(code) = code {
        transaction.set_data("http.response.status_code", code.into());
    }
    transaction.set_status(status);
    transaction.finish();
    result
}

fn span_status(code: u16) -> SpanStatus {
    match code {
        200..=299 => SpanStatus::Ok,
        400 => SpanStatus::InvalidArgument,
        403 => SpanStatus::PermissionDenied,
        404 => SpanStatus::NotFound,
        409 => SpanStatus::AlreadyExists,
        500..=599 => SpanStatus::InternalError,
        _ => SpanStatus::UnknownError,
    }
}

/// A timed piece of a run, such as a capture session, as a transaction:
/// finished, with its length, when dropped.
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

/// This computer, without anything that identifies it: Windows' version,
/// the processor, its threads, the memory (in GiB) and the architecture.
pub fn system() -> Context {
    let mut map = Map::new();
    const VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    if let Some(v) = win::registry_string(VERSION, "DisplayVersion") {
        map.insert("windows_version".into(), v.into());
    }
    if let Some(build) = win::registry_string(VERSION, "CurrentBuild") {
        let ubr = win::registry_dword(VERSION, "UBR").map_or(String::new(), |u| format!(".{u}"));
        map.insert("windows_build".into(), format!("{build}{ubr}").into());
    }
    if let Some(cpu) = win::registry_string(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString") {
        map.insert("cpu".into(), cpu.trim().into());
    }
    if let Ok(threads) = std::thread::available_parallelism() {
        map.insert("cpu_threads".into(), threads.get().into());
    }
    if let Some(bytes) = win::memory() {
        map.insert("memory_gib".into(), (bytes as f64 / (1u64 << 30) as f64).round().into());
    }
    map.insert("arch".into(), std::env::consts::ARCH.into());
    Context::Other(map)
}

/// `text` without what could identify anyone: this computer's and the
/// user's names, the user's folder in paths, network (IP and MAC)
/// addresses, UUIDs and long hexadecimal strings (identifiers, keys).
pub fn redact(text: &str) -> String {
    static USERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)([\\/]Users[\\/])[^\\/\s<>]+").unwrap());
    static UUID: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b").unwrap());
    static MAC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b[0-9a-f]{2}(?:[:-][0-9a-f]{2}){5}\b").unwrap());
    static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b[0-9a-f]{12,}\b").unwrap());
    // Candidates for IP addresses, with a zone or port: checked by parsing.
    static IP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9A-Za-z:.%]*[:.][0-9A-Za-z:.%]*").unwrap());
    static NAMES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [("COMPUTERNAME", "<host>"), ("USERNAME", "<user>")]
            .into_iter()
            .filter_map(|(var, with)| {
                let name = std::env::var(var).ok().filter(|n| n.trim().len() >= 2)?;
                Some((Regex::new(&format!("(?i){}", regex_lite::escape(name.trim()))).ok()?, with))
            })
            .collect()
    });
    let mut out = text.to_owned();
    for (name, with) in NAMES.iter() {
        out = name.replace_all(&out, *with).into_owned();
    }
    out = USERS.replace_all(&out, "${1}<user>").into_owned();
    out = IP
        .replace_all(&out, |m: &regex_lite::Captures| {
            let token = &m[0];
            if is_ip(token) { "<addr>".to_owned() } else { token.to_owned() }
        })
        .into_owned();
    out = UUID.replace_all(&out, "<id>").into_owned();
    out = MAC.replace_all(&out, "<addr>").into_owned();
    HEX.replace_all(&out, "<hex>").into_owned()
}

/// Whether `token` is an IP address, maybe with a zone (`%49`) or a port.
fn is_ip(token: &str) -> bool {
    let token = token.trim_matches(|c| c == '.' || c == ':');
    let address = token.split('%').next().unwrap_or(token);
    address.parse::<std::net::Ipv6Addr>().is_ok()
        || address.parse::<std::net::Ipv4Addr>().is_ok()
        || address.parse::<std::net::SocketAddrV4>().is_ok()
}

fn redact_value(value: &mut Value) {
    match value {
        Value::String(s) => *s = redact(s),
        Value::Array(items) => items.iter_mut().for_each(redact_value),
        Value::Object(map) => map.values_mut().for_each(redact_value),
        _ => {}
    }
}

fn redact_stack(stack: &mut Option<Stacktrace>) {
    for frame in stack.iter_mut().flat_map(|s| s.frames.iter_mut()) {
        for path in [&mut frame.abs_path, &mut frame.filename].into_iter().flatten() {
            *path = redact(path);
        }
    }
}

/// A path's last component: modules keep only their file names.
fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_owned()
}

/// An event as it may leave: no user or request, its texts redacted, its
/// modules' paths cut to their file names.
fn scrub_event(mut event: Event<'static>) -> Event<'static> {
    event.user = None;
    event.request = None;
    for text in [&mut event.message, &mut event.culprit, &mut event.transaction].into_iter().flatten() {
        *text = redact(text);
    }
    if let Some(entry) = &mut event.logentry {
        entry.message = redact(&entry.message);
        entry.params.iter_mut().for_each(redact_value);
    }
    for exception in &mut event.exception.values {
        if let Some(value) = &mut exception.value {
            *value = redact(value);
        }
        redact_stack(&mut exception.stacktrace);
        redact_stack(&mut exception.raw_stacktrace);
    }
    for thread in &mut event.threads.values {
        redact_stack(&mut thread.stacktrace);
        redact_stack(&mut thread.raw_stacktrace);
    }
    redact_stack(&mut event.stacktrace);
    for breadcrumb in &mut event.breadcrumbs.values {
        *breadcrumb = scrub_breadcrumb(std::mem::take(breadcrumb));
    }
    for tag in event.tags.values_mut() {
        *tag = redact(tag);
    }
    event.extra.values_mut().for_each(redact_value);
    for image in &mut event.debug_meta.to_mut().images {
        if let DebugImage::Symbolic(image) = image {
            image.name = file_name(&image.name);
            image.debug_file = image.debug_file.as_deref().map(file_name);
        }
    }
    event
}

fn scrub_breadcrumb(mut breadcrumb: Breadcrumb) -> Breadcrumb {
    if let Some(message) = &mut breadcrumb.message {
        *message = redact(message);
    }
    breadcrumb.data.values_mut().for_each(redact_value);
    breadcrumb
}

/// What [`system`] reads from Windows.
mod win {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// A string value under `HKEY_LOCAL_MACHINE\key`.
    pub fn registry_string(key: &str, name: &str) -> Option<String> {
        let (key, name) = (wide(key), wide(name));
        let mut data = vec![0u16; 256];
        let mut size = (data.len() * 2) as u32;
        // SAFETY: NUL-terminated names, and a buffer of the size we pass.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 {
            return None;
        }
        let len = data.iter().position(|&c| c == 0).unwrap_or(data.len());
        Some(String::from_utf16_lossy(&data[..len]))
    }

    /// A DWORD value under `HKEY_LOCAL_MACHINE\key`.
    pub fn registry_dword(key: &str, name: &str) -> Option<u32> {
        let (key, name) = (wide(key), wide(name));
        let mut value = 0u32;
        let mut size = 4u32;
        // SAFETY: as above, with a DWORD.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut size,
            )
        };
        (status == 0).then_some(value)
    }

    /// The physical memory, in bytes.
    pub fn memory() -> Option<u64> {
        // SAFETY: a plain C struct, its size set as the call requires.
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullTotalPhys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn do_not_track_is_anything_but_empty_zero_or_false() {
        for on in ["1", "true", "yes", " 1 "] {
            assert!(do_not_track(Some(on)), "{on:?}");
        }
        for off in [None, Some(""), Some("0"), Some("false"), Some("FALSE"), Some("  ")] {
            assert!(!do_not_track(off), "{off:?}");
        }
    }

    #[test]
    fn addresses_ids_and_keys_are_redacted() {
        let cases = [
            ("connection from [fe80::c445:45ff:feba:cdd3%49]:49277", "connection from [<addr>]:49277"),
            ("Bad peer address string 'fe80::1%awdl0%llw0'", "Bad peer address string '<addr>'"),
            ("from 192.168.1.20:5353 now", "from <addr> now"),
            ("peer c6:45:45:ba:cd:d3 gone", "peer <addr> gone"),
            ("PAIRED with 207E09B7-FEDF-4161-829C-2B1149A21A52", "PAIRED with <id>"),
            ("instance RDLink-03ad02a1fc20._rdlink._tcp.local", "instance RDLink-<hex>._rdlink._tcp.local"),
            ("irk 5be0c2a91f7d43e8a06b1c2d00e4f9a1", "irk <hex>"),
            (r"C:\Users\someone\AppData\Local\Koala\x.log", r"C:\Users\<user>\AppData\Local\Koala\x.log"),
        ];
        for (text, expected) in cases {
            assert_eq!(redact(text), expected, "{text}");
        }
        // Left alone: versions, times, Windows builds, ports, plain words.
        for kept in ["Warlock 0.2.4", "at 12:34", "build 10.0.26300.7019", "port 49278", "iPhone18,1 24B156"] {
            assert_eq!(redact(kept), kept);
        }
    }

    #[test]
    fn this_computers_and_users_names_are_redacted() {
        let (Ok(host), Ok(user)) = (std::env::var("COMPUTERNAME"), std::env::var("USERNAME")) else { return };
        let text = format!("{host}.local and {} in a path", user.to_lowercase());
        let out = redact(&text);
        assert!(!out.to_lowercase().contains(&host.to_lowercase()), "{out}");
        assert!(!out.to_lowercase().contains(&user.to_lowercase()), "{out}");
    }

    #[test]
    fn modules_keep_only_their_file_names() {
        assert_eq!(file_name(r"C:\Users\someone\Koala\koala.exe"), "koala.exe");
        assert_eq!(file_name("koala.pdb"), "koala.pdb");
    }

    /// What would leave, end to end, through the SDK with these options and
    /// the integrations `sentry::init` adds (contexts, debug images with
    /// their modules' paths; not the panic hook, which would take the other
    /// tests' panics): an error and a request's transaction carry neither
    /// this computer's name nor the user's, nor an address, nor a user.
    #[test]
    fn nothing_identifying_leaves() {
        let mut options = sentry::apply_defaults(options("koala-test", None));
        options.integrations.retain(|integration| integration.name() != "panic");
        let envelopes = sentry::test::with_captured_envelopes_options(
            || {
                sentry::configure_scope(|scope| scope.set_context("system", system()));
                step("paired with c6:45:45:ba:cd:d3");
                error(&format!(
                    r"opening C:\Users\{}\x.mkv on {} from fe80::1%49",
                    std::env::var("USERNAME").unwrap_or_default(),
                    std::env::var("COMPUTERNAME").unwrap_or_default()
                ));
                let transaction =
                    sentry::start_transaction(TransactionContext::new("GET /v1/peers/{address}", "http.client"));
                transaction.finish();
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
