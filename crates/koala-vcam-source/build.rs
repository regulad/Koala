//! The DLL's link, for where it runs: inside Windows' camera service, which
//! loads only what Windows has (Microsoft's VirtualCamera sample, "Updates"
//! and FAQ 1: a missing dependency fails the camera with MOD_NOT_FOUND).
//!
//! - **The C runtime, hybrid:** Visual C++'s part (`vcruntime140.dll`,
//!   which Windows doesn't ship) linked in, the Universal CRT (which it
//!   does) from Windows. Rust's default for MSVC is both from DLLs, and
//!   `crt-static` both linked in for every crate of the workspace, `koala`'s
//!   C dependencies included; this changes the DLL's alone: the static
//!   startup and `vcruntime` libraries for the DLL ones, and `ucrt.lib`.
//! - **`LNK4104`**, that `DllGetClassObject` and the rest should be
//!   `PRIVATE` (left out of the import library), is silenced: nothing links
//!   against this DLL's import library.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    for arg in [
        "/NODEFAULTLIB:msvcrt.lib",
        "/NODEFAULTLIB:vcruntime.lib",
        // libcmt's own default: the Universal CRT linked in.
        "/NODEFAULTLIB:libucrt.lib",
        "libcmt.lib",
        "libvcruntime.lib",
        "ucrt.lib",
        "/IGNORE:4104",
    ] {
        println!("cargo:rustc-cdylib-link-arg={arg}");
    }
}
