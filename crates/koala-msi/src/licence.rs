//! The licence page's text: which part of Koala is under which licence,
//! then the licences themselves, from the repository's files; and the
//! privacy page's, `PRIVACY.md`. `warlock_msi_kit::rtf` makes them the RTF
//! a ScrollableText control shows.

use std::path::Path;

use warlock_msi_kit::{Result, rtf as convert};

/// What the page says before the licences.
const INTRO: [&str; 4] = [
    "**Koala is free software.**",
    "`koala.exe`, `koala-tray.exe` and `koala_vcam_source.dll`, its virtual camera's source, are licensed under \
     the GNU Affero General Public License, version 3 or later. Its source code is at \
     https://github.com/regulad/Koala.",
    "FFmpeg's libraries, installed beside them, are FFmpeg's, under the GNU Lesser General Public License, \
     version 3 (FFMPEG-LICENSE.txt).",
    "Both texts follow, and are installed with Koala. The programs are built with other free software, \
     Sentry's SDK among it, which sends Koala's diagnostics (PRIVACY.md says what, and how to turn them off): \
     THIRD-PARTY-NOTICES.txt, installed with Koala, has its licences.",
];

/// The licences, in the order the page shows them.
const FILES: [&str; 2] = ["LICENSE.md", "third_party/ffmpeg/LICENSE.txt"];

/// The privacy page's RTF, on one line: `PRIVACY.md`, as it's installed.
pub fn privacy(root: &Path) -> Result<String> {
    let text = std::fs::read_to_string(root.join("PRIVACY.md")).map_err(|e| format!("PRIVACY.md: {e}"))?;
    Ok(convert::document(&text))
}

/// The licence page's RTF, on one line.
pub fn rtf(root: &Path) -> Result<String> {
    let mut texts = Vec::new();
    for f in FILES {
        texts.push(std::fs::read_to_string(root.join(f)).map_err(|e| format!("{f}: {e}"))?);
    }
    Ok(convert::page(&INTRO, &texts.iter().map(String::as_str).collect::<Vec<_>>()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn the_privacy_page_is_the_policy() {
        let rtf = privacy(&root()).unwrap();
        assert!(rtf.is_ascii() && !rtf.contains(['\n', '\r', '\t', '[', ']']));
        assert!(rtf.contains("privacy policy") && rtf.contains("DO_NOT_TRACK"), "{rtf}");
    }

    /// Needs FFmpeg's folder (`tools/ffmpeg.ps1`), as the build does.
    #[test]
    fn the_page_is_one_ascii_line() {
        let rtf = rtf(&root()).unwrap();
        assert!(rtf.is_ascii() && !rtf.contains(['\n', '\r', '\t', '[', ']']));
        assert!(rtf.contains("GNU AFFERO GENERAL PUBLIC LICENSE") && rtf.contains("GNU LESSER GENERAL PUBLIC LICENSE"));
    }
}
