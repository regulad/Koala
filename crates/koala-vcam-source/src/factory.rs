//! The class factory COM asks for the class: it makes an activator
//! ([`crate::activate`]), as the camera service expects.

use std::ffi::c_void;

use windows::Win32::Foundation::CLASS_E_NOAGGREGATION;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::core::{BOOL, GUID, IUnknown, Interface, Ref, implement};

use crate::Counted;

#[implement(IClassFactory)]
pub struct Factory {
    _counted: Counted,
}

impl Factory {
    pub fn new() -> Self {
        Self { _counted: Counted::new() }
    }
}

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if !out.is_null() {
            unsafe { *out = std::ptr::null_mut() };
        }
        if outer.as_ref().is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activate = crate::activate::create()?;
        unsafe { activate.query(iid, out) }.ok()
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}
