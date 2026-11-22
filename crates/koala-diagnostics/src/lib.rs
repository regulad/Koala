//! What Koala's diagnostics share, wherever they're sent from (`PRIVACY.md`;
//! PLAN.md, "Diagnostics"): Koala's programs (`koala`'s `diagnostics`) and
//! its virtual camera's media source (`koala-vcam-source`), which runs in
//! Windows' camera service, each to a Sentry project of its own, as
//! `warlock-diagnostics` is for Warlock's programs.
//!
//! - [`release`] and [`environment`]: one release per commit, the same for
//!   both, so a commit is one release in each project.
//! - [`dsn`]: a project's DSN in a release or a trial's build, none in any
//!   other.
//! - [`do_not_track`] and [`turned_off`].
//! - [`system`]: this computer, without anything that identifies it.
//! - [`options`]: the SDK's, with every event and breadcrumb passed through
//!   [`redact`] before it leaves, and modules cut to their file names.

use std::sync::{Arc, LazyLock};

use regex_lite::Regex;
use sentry::protocol::{Breadcrumb, Context, DebugImage, Event, Map, Stacktrace, Value};

/// Koala's version, which everything built from it carries.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit Koala was built from (`build.rs`), if known.
pub const COMMIT: Option<&str> = option_env!("KOALA_COMMIT");
/// Whether this is a tagged release build (`build.rs`).
pub const TAGGED: bool = option_env!("KOALA_TAGGED").is_some();
/// Whether this build was made with `KOALA_DIAGNOSTICS=1`, for a trial
/// (`build.rs`).
pub const TRIAL: bool = option_env!("KOALA_DIAGNOSTICS_TRIAL").is_some();

/// `koala@<version>`, with `+<commit>` for any build but a tagged one.
pub fn release() -> String {
    match COMMIT {
        Some(commit) if !TAGGED => format!("koala@{VERSION}+{commit}"),
        _ => format!("koala@{VERSION}"),
    }
}

/// `production` for a tagged release build, `development` otherwise.
pub fn environment() -> &'static str {
    if TAGGED { "production" } else { "development" }
}

/// `project`'s DSN in a release, or a build for a trial; none (`""`) in any
/// other, from source or a `testing` run, which so sends nothing (Parker,
/// 2026-11-21, as Warlock's #1 asked of Warlock). A DSN only lets a client
/// send events, so it isn't a secret.
pub const fn dsn(project: &'static str) -> &'static str {
    if TAGGED || TRIAL { project } else { "" }
}

/// Whether a `DO_NOT_TRACK` of `value` asks for no tracking: set to anything
/// but empty, `0` or `false`.
pub fn do_not_track(value: Option<&str>) -> bool {
    value.map(str::trim).is_some_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
}

/// Whether this process's environment asks for no tracking.
pub fn turned_off() -> bool {
    do_not_track(std::env::var("DO_NOT_TRACK").ok().as_deref())
}

/// The SDK's options for `program`, sending to `dsn`: the release and its
/// environment, the program's name for `server_name` (without one, the SDK
/// reports this computer's), no personal data, and every event and
/// breadcrumb scrubbed. No sessions or transactions: a program that wants
/// them turns them on.
pub fn options(program: &'static str, dsn: Option<sentry::types::Dsn>) -> sentry::ClientOptions {
    // Field by field: the struct is non-exhaustive.
    let mut options = sentry::ClientOptions::default();
    options.dsn = dsn;
    options.release = Some(release().into());
    options.environment = Some(environment().into());
    options.server_name = Some(program.into());
    options.send_default_pii = false;
    options.before_send = Some(Arc::new(|event| Some(scrub_event(event))));
    options.before_breadcrumb = Some(Arc::new(|breadcrumb| Some(scrub_breadcrumb(breadcrumb))));
    options.auto_session_tracking = false;
    options
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
/// addresses, Windows security identifiers (SIDs: the virtual camera's
/// source is given its user's), UUIDs and long hexadecimal strings
/// (identifiers, keys).
pub fn redact(text: &str) -> String {
    static USERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)([\\/]Users[\\/])[^\\/\s<>]+").unwrap());
    static SID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bS-1-\d+(?:-\d+)+").unwrap());
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
    out = SID.replace_all(&out, "<sid>").into_owned();
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
pub fn scrub_event(mut event: Event<'static>) -> Event<'static> {
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

/// A breadcrumb as it may leave: its message and data redacted.
pub fn scrub_breadcrumb(mut breadcrumb: Breadcrumb) -> Breadcrumb {
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
    fn only_a_release_or_a_trial_has_a_dsn() {
        assert_eq!(dsn("https://key@example.invalid/1").is_empty(), !TAGGED && !TRIAL);
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
            (
                r"Global\Koala.Camera.1.S-1-5-21-3623811015-3361044348-30300820-1013: access denied",
                r"Global\Koala.Camera.1.<sid>: access denied",
            ),
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
}
