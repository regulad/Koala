//! koala-msi: builds Koala's Windows Installer package, for winget as
//! `Regulad.Koala`, as Warlock's is built: with Windows' own tools only, the
//! Windows Installer API (`msi.dll`) writing the database and `makecab`
//! packing the files, through Warlock's `warlock-msi-kit` (Parker,
//! 2026-11-21: shared, not copied).
//!
//! ```text
//! cargo run -p koala-msi [-- --out PATH] [--no-build]
//! cargo run -p koala-msi -- winget MSI --installer-url URL [--out DIR]
//! ```
//!
//! Builds `koala`, `koala-tray` and the virtual camera's media source
//! (`koala-vcam-source`) in release (unless `--no-build`), and writes
//! `target/msi/Koala-<version>-x64.msi`, which installs, for every user of
//! the computer, into `C:\Program Files\Koala`:
//! - `koala-tray.exe`, the app, with a Start menu shortcut, and `koala.exe`,
//!   the console tool;
//! - FFmpeg's DLLs, which both link, and FFmpeg's licence;
//! - `koala_vcam_source.dll`, registered as the camera's COM class
//!   ([`koala_vcam::CLSID`]) under `HKEY_LOCAL_MACHINE`, the same keys its
//!   own `DllRegisterServer` writes. Windows' camera service, which loads it
//!   as LocalService, is stopped as it's installed or removed, so that it
//!   lets go of an old copy; Windows starts it again when an app next wants
//!   a camera (`tools/vcam.ps1` does the same for development);
//! - Koala's licence, its privacy policy and the third-party notices.
//!
//! It installs only where Koala can run, and says what's missing otherwise
//! (Parker, 2026-11-21: "reject the install if they're not detected, but
//! don't attempt to install them ourselves"):
//! - **Warlock** ([`WARLOCK_MIN`] or later), which carries the connection to
//!   the iPhone, found by its package's upgrade code;
//! - **usbip-win2** ([`USBIP_MIN`] or later), which gives Windows the
//!   iPhone's microphones, found as its client is where Koala runs it
//!   (`koala::usb_mics`);
//! - the Visual C++ runtime the programs link to, at least as new as the
//!   toolset that linked them, as Warlock's package checks.
//!
//! winget installs them first: the manifests [`winget`] writes depend on all
//! three (Parker, 2026-11-21), and on the Windows App Runtime the tray's
//! WinUI needs, which the package doesn't look for. A newer version replaces an older one, and a
//! rebuild of the same version replaces itself; an older one won't install
//! over a newer one.
//!
//! Started by hand, it shows its own pages ([`ui`]); silent installs,
//! winget's among them, show none.

mod licence;
mod ui;

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use warlock_msi_kit::winget::{self, Dependency, Manifest};
use warlock_msi_kit::{
    COMPONENT_64BIT, Database, HKLM, Payload, Result, SERVICE_STOP_INSTALL, SERVICE_STOP_UNINSTALL, Search, Summary,
    Table, UPGRADE_MAX_INCLUSIVE, UPGRADE_MIGRATE_FEATURES, UPGRADE_MIN_INCLUSIVE, UPGRADE_ONLY_DETECT, VcRuntime,
    file_table, media_table, n, new_guid, package_properties, product_version, runtime_min, s, schema, sequence,
    with_sizes,
};

