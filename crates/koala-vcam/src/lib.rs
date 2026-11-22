//! Koala's virtual camera, Koala's side (`docs/virtual-camera.md`): the
//! camera's identity, which the media source (`koala-vcam-source`, the DLL
//! Windows' camera service loads) shares; the pictures' shared memory
//! ([`frames`]); and making the camera ([`VirtualCamera`]).

pub mod frames;

use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::Win32::Media::MediaFoundation::{
    IMFVirtualCamera, MF_VERSION, MFSTARTUP_NOSOCKET, MFStartup, MFVirtualCameraAccess,
    MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime, MFVirtualCameraLifetime_Session, MFVirtualCameraType,
    MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{GUID, HRESULT, HSTRING, PCWSTR, s, w};

/// The media source's COM class.
pub const CLSID: GUID = GUID::from_u128(0x08227305_3597_4cca_b439_905a2395c9ef);

/// The camera's attribute that names the user whose Koala feeds it: their
/// SID, a string. The media source allows its shared memory to that user
/// alone ([`frames`]).
pub const USER_SID: GUID = GUID::from_u128(0x0cc9895d_fee0_48d7_939a_b1ccb06b0d3d);

/// The camera's attribute that says its user allows diagnostics: a UINT32,
/// 1 when Koala's environment has no `DO_NOT_TRACK`, which the camera
/// service, where the media source runs, can't see. Absent or 0, the
/// source sends none.
pub const DIAGNOSTICS_ALLOWED: GUID = GUID::from_u128(0x725640fe_a871_43ec_8506_cb20f0b1e908);

/// The camera's name where there's no iPhone's to give it: apps list a
/// camera by the name it's made with, and Koala's tray makes it as an
/// iPhone pairs, named after it, as a Mac does (Parker, 2026-11-21).
/// DirectShow apps see " (Windows Virtual Camera)" after it: Windows adds
/// that.
pub const DEFAULT_NAME: &str = "iPhone";

/// The CLSID as the registry and `MFCreateVirtualCamera` write it.
pub fn clsid_string() -> String {
    format!("{{{CLSID:?}}}")
}

type CreateVirtualCamera = unsafe extern "system" fn(
    MFVirtualCameraType,
    MFVirtualCameraLifetime,
    MFVirtualCameraAccess,
    PCWSTR,
    PCWSTR,
    *const GUID,
    u32,
    *mut Option<IMFVirtualCamera>,
) -> HRESULT;

/// The camera, while Koala runs: made for this user, for as long as this
/// lives (a session camera: Windows removes it when it's dropped, or when
/// Koala ends). Its object lives on a thread of its own, in the
/// multithreaded apartment, whatever the caller's thread is.
pub struct VirtualCamera {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl VirtualCamera {
    /// Makes the camera, called `name`, and starts it, its source allowed
    /// diagnostics if `diagnostics` ([`DIAGNOSTICS_ALLOWED`]). Fails where
    /// Windows is older than Windows 11 (no `MFCreateVirtualCamera`), where
    /// the media source isn't registered, or where the user doesn't allow
    /// apps the camera.
    pub fn start(name: &str, diagnostics: bool) -> Result<Self, String> {
        let sid = frames::current_user_sid()?;
        let name = name.to_owned();
        let (started_tx, started_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("virtual camera".into())
            .spawn(move || {
                let camera = match make(&name, &sid, diagnostics) {
                    Ok(camera) => {
                        let _ = started_tx.send(Ok(()));
                        camera
                    }
                    Err(e) => return drop(started_tx.send(Err(e))),
                };
                // Until dropped, or the process ends.
                let _ = stop_rx.recv();
                let _ = unsafe { camera.Shutdown() };
            })
            .map_err(|e| e.to_string())?;
        match started_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop: Some(stop_tx), thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("the virtual camera's thread ended".into()),
        }
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn make(name: &str, sid: &str, diagnostics: bool) -> Result<IMFVirtualCamera, String> {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.map_err(|e| format!("MFStartup: {e}"))?;
    // Looked up, not linked: Koala still starts on Windows 10, which has
    // no virtual cameras.
    let library = unsafe { LoadLibraryW(w!("mfsensorgroup.dll")) }
        .map_err(|e| format!("no virtual cameras on this Windows (mfsensorgroup.dll: {e})"))?;
    let create = unsafe { GetProcAddress(library, s!("MFCreateVirtualCamera")) }
        .ok_or("no virtual cameras on this Windows (MFCreateVirtualCamera)")?;
    // SAFETY: MFCreateVirtualCamera's signature (mfvirtualcamera.h).
    let create: CreateVirtualCamera = unsafe { std::mem::transmute(create) };
    let (name, clsid) = (HSTRING::from(name), HSTRING::from(clsid_string()));
    let mut camera = None;
    unsafe {
        create(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_Session,
            MFVirtualCameraAccess_CurrentUser,
            PCWSTR(name.as_ptr()),
            PCWSTR(clsid.as_ptr()),
            std::ptr::null(),
            0,
            &mut camera,
        )
    }
    .ok()
    .map_err(|e| format!("MFCreateVirtualCamera: {e}"))?;
    let camera = camera.ok_or("MFCreateVirtualCamera gave no camera")?;
    unsafe { camera.SetString(&USER_SID, &HSTRING::from(sid)) }.map_err(|e| format!("the camera's user: {e}"))?;
    unsafe { camera.SetUINT32(&DIAGNOSTICS_ALLOWED, diagnostics.into()) }
        .map_err(|e| format!("the camera's diagnostics: {e}"))?;
    // E_ACCESSDENIED here: the camera service can't read the source's DLL,
    // or the user doesn't allow apps the camera (Settings > Privacy).
    unsafe { camera.Start(None) }.map_err(|e| format!("starting the camera: {e}"))?;
    Ok(camera)
}
