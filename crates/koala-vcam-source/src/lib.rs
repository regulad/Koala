//! `koala_vcam_source.dll`: the media source of Koala's virtual camera
//! (`docs/virtual-camera.md`).
//!
//! Windows' camera service, the Frame Server, loads it as the COM class
//! [`koala_vcam::CLSID`] when an app opens the camera Koala makes (and its
//! monitor loads it when Koala makes it), as LocalService in session 0. It
//! runs there, not in Koala: what it has from Koala comes through the
//! camera's attributes (the user, [`koala_vcam::USER_SID`]) and the shared
//! memory [`koala_vcam::frames`] describes. `regsvr32` registers it, under
//! `HKEY_LOCAL_MACHINE` (the service reads no user's classes): Administrator
//! rights, once.
//!
//! It offers one stream: NV12, 1920×1080, 30 pictures a second, BT.709 in
//! limited range ([`stream`]). Its pictures are Koala's latest; black while
//! Koala sends none.
//!
//! The contract the camera service holds a source to is Microsoft's "Frame
//! Server Custom Media Source". Prior art, both MIT, followed for it:
//! Microsoft's Windows-Camera `Samples/VirtualCamera` (`SimpleMediaSource`,
//! `SimpleMediaStream`) and Simon Mourier's VCamSample (`MediaSource`,
//! `MediaStream`).

mod activate;
mod attributes;
mod diagnostics;
mod factory;
mod source;
mod stream;
#[cfg(test)]
mod tests;

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use koala_vcam::{CLSID, clsid_string};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, E_POINTER, E_UNEXPECTED, HINSTANCE, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleFileNameW, GetModuleHandleExW,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, REG_SZ, RegDeleteTreeW, RegSetKeyValueW};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::{BOOL, GUID, HRESULT, HSTRING, Interface, PCWSTR};

/// What `DllRegisterServer` answers when it can't (olectl.h).
const SELFREG_E_CLASS: HRESULT = HRESULT(0x8004_0201_u32 as i32);

/// This DLL, as Windows loaded it.
static MODULE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// Objects of this DLL alive: it can be unloaded at none.
static OBJECTS: AtomicUsize = AtomicUsize::new(0);

/// Counts its owner among [`OBJECTS`].
pub(crate) struct Counted;

impl Counted {
    pub(crate) fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A line for a debugger (the camera service has no console: DebugView, or
/// Microsoft's `Debug-FrameServer` scripts), and for `koala-vcam.log` in the
/// process's temporary directory: the camera service's, LocalService's, is
/// `C:\Windows\ServiceProfiles\LocalService\AppData\Local\Temp`. The file
/// is kept under 1 MB, its predecessor beside it as `koala-vcam.log.old`.
/// It holds what the source did, not what it was given: no names, pictures
/// or SIDs.
pub(crate) fn log(message: &str) {
    let line = HSTRING::from(format!("koala-vcam: {message}\n"));
    unsafe { OutputDebugStringW(PCWSTR(line.as_ptr())) };
    let time = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let path = std::env::temp_dir().join("koala-vcam.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(
            file,
            "{:02}:{:02}:{:02}.{:03} [{}] {message}",
            time.wHour,
            time.wMinute,
            time.wSecond,
            time.wMilliseconds,
            std::process::id()
        );
    }
}

/// Keeps this DLL loaded until the process ends, whatever
/// `DllCanUnloadNow` answers: for the diagnostics' threads, which run its
/// code ([`diagnostics`]).
pub(crate) fn pin() {
    let mut module = HMODULE::default();
    // Any address inside this DLL names it: this function's.
    let inside = PCWSTR(pin as *const () as *const u16);
    let flags = GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS;
    if let Err(e) = unsafe { GetModuleHandleExW(flags, inside, &mut module) } {
        log(&format!("pinning this DLL: {e}"));
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllMain(module: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(module.0, Ordering::SeqCst);
    }
    true.into()
}

/// The class factory for [`CLSID`], the only class here.
///
/// # Safety
/// COM's contract: valid pointers from the caller.
#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || clsid.is_null() || iid.is_null() {
        return E_POINTER;
    }
    unsafe { *out = std::ptr::null_mut() };
    if unsafe { *clsid } != CLSID {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = factory::Factory::new().into();
    unsafe { factory.query(iid, out) }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) == 0 { S_OK } else { S_FALSE }
}

/// Registers the class (`regsvr32`): `HKLM\Software\Classes\CLSID\{…}`,
/// its `InprocServer32` this DLL, free-threaded or not (`Both`).
#[unsafe(no_mangle)]
extern "system" fn DllRegisterServer() -> HRESULT {
    match register() {
        Ok(()) => S_OK,
        Err(e) => {
            log(&format!("registering: {e}"));
            SELFREG_E_CLASS
        }
    }
}

/// Removes the class (`regsvr32 /u`).
#[unsafe(no_mangle)]
extern "system" fn DllUnregisterServer() -> HRESULT {
    let key = HSTRING::from(class_key());
    match unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &key) }.ok() {
        Ok(()) => S_OK,
        Err(e) if e.code() == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.to_hresult() => S_OK,
        Err(e) => {
            log(&format!("unregistering: {e}"));
            SELFREG_E_CLASS
        }
    }
}

fn class_key() -> String {
    format!(r"Software\Classes\CLSID\{}", clsid_string())
}

fn register() -> Result<(), String> {
    let module = MODULE.load(Ordering::SeqCst);
    if module.is_null() {
        return Err(E_UNEXPECTED.to_string());
    }
    let mut path = vec![0u16; 32_768];
    let length = unsafe { GetModuleFileNameW(Some(HMODULE(module)), &mut path) } as usize;
    if length == 0 || length >= path.len() {
        return Err("this DLL's path".into());
    }
    let path = String::from_utf16_lossy(&path[..length]);
    let class = class_key();
    set(&class, None, "Koala's virtual camera")?;
    let server = format!(r"{class}\InprocServer32");
    set(&server, None, &path)?;
    set(&server, Some("ThreadingModel"), "Both")
}

/// A string value under `HKEY_LOCAL_MACHINE`, its key made if need be.
fn set(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    let data: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let (key_name, value_name) = (HSTRING::from(key), name.map(HSTRING::from));
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            &key_name,
            value_name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    };
    status.ok().map_err(|e| format!(r"HKLM\{key}: {e}"))
}