// GUIDs that never change. Generated once with PowerShell's
// `[guid]::NewGuid()` on 2026-11-21, while writing this builder.
// - The upgrade code ties every version of Koala together: Windows
//   Installer finds an installed Koala to replace by it.
// - A component's GUID is how Windows Installer counts that component
//   across versions and products, so each stays with its key file for good.
//   A new file that isn't one of these goes in a new component with a new
//   GUID, generated the same way and noted here; FFmpeg's DLLs have theirs
//   from their names ([`component_guid`]).
// The product code and package code are new for every build (`new_guid`).
const UPGRADE_CODE: &str = "{5100E6EC-88FC-4E17-8542-80D55BF579C2}";
/// Key file `koala.exe`.
const COMPONENT_KOALA: &str = "{C3567E34-C44C-4FC5-A724-A3FE9E1F12E8}";
/// Key file `koala-tray.exe`; the Start menu shortcut hangs off it.
const COMPONENT_TRAY: &str = "{5FB78CD7-AE40-4739-8753-FDD129EAA8A9}";
/// Key file `koala_vcam_source.dll`; its COM registration and the camera
/// service's stopping hang off it.
const COMPONENT_CAMERA: &str = "{A1C298B4-6658-4E4A-A2EE-5C56CA9ECFA2}";
/// Key file `LICENSE.md`; with FFmpeg's licence.
const COMPONENT_LICENSES: &str = "{B4888DF3-5524-45D6-A509-F72A0DD8B3E4}";
/// Key file `PRIVACY.md`, the privacy policy.
const COMPONENT_DIAGNOSTICS: &str = "{3B24E613-89A2-4E8B-A0F2-73EBD1621817}";
/// Key file `THIRD-PARTY-NOTICES.txt`.
const COMPONENT_NOTICES: &str = "{2BBC61E2-9299-4690-8E57-F9216DCD9AB9}";

/// Warlock's package's upgrade code (Warlock's `warlock-msi`), which finds an
/// installed Warlock of any version.
const WARLOCK_UPGRADE_CODE: &str = "{13F9A3A8-EA0D-4025-8C66-70946C436AE5}";
/// The least Warlock Koala takes: the one it's built against (the
/// submodule's), whose API it uses.
const WARLOCK_MIN: &str = "0.2.5";
/// Set by FindRelatedProducts when Warlock is installed at [`WARLOCK_MIN`] or
/// later.
const WARLOCK_FOUND: &str = "WARLOCKFOUND";
/// The least usbip-win2: 0.9.7.8, winget's copy, bugchecks Windows when an
/// app closes an emulated device's stream (PLAN.md, "usbip-win2's minimum").
const USBIP_MIN: &str = "0.9.8.0";
/// [`USBIP_MIN`] for the file search, which a file at exactly its floor
/// passes only if its language matches (`Search::file`): one below it.
const USBIP_FLOOR: &str = "0.9.7.65535";
/// Set by AppSearch when usbip-win2's client is there at [`USBIP_MIN`] or
/// later: `%ProgramFiles%\USBip\usbip.exe`, where its installer puts it and
/// Koala runs it.
const USBIP_FOUND: &str = "USBIPFOUND";

/// The Visual C++ runtime DLL `koala.exe` and `koala-tray.exe` import (the
/// media source links its own), with the property its search sets and its
/// `short|long` name.
const RUNTIME_DLLS: [(&str, &str); 1] = [("VCRUNTIME140", "VCRT140.DLL|vcruntime140.dll")];
/// The property recording the least runtime version, for `winget`.
const RUNTIME_MIN_PROPERTY: &str = "KoalaVcRuntimeMin";

/// As the winget package's publisher, as Warlock's.
const MANUFACTURER: &str = "Regulad";
/// The winget package, and its dependencies but the runtime.
const PACKAGE: &str = "Regulad.Koala";
const WARLOCK_PACKAGE: &str = "Regulad.Warlock";
const USBIP_PACKAGE: &str = "vadimgrn.usbip-win2";
/// The Windows App Runtime that `koala-tray`'s WinUI needs (Reactor's, at
/// the pinned commit), and the least of it (PLAN.md, "Distribution through
/// winget"). winget installs it; the package doesn't look for it.
const APP_RUNTIME_PACKAGE: &str = "Microsoft.WindowsAppRuntime.2";
const APP_RUNTIME_MIN: &str = "2.6.4";
/// Koala's repository, where its privacy policy is published.
const REPOSITORY: &str = "https://github.com/regulad/Koala";
const DESCRIPTION: &str = "An iPhone as this computer's microphones and camera";
const CABINET: &str = "koala.cab";
/// Windows' camera service, and its monitor, which load the media source.
const CAMERA_SERVICES: [&str; 2] = ["FrameServer", "FrameServerMonitor"];

