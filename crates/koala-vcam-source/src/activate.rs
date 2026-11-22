//! The activator the class factory makes. The camera service makes them to
//! read the camera's attributes, which it writes into this one's store
//! (Koala's [`koala_vcam::USER_SID`] among them), and asks one for the media
//! source only when an app opens the camera: so the source is made then,
//! from the store, and making an activator costs nothing.

use std::ffi::c_void;
use std::sync::Mutex;

use koala_vcam::CLSID;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFMediaSource, MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES,
    MFT_TRANSFORM_CLSID_Attribute,
};
use windows::core::{GUID, Interface, implement};

use crate::Counted;
use crate::attributes::{forward_attributes, store};

#[implement(IMFActivate)]
pub struct Activate {
    attributes: IMFAttributes,
    source: Mutex<Option<IMFMediaSource>>,
    _counted: Counted,
}

forward_attributes!(Activate_Impl, attributes);

/// An activator, with the attributes both samples give theirs.
pub fn create() -> windows::core::Result<IMFActivate> {
    let attributes = store()?;
    unsafe {
        attributes.SetUINT32(&MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, 1)?;
        attributes.SetGUID(&MFT_TRANSFORM_CLSID_Attribute, &CLSID)?;
    }
    Ok(Activate { attributes, source: Mutex::new(None), _counted: Counted::new() }.into())
}

impl IMFActivate_Impl for Activate_Impl {
    /// The media source: made the first time, from the camera's attributes.
    fn ActivateObject(&self, iid: *const GUID, out: *mut *mut c_void) -> windows::core::Result<()> {
        let source = {
            let mut slot = self.source.lock().unwrap_or_else(|p| p.into_inner());
            match &*slot {
                Some(source) => source.clone(),
                None => slot.insert(crate::source::create(&self.attributes)?).clone(),
            }
        };
        unsafe { source.query(iid, out) }.ok()
    }

    /// Shuts the source down; another `ActivateObject` makes a new one.
    fn ShutdownObject(&self) -> windows::core::Result<()> {
        if let Some(source) = self.source.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = unsafe { source.Shutdown() };
        }
        Ok(())
    }

    /// Lets go of the source, which its caller now owns.
    fn DetachObject(&self) -> windows::core::Result<()> {
        self.source.lock().unwrap_or_else(|p| p.into_inner()).take();
        Ok(())
    }
}
