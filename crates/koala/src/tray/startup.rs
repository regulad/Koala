//! Starting Koala at logon: a value under the user's `Run` key, the
//! documented way for an unpackaged app ("Run and RunOnce Registry Keys").
//! Windows lists it in Settings > Apps > Startup and on Task Manager's
//! Startup apps page, where the user can turn it off. (A packaged app would
//! declare a `StartupTask` in its manifest instead; Koala has no package.)
//!
//! What those pages record isn't documented: a value of the same name under
//! `StartupApproved\Run`, whose first byte is odd when the user has turned
//! it off. Koala reads it, so the menu says what Windows will do, and turning
//! it on from the menu clears it.

use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

use super::win::wide;

/// Where a startup entry lives: the `Run` key, the key Windows' Startup pages
/// record their choice in, and the value's name in both.
pub struct Startup<'a> {
    pub run: &'a str,
    pub approved: &'a str,
    pub name: &'a str,
}

/// Koala's own entry.
pub const KOALA: Startup<'static> = Startup {
    run: r"Software\Microsoft\Windows\CurrentVersion\Run",
    approved: r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
    name: "Koala",
};

impl Startup<'_> {
    /// Whether Windows will start Koala at the next logon.
    pub fn enabled(&self) -> bool {
        self.command().is_some() && !self.turned_off()
    }

    /// The command line registered, if any.
    pub fn command(&self) -> Option<String> {
        let (key, name) = (wide(self.run), wide(self.name));
        let mut size = 0u32;
        unsafe {
            let status = RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            );
            if status != 0 || size == 0 {
                return None;
            }
            let mut data = vec![0u16; (size as usize).div_ceil(2)];
            let status = RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut size,
            );
            if status != 0 {
                return None;
            }
            let len = data.iter().position(|&c| c == 0).unwrap_or(data.len());
            Some(String::from_utf16_lossy(&data[..len]))
        }
    }

    /// Whether the user turned it off in Windows' Startup pages.
    fn turned_off(&self) -> bool {
        let (key, name) = (wide(self.approved), wide(self.name));
        let mut data = [0u8; 12];
        let mut size = data.len() as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_BINARY,
                std::ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut size,
            )
        };
        status == 0 && size > 0 && data[0] & 1 == 1
    }

    /// Starts `command` at logon, or stops starting it. Either way, Windows'
    /// own record of the user's choice is cleared: this is the user's choice.
    pub fn set(&self, command: Option<&str>) -> Result<(), String> {
        let (run, approved, name) = (wide(self.run), wide(self.approved), wide(self.name));
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, approved.as_ptr(), name.as_ptr()) };
        let status = match command {
            Some(command) => {
                let data = wide(command);
                unsafe {
                    RegSetKeyValueW(
                        HKEY_CURRENT_USER,
                        run.as_ptr(),
                        name.as_ptr(),
                        REG_SZ,
                        data.as_ptr().cast(),
                        (data.len() * 2) as u32,
                    )
                }
            }
            None => match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, run.as_ptr(), name.as_ptr()) } {
                // Already gone: ERROR_FILE_NOT_FOUND.
                2 => 0,
                status => status,
            },
        };
        if status == 0 { Ok(()) } else { Err(format!("the Run key: error {status}")) }
    }
}

/// The command that starts this program: its own path, quoted.
pub fn this_program() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(format!("\"{}\"", exe.display()))
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::Registry::{REG_BINARY, RegDeleteTreeW};

    use super::*;

    /// A throwaway place under HKCU, removed afterwards.
    const ROOT: &str = r"Software\Koala-test-startup";

    #[test]
    fn on_off_and_turned_off_in_windows() {
        let run = format!(r"{ROOT}\Run");
        let approved = format!(r"{ROOT}\Approved");
        let startup = Startup { run: &run, approved: &approved, name: "Koala" };
        assert!(!startup.enabled());

        startup.set(Some(r#""C:\Koala\koala-tray.exe""#)).unwrap();
        assert!(startup.enabled());
        assert_eq!(startup.command().as_deref(), Some(r#""C:\Koala\koala-tray.exe""#));

        // Turned off on Windows' Startup page: an odd first byte.
        let (key, name) = (wide(&approved), wide("Koala"));
        let off = [3u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), REG_BINARY, off.as_ptr().cast(), 12) };
        assert!(!startup.enabled(), "Windows won't start it");

        // On again from Koala's menu: the user's choice, so Windows' is cleared.
        startup.set(Some(r#""C:\Koala\koala-tray.exe""#)).unwrap();
        assert!(startup.enabled());

        startup.set(None).unwrap();
        assert!(!startup.enabled());
        startup.set(None).unwrap();

        let root = wide(ROOT);
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, root.as_ptr()) };
    }
}
