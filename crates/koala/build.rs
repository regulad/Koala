//! Puts the FFmpeg DLLs that Koala links (`FFMPEG_DIR`, set in
//! `.cargo/config.toml`, fetched by `tools/ffmpeg.ps1`) where Windows finds
//! them for every program and test, whichever build-directory layout Cargo
//! uses (Cargo 1.100 moved tests out of `deps`):
//! - in this script's `OUT_DIR`, declared as a native library search path,
//!   which Cargo puts on `PATH` for whatever it runs (`cargo test`,
//!   `cargo run`): its directories inside the target directory;
//! - beside the programs in `target/<profile>` and its `examples`, for
//!   running them directly, and for packaging.
//!
//! (What the diagnostics name the release after is `koala-diagnostics`'s
//! build script's.)

use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let Some(ffmpeg) = env::var_os("FFMPEG_DIR") else { return };
    let bin = Path::new(&ffmpeg).join("bin");
    println!("cargo:rerun-if-changed={}", bin.display());
    let Ok(entries) = fs::read_dir(&bin) else {
        println!("cargo:warning=no FFmpeg DLLs in {}: run tools/ffmpeg.ps1", bin.display());
        return;
    };
    let dlls: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "dll")).collect();
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    copy(&dlls, &out);
    println!("cargo:rustc-link-search=native={}", out.display());
    // OUT_DIR is under target/<profile>/build (target/<profile>/build/
    // koala-<hash>/out before Cargo 1.100, target/<profile>/build/koala/
    // <hash>/out since): the profile's directory holds the programs.
    let Some(profile) = out.ancestors().find(|a| a.file_name().is_some_and(|n| n == "build")).and_then(Path::parent)
    else {
        println!("cargo:warning=no target/<profile> above {}: the programs won't find FFmpeg", out.display());
        return;
    };
    copy(&dlls, profile);
    copy(&dlls, &profile.join("examples"));
}

/// Copies `dlls` into `dir`, skipping those already there at the same size.
fn copy(dlls: &[PathBuf], dir: &Path) {
    let _ = fs::create_dir_all(dir);
    for dll in dlls {
        let to = dir.join(dll.file_name().expect("a file"));
        let same = fs::metadata(&to).and_then(|t| Ok(t.len() == fs::metadata(dll)?.len())).unwrap_or(false);
        if !same && let Err(e) = fs::copy(dll, &to) {
            println!("cargo:warning=copying {} to {}: {e}", dll.display(), to.display());
        }
    }
}
