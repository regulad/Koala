//! Copies the FFmpeg DLLs that Koala links (`FFMPEG_DIR`, set in
//! `.cargo/config.toml`, fetched by `tools/ffmpeg.ps1`) beside its programs
//! and tests, where Windows looks for them first.

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
    // OUT_DIR is target/<profile>/build/koala-<hash>/out: the profile's
    // directory holds the programs, and its deps and examples the rest.
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let Some(profile) = out.ancestors().nth(3) else { return };
    for dir in [profile.to_path_buf(), profile.join("deps"), profile.join("examples")] {
        let _ = fs::create_dir_all(&dir);
        for dll in &dlls {
            let to = dir.join(dll.file_name().expect("a file"));
            let same = fs::metadata(&to).and_then(|t| Ok(t.len() == fs::metadata(dll)?.len())).unwrap_or(false);
            if !same && let Err(e) = fs::copy(dll, &to) {
                println!("cargo:warning=copying {} to {}: {e}", dll.display(), to.display());
            }
        }
    }
}
