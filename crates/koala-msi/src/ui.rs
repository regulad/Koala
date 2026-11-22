//! The package's own pages, for an install started by hand (winget's silent
//! installs never show them), `warlock_msi_kit::ui`'s standard ones in
//! Koala's words:
//!
//! - Licences ([`crate::licence`]). It asks nothing: the AGPL and the LGPL
//!   don't need accepting to install or run a program.
//! - Privacy: the privacy policy, `PRIVACY.md`, as it's installed. It asks
//!   nothing either: the policy says how to turn diagnostics off.
//! - Install folder, with a folder browser, then Install.
//! - Progress, then Done, Cancelled or Failed; a confirmation before
//!   cancelling; the error dialog Windows Installer reports through.
//! - Repair or remove, when the package is opened again on a PC that has it.

use warlock_msi_kit::ui::{Ends, Step, Ui};

/// The first page of an install.
pub const FIRST: &str = "LicenseDlg";

/// The pages. `licence` and `privacy` are the licence and privacy pages'
/// RTF.
pub fn build(licence: &str, privacy: &str) -> Ui {
    let mut ui = Ui::new("Koala");

    let step = Step {
        dialog: FIRST,
        title: "Licences",
        description: "Koala is free software, under these licences.",
        back: None,
        next: Some("PrivacyDlg"),
        before: &[],
    };
    let note = "You don't have to accept these licences to install or use Koala. They say what you may do when you \
                copy, change or share it.";
    ui.document_page(&step, "LicenseText", licence, note);

    let step = Step {
        dialog: "PrivacyDlg",
        title: "Privacy",
        description: "What Koala sends to find and fix problems, and how to turn it off.",
        back: Some(FIRST),
        next: Some("InstallDirDlg"),
        before: &[],
    };
    let note =
        "This is Koala's privacy policy, which is installed with it. Nothing on this page changes your computer.";
    ui.document_page(&step, "PrivacyText", privacy, note);

    // Install folder, then Install.
    let d = "InstallDirDlg";
    let step = Step {
        dialog: d,
        title: "Install folder",
        description: "Where Koala's programs go.",
        back: Some("PrivacyDlg"),
        next: None,
        before: &[],
    };
    ui.folder_page(&step);
    ui.text(
        d,
        "CameraNote",
        (20, 116, 330, 40),
        "Koala adds a camera to Windows, its source in this folder. Windows' camera service restarts as Koala \
         is installed, so an app using a camera now may lose it for a moment.",
    );

    ui.maintenance_page(
        "Puts Koala's files and its camera back as installed.",
        "Removes Koala, and its camera with it. Warlock and usbip-win2 stay.",
    );
    ui.progress_page();
    ui.ends(&Ends {
        installed: "Koala is installed. Start it from the Start menu: it lives in the notification area, by the \
                    clock, and connects to an iPhone from there.",
        removed: "Koala is removed, and its camera with it.",
        finish: &[("EndDialog", "Return", "1")],
        cancelled: None,
        failed: None,
    });
    ui.cancel_dialog();
    ui.error_dialog();
    ui
}
