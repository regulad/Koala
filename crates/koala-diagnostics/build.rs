//! Tells the diagnostics (`src/lib.rs`) what they name the release after:
//! the commit (`KOALA_COMMIT`), and whether this is a tagged build
//! (`KOALA_TAGGED`, from GitHub's runner); and whether to build them in for
//! a trial (`KOALA_DIAGNOSTICS=1` when building).

use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_REF_TYPE");
    if env::var("GITHUB_REF_TYPE").as_deref() == Ok("tag") {
        println!("cargo:rustc-env=KOALA_TAGGED=1");
    }
    println!("cargo:rerun-if-env-changed=KOALA_DIAGNOSTICS");
    if env::var("KOALA_DIAGNOSTICS").as_deref() == Ok("1") {
        println!("cargo:rustc-env=KOALA_DIAGNOSTICS_TRIAL=1");
    }
    let Some(manifest) = env::var_os("CARGO_MANIFEST_DIR") else { return };
    let root = Path::new(&manifest).join("..").join("..");
    let commit = Command::new("git").args(["rev-parse", "--short=12", "HEAD"]).current_dir(&root).output();
    if let Ok(out) = commit
        && out.status.success()
    {
        println!("cargo:rustc-env=KOALA_COMMIT={}", String::from_utf8_lossy(&out.stdout).trim());
    }
    // A commit or a checkout changes these.
    for file in ["HEAD", "logs/HEAD"] {
        let path = root.join(".git").join(file);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