/// A component GUID from a file's name, for FFmpeg's DLLs, which change
/// names with FFmpeg's versions: the same name keeps its component, and a new
/// name gets a new one, as the component rules ask. A SHA-256 of the name,
/// as a version 8 (custom) UUID.
fn component_guid(file_name: &str) -> String {
    let hash = Sha256::digest(format!("Koala's MSI component: {file_name}").as_bytes());
    let mut b: [u8; 16] = hash[..16].try_into().expect("16 bytes of 32");
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{{{}-{}-{}-{}-{}}}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// A file key from a file's name: Windows Installer's identifiers take no
/// hyphens.
fn file_key(file_name: &str) -> String {
    file_name.replace('-', "_")
}

/// FFmpeg's DLLs, as `tools/ffmpeg.ps1` fetched them: (name, source).
fn ffmpeg_dlls(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let bin = root.join("third_party").join("ffmpeg").join("bin");
    let mut dlls: Vec<(String, PathBuf)> = std::fs::read_dir(&bin)
        .map_err(|e| format!("{}: {e} (run tools/ffmpeg.ps1)", bin.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")))
        .filter_map(|p| Some((p.file_name()?.to_str()?.to_owned(), p)))
        .collect();
    dlls.sort();
    if dlls.is_empty() {
        return Err(format!("no FFmpeg DLLs in {} (run tools/ffmpeg.ps1)", bin.display()).into());
    }
    Ok(dlls)
}

/// Every table, filled in. `ffmpeg` are the FFmpeg DLLs' names, each its own
/// component; `runtime_min` is the least Visual C++ runtime version (`14.50`)
/// the programs need.
fn tables(
    version: &str,
    product_code: &str,
    runtime_min: &str,
    (licence, privacy): (&str, &str),
    files: &[(Payload, u64)],
    ffmpeg: &[String],
) -> Vec<Table> {
    let ui = ui::build(licence, privacy);
    let ui_properties = ui.properties();
    let mut all = ui.into_tables();
    let runtime = VcRuntime { dlls: &RUNTIME_DLLS, min: runtime_min.to_string() };

    let mut property = schema::property();
    let secure = format!("OLDERFOUND;NEWERFOUND;{WARLOCK_FOUND};{USBIP_FOUND}");
    for (k, v) in [
        (RUNTIME_MIN_PROPERTY, runtime_min),
        ("ProductCode", product_code),
        ("UpgradeCode", UPGRADE_CODE),
        ("ProductName", "Koala"),
        ("ProductVersion", version),
        ("Manufacturer", MANUFACTURER),
        ("ProductLanguage", "1033"),
        ("ALLUSERS", "1"),
        ("ARPNOMODIFY", "1"),
        ("ARPCOMMENTS", DESCRIPTION),
        ("ARPURLINFOABOUT", REPOSITORY),
        // Set in the UI (FindRelatedProducts, AppSearch), used by the
        // elevated part.
        ("SecureCustomProperties", &secure),
    ] {
        property.row(vec![s(k), s(v)]);
    }
    for (k, v) in &ui_properties {
        property.row(vec![s(k), s(v)]);
    }
    all.push(property);

    let mut directory = schema::directory();
    directory
        .row(vec![s("TARGETDIR"), None, s("SourceDir")])
        .row(vec![s("ProgramFiles64Folder"), s("TARGETDIR"), s(".")])
        .row(vec![s("INSTALLDIR"), s("ProgramFiles64Folder"), s("Koala")])
        .row(vec![s("ProgramMenuFolder"), s("TARGETDIR"), s(".")]);
    all.push(directory);

    let mut component = schema::component();
    let mut components: Vec<(String, String, String)> = vec![
        ("Koala".into(), COMPONENT_KOALA.into(), "koala.exe".into()),
        ("Tray".into(), COMPONENT_TRAY.into(), "koala_tray.exe".into()),
        ("Camera".into(), COMPONENT_CAMERA.into(), "koala_vcam_source.dll".into()),
        ("Licenses".into(), COMPONENT_LICENSES.into(), "LICENSE.md".into()),
        ("Diagnostics".into(), COMPONENT_DIAGNOSTICS.into(), "PRIVACY.md".into()),
        ("Notices".into(), COMPONENT_NOTICES.into(), "THIRD-PARTY-NOTICES.txt".into()),
    ];
    for dll in ffmpeg {
        components.push((file_key(dll), component_guid(dll), file_key(dll)));
    }
    for (name, guid, key) in &components {
        component.row(vec![s(name), s(guid), s("INSTALLDIR"), n(COMPONENT_64BIT), None, s(key)]);
    }
    all.push(component);

    let mut feature = schema::feature();
    feature.row(vec![s("Koala"), None, s("Koala"), s(DESCRIPTION), n(1), n(1), s("INSTALLDIR"), n(0)]);
    all.push(feature);

    let mut feature_components = schema::feature_components();
    for (name, _, _) in &components {
        feature_components.row(vec![s("Koala"), s(name)]);
    }
    all.push(feature_components);

    all.push(file_table(files));
    all.push(media_table(CABINET, files.len()));

    // The camera's COM class, as its DllRegisterServer writes it.
    let class = format!(r"Software\Classes\CLSID\{}", koala_vcam::clsid_string());
    let server = format!(r"{class}\InprocServer32");
    let mut registry = schema::registry();
    registry
        .row(vec![s("CameraClass"), n(HKLM), s(&class), None, s(koala_vcam::CLASS_NAME), s("Camera")])
        .row(vec![s("CameraServer"), n(HKLM), s(&server), None, s("[#koala_vcam_source.dll]"), s("Camera")])
        .row(vec![s("CameraThreading"), n(HKLM), s(&server), s("ThreadingModel"), s("Both"), s("Camera")]);
    all.push(registry);

    let mut service_control = schema::service_control();
    for name in CAMERA_SERVICES {
        let events = SERVICE_STOP_INSTALL | SERVICE_STOP_UNINSTALL;
        service_control.row(vec![s(&format!("Stop{name}")), s(name), n(events), None, n(1), s("Camera")]);
    }
    all.push(service_control);

    let mut shortcut = schema::shortcut();
    shortcut.row(vec![
        s("KoalaShortcut"),
        s("ProgramMenuFolder"),
        s("Koala"),
        s("Tray"),
        s("[#koala_tray.exe]"),
        None,
        s(DESCRIPTION),
        None,
        None,
        None,
        None,
        s("INSTALLDIR"),
        None,
        None,
        None,
        None,
    ]);
    all.push(shortcut);

    let mut upgrade = schema::upgrade();
    upgrade
        // Up to and including this version: replaced.
        .row(vec![
            s(UPGRADE_CODE),
            None,
            s(version),
            None,
            n(UPGRADE_MIGRATE_FEATURES | UPGRADE_MAX_INCLUSIVE),
            None,
            s("OLDERFOUND"),
        ])
        // Newer: only noticed, to refuse the install.
        .row(vec![s(UPGRADE_CODE), s(version), None, None, n(UPGRADE_ONLY_DETECT), None, s("NEWERFOUND")])
        // Warlock, at its least or later: only noticed.
        .row(vec![
            s(WARLOCK_UPGRADE_CODE),
            s(WARLOCK_MIN),
            None,
            None,
            n(UPGRADE_ONLY_DETECT | UPGRADE_MIN_INCLUSIVE),
            None,
            s(WARLOCK_FOUND),
        ]);
    all.push(upgrade);

    // The Visual C++ runtime, found by its DLL in System32; usbip-win2's
    // client, where its installer puts it and Koala runs it.
    let mut search = Search::new();
    runtime.search(&mut search);
    search.file("SigUsbip", "usbip.exe", Some(USBIP_FLOOR), r"[ProgramFiles64Folder]USBip", &[USBIP_FOUND]);
    all.extend(search.into_tables());

    let (runtime_found, runtime_missing) = runtime.launch_condition("Koala");
    let mut launch = schema::launch_condition();
    launch
        .row(vec![s("VersionNT64"), s("Koala needs 64-bit Windows.")])
        .row(vec![s("NOT NEWERFOUND"), s("A newer version of Koala is already installed.")])
        .row(vec![s(&runtime_found), s(&runtime_missing)])
        // Neither is needed to repair or remove Koala.
        .row(vec![
            s(&format!("Installed OR {WARLOCK_FOUND}")),
            s(&format!(
                "Koala needs Warlock {WARLOCK_MIN} or later, which carries its connection to the iPhone. Install \
                 it first (winget install {WARLOCK_PACKAGE}, or https://github.com/regulad/Warlock/releases), \
                 then run this again."
            )),
        ])
        .row(vec![
            s(&format!("Installed OR {USBIP_FOUND}")),
            s(&format!(
                "Koala needs usbip-win2 {USBIP_MIN} or later, which gives Windows the iPhone's microphones. \
                 Install it first (winget install {USBIP_PACKAGE}, or \
                 https://github.com/vadimgrn/usbip-win2/releases), then run this again."
            )),
        ]);
    all.push(launch);

    let ui_sequence: Vec<(&str, Option<&str>, i32)> = [
        ("FindRelatedProducts", None, 25),
        ("AppSearch", None, 50),
        ("LaunchConditions", None, 100),
        ("CostInitialize", None, 800),
        ("FileCost", None, 900),
        ("CostFinalize", None, 1000),
        ("MigrateFeatureStates", None, 1200),
        ("ExecuteAction", None, 1300),
    ]
    .into_iter()
    .chain(warlock_msi_kit::ui::sequence(ui::FIRST))
    .collect();
    all.push(sequence("InstallUISequence", &ui_sequence));
    all.push(sequence(
        "InstallExecuteSequence",
        &[
            ("FindRelatedProducts", None, 25),
            ("AppSearch", None, 50),
            ("LaunchConditions", None, 100),
            ("ValidateProductID", None, 700),
            ("CostInitialize", None, 800),
            ("FileCost", None, 900),
            ("CostFinalize", None, 1000),
            ("MigrateFeatureStates", None, 1200),
            ("InstallValidate", None, 1400),
            // The old version goes first, whole.
            ("RemoveExistingProducts", None, 1401),
            ("InstallInitialize", None, 1500),
            ("ProcessComponents", None, 1600),
            ("UnpublishFeatures", None, 1800),
            // The camera service lets go of the media source.
            ("StopServices", Some("VersionNT"), 1900),
            ("RemoveRegistryValues", None, 2600),
            ("RemoveShortcuts", None, 3200),
            ("RemoveFiles", None, 3500),
            ("RemoveFolders", None, 3600),
            ("CreateFolders", None, 3700),
            ("InstallFiles", None, 4000),
            ("CreateShortcuts", None, 4500),
            ("WriteRegistryValues", None, 5000),
            ("RegisterUser", None, 6000),
            ("RegisterProduct", None, 6100),
            ("PublishFeatures", None, 6300),
            ("PublishProduct", None, 6400),
            ("InstallFinalize", None, 6600),
        ],
    ));
    all.push(sequence(
        "AdminUISequence",
        &[
            ("CostInitialize", None, 800),
            ("FileCost", None, 900),
            ("CostFinalize", None, 1000),
            ("ExecuteAction", None, 1300),
        ],
    ));
    all.push(sequence(
        "AdminExecuteSequence",
        &[
            ("CostInitialize", None, 800),
            ("FileCost", None, 900),
            ("CostFinalize", None, 1000),
            ("InstallValidate", None, 1400),
            ("InstallInitialize", None, 1500),
            ("InstallAdminPackage", None, 3900),
            ("InstallFiles", None, 4000),
            ("InstallFinalize", None, 6600),
        ],
    ));
    all.push(sequence(
        "AdvtExecuteSequence",
        &[
            ("CostInitialize", None, 800),
            ("CostFinalize", None, 1000),
            ("InstallValidate", None, 1400),
            ("InstallInitialize", None, 1500),
            ("CreateShortcuts", None, 4500),
            ("RegisterClassInfo", None, 4600),
            ("PublishFeatures", None, 6300),
            ("PublishProduct", None, 6400),
            ("InstallFinalize", None, 6600),
        ],
    ));
    all
}

/// Koala's version: the workspace's, which this builder shares with every
/// other crate.
fn koala_version() -> Result<String> {
    Ok(product_version(env!("CARGO_PKG_VERSION"))?.to_string())
}

fn build(root: &Path, out: &Path, version: &str) -> Result<()> {
    let release = root.join("target").join("release");
    let ffmpeg = ffmpeg_dlls(root)?;
    // The File key, the component, the `short|long` name, the source, and
    // whether it's vital.
    let mut payload = vec![
        ("koala.exe".to_string(), "Koala".to_string(), "koala.exe".to_string(), release.join("koala.exe"), true),
        (
            "koala_tray.exe".into(),
            "Tray".into(),
            "KTRAY.EXE|koala-tray.exe".into(),
            release.join("koala-tray.exe"),
            true,
        ),
        (
            "koala_vcam_source.dll".into(),
            "Camera".into(),
            "KVCAMSRC.DLL|koala_vcam_source.dll".into(),
            release.join("koala_vcam_source.dll"),
            true,
        ),
        ("LICENSE.md".into(), "Licenses".into(), "LICENSE.MD|LICENSE.md".into(), root.join("LICENSE.md"), false),
        (
            "FFMPEG_LICENSE.txt".into(),
            "Licenses".into(),
            "FFMPEGLI.TXT|FFMPEG-LICENSE.txt".into(),
            root.join("third_party").join("ffmpeg").join("LICENSE.txt"),
            false,
        ),
        ("PRIVACY.md".into(), "Diagnostics".into(), "PRIVACY.MD|PRIVACY.md".into(), root.join("PRIVACY.md"), false),
        (
            "THIRD-PARTY-NOTICES.txt".into(),
            "Notices".into(),
            "NOTICES.TXT|THIRD-PARTY-NOTICES.txt".into(),
            root.join("crates").join("koala").join("data").join("third-party-notices.txt"),
            false,
        ),
    ];
    // Short names `FFMPEG1.DLL` and on: they only count where long names
    // don't.
    for (i, (name, source)) in ffmpeg.iter().enumerate() {
        let key = file_key(name);
        payload.push((key.clone(), key, format!("FFMPEG{}.DLL|{name}", i + 1), source.clone(), true));
    }
    let files = with_sizes(payload.into_iter().map(|(key, component, name, source, vital)| Payload {
        key,
        component,
        name,
        source,
        vital,
    }))?;

    let exes: Vec<&Path> =
        files.iter().filter(|(p, _)| p.key.ends_with(".exe")).map(|(p, _)| p.source.as_path()).collect();
    let runtime_min = runtime_min(&exes)?;
    let licence = licence::rtf(root)?;
    let privacy = licence::privacy(root)?;
    let names: Vec<String> = ffmpeg.into_iter().map(|(name, _)| name).collect();

    let product_code = new_guid()?;
    let package_code = new_guid()?;
    let db = Database::create(out)?;
    for t in tables(version, &product_code, &runtime_min, (&licence, &privacy), &files, &names) {
        db.import(&t)?;
    }
    db.cabinet(CABINET, &files)?;
    db.summary(&Summary {
        subject: "Koala",
        author: MANUFACTURER,
        keywords: "Installer, iPhone, microphone, camera",
        comments: "This installer database contains the logic and data required to install Koala.",
        // x64, English.
        template: "x64;1033",
        package_code: &package_code,
        application: "koala-msi (Windows Installer API)",
    })?;
    db.commit()?;
    println!(
        "{} (Koala {version}, product code {product_code}, Visual C++ runtime {runtime_min} or later)",
        out.display()
    );
    Ok(())
}

/// Writes winget's three manifests (version, installer, default locale) for
/// the package at `msi`, to be downloaded from `url`, into `out` (by default
/// `target/msi/winget/<version>`).
fn winget(root: &Path, msi: &Path, url: &str, out: Option<PathBuf>) -> Result<()> {
    let p = package_properties(msi)?;
    let get = |k: &str| p.get(k).map(String::as_str).ok_or_else(|| format!("{}: no {k} property", msi.display()));
    let version = get("ProductVersion")?;
    let runtime = VcRuntime { dlls: &RUNTIME_DLLS, min: get(RUNTIME_MIN_PROPERTY)?.to_string() };
    let dependencies = [
        runtime.dependency(),
        Dependency { package: WARLOCK_PACKAGE, min_version: Some(WARLOCK_MIN.into()) },
        Dependency { package: USBIP_PACKAGE, min_version: Some(USBIP_MIN.into()) },
        Dependency { package: APP_RUNTIME_PACKAGE, min_version: Some(APP_RUNTIME_MIN.into()) },
    ];
    let manifest = Manifest {
        package: PACKAGE,
        name: "Koala",
        generator: "koala-msi",
        license: "AGPL-3.0-or-later",
        short_description: DESCRIPTION,
        // The privacy policy as of this version's tag.
        privacy_url: Some(format!("{REPOSITORY}/blob/v{version}/PRIVACY.md")),
        dependencies_note: Some(
            "Warlock, which carries the connection to the iPhone; usbip-win2, which gives Windows its \
             microphones; the Windows App Runtime, which the tray's window needs; and the Visual C++ runtime \
             the programs link to.",
        ),
        dependencies: &dependencies,
    };
    let manifests = winget::manifests(&manifest, msi, &p, url)?;
    let dir = out.unwrap_or_else(|| root.join("target").join("msi").join("winget").join(version));
    std::fs::create_dir_all(&dir)?;
    for (name, text) in manifests {
        std::fs::write(dir.join(&name), text)?;
        println!("{}", dir.join(name).display());
    }
    Ok(())
}

const USAGE: &str = "usage: koala-msi [--out PATH] [--no-build]\n       \
                     koala-msi winget MSI --installer-url URL [--out DIR]";

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize()?;
    // Without the `\\?\` prefix canonicalize adds, which makecab can't read.
    let root = PathBuf::from(root.to_string_lossy().trim_start_matches(r"\\?\"));
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|a| a == "winget") {
        args.next();
        let (mut msi, mut url, mut out) = (None, None, None);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--installer-url" => url = Some(args.next().ok_or("--installer-url needs a URL")?),
                "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a folder")?)),
                _ if msi.is_none() && !a.starts_with("--") => msi = Some(PathBuf::from(a)),
                _ => return Err(format!("unknown argument {a}\n{USAGE}").into()),
            }
        }
        let msi = msi.ok_or(USAGE)?;
        let url = url.ok_or("--installer-url is needed: where winget will download the package from")?;
        return winget(&root, &msi, &url, out);
    }
    let (mut out, mut no_build) = (None, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a path")?)),
            "--no-build" => no_build = true,
            _ => return Err(format!("unknown argument {a}\n{USAGE}").into()),
        }
    }
    let version = koala_version()?;
    if !no_build {
        let status = Command::new("cargo")
            .args(["build", "--release", "-p", "koala", "-p", "koala-vcam-source"])
            .current_dir(&root)
            .status()?;
        if !status.success() {
            return Err("cargo build failed".into());
        }
    }
    let out = out.unwrap_or_else(|| root.join("target").join("msi").join(format!("Koala-{version}-x64.msi")));
    std::fs::create_dir_all(out.parent().ok_or("no folder for --out")?)?;
    build(&root, &out, &version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<Table> {
        let ffmpeg = ["avcodec-63.dll".to_string(), "avutil-61.dll".to_string()];
        tables("0.0.0", "{00000000-0000-0000-0000-000000000000}", "14.50", ("{\\rtf1 x}", "{\\rtf1 y}"), &[], &ffmpeg)
    }

    fn rows<'a>(all: &'a [Table], name: &str) -> &'a [Vec<warlock_msi_kit::Cell>] {
        &all.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no {name}")).rows
    }

    /// A name keeps its GUID, and another name gets another, in the form
    /// the Component table takes.
    #[test]
    fn ffmpegs_components_are_named_by_their_files() {
        let (a, b) = (component_guid("avcodec-63.dll"), component_guid("avcodec-64.dll"));
        assert_eq!(a, component_guid("avcodec-63.dll"));
        assert_ne!(a, b);
        assert_eq!(a.len(), 38);
        assert!(a.starts_with('{') && a.ends_with('}') && a.chars().all(|c| !c.is_ascii_lowercase()), "{a}");
        assert_eq!(&a[15..16], "8", "version 8: {a}");
        assert_eq!(file_key("avcodec-63.dll"), "avcodec_63.dll");
    }

    /// The camera's registration is its DLL's own: the class under the
    /// CLSID, its server the installed DLL, both apartments.
    #[test]
    fn the_camera_is_registered_as_its_dll_registers_itself() {
        let all = all();
        let registry = rows(&all, "Registry");
        let clsid = koala_vcam::clsid_string();
        let values: Vec<(Option<&str>, Option<&str>, Option<&str>)> =
            registry.iter().map(|r| (r[2].as_deref(), r[3].as_deref(), r[4].as_deref())).collect();
        let class = format!(r"Software\Classes\CLSID\{clsid}");
        let server = format!(r"{class}\InprocServer32");
        assert!(values.contains(&(Some(&class), None, Some(koala_vcam::CLASS_NAME))));
        assert!(values.contains(&(Some(&server), None, Some("[#koala_vcam_source.dll]"))));
        assert!(values.contains(&(Some(&server), Some("ThreadingModel"), Some("Both"))));
        // And the DLL's own registration writes these keys too.
        let dll =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../koala-vcam-source/src/lib.rs"))
                .unwrap();
        assert!(dll.contains(r#"format!(r"Software\Classes\CLSID\{}", clsid_string())"#));
        assert!(dll.contains(r#"set(&server, Some("ThreadingModel"), "Both")"#));
    }

    /// Without Warlock or usbip-win2 the install stops, saying which and how
    /// to get it; a repair or removal doesn't need them.
    #[test]
    fn warlock_and_usbip_are_required_to_install() {
        let all = all();
        let launch = rows(&all, "LaunchCondition");
        for (property, named) in [(WARLOCK_FOUND, "Warlock"), (USBIP_FOUND, "usbip-win2")] {
            let row = launch
                .iter()
                .find(|r| r[0].as_deref() == Some(&format!("Installed OR {property}")))
                .unwrap_or_else(|| panic!("no condition on {property}"));
            assert!(row[1].as_deref().unwrap().contains(named));
        }
        // Warlock is found by its upgrade code, from its least version.
        let upgrade = rows(&all, "Upgrade");
        let warlock = upgrade.iter().find(|r| r[6].as_deref() == Some(WARLOCK_FOUND)).unwrap();
        assert_eq!(warlock[0].as_deref(), Some(WARLOCK_UPGRADE_CODE));
        assert_eq!(warlock[1].as_deref(), Some(WARLOCK_MIN));
        assert_eq!(warlock[4].as_deref(), Some(&*(UPGRADE_ONLY_DETECT | UPGRADE_MIN_INCLUSIVE).to_string()));
        // usbip-win2 is found where Koala runs it.
        let usb_mics =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../koala/src/usb_mics.rs")).unwrap();
        assert!(usb_mics.contains(r#"var_os("ProgramFiles")"#) && usb_mics.contains(r#"join(r"USBip\usbip.exe")"#));
        let dr_locator = rows(&all, "DrLocator");
        assert!(dr_locator.iter().any(|r| r[2].as_deref() == Some(r"[ProgramFiles64Folder]USBip")));
        // Both are set before the elevated part reads them.
        let property = rows(&all, "Property");
        let secure = property.iter().find(|r| r[0].as_deref() == Some("SecureCustomProperties")).unwrap();
        for p in [WARLOCK_FOUND, USBIP_FOUND] {
            assert!(secure[1].as_deref().unwrap().split(';').any(|x| x == p), "{p} isn't secure");
        }
    }

    /// Every file's component exists, and every component is the feature's.
    #[test]
    fn every_component_is_installed() {
        let all = all();
        let components: Vec<&str> = rows(&all, "Component").iter().map(|r| r[0].as_deref().unwrap()).collect();
        let featured: Vec<&str> = rows(&all, "FeatureComponents").iter().map(|r| r[1].as_deref().unwrap()).collect();
        assert_eq!(components, featured);
        for name in ["Koala", "Tray", "Camera", "avcodec_63.dll", "avutil_61.dll"] {
            assert!(components.contains(&name), "no {name}");
        }
    }

    #[test]
    fn every_dialog_is_well_formed() {
        warlock_msi_kit::ui::check(&ui::build("{\\rtf1 x}", "{\\rtf1 y}").into_tables()).unwrap();
    }
}
