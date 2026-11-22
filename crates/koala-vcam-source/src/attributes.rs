//! `IMFAttributes` for an object of this DLL, forwarded to an attribute
//! store it holds: the activator is one (`IMFActivate` extends it), and the
//! camera service writes the camera's attributes into it.

use windows::Win32::Media::MediaFoundation::{IMFAttributes, MFCreateAttributes};

/// A new, empty store.
pub fn store() -> windows::core::Result<IMFAttributes> {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, 8)? };
    attributes.ok_or_else(|| windows::Win32::Foundation::E_OUTOFMEMORY.into())
}

/// Implements `IMFAttributes_Impl` for `$type` by forwarding each method to
/// `self.$field`, an `IMFAttributes`.
macro_rules! forward_attributes {
    ($type:ty, $field:ident) => {
        impl windows::Win32::Media::MediaFoundation::IMFAttributes_Impl for $type {
            fn GetItem(
                &self,
                key: *const windows::core::GUID,
                value: *mut windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.GetItem(key, (!value.is_null()).then_some(value)) }
            }
            fn GetItemType(
                &self,
                key: *const windows::core::GUID,
            ) -> windows::core::Result<windows::Win32::Media::MediaFoundation::MF_ATTRIBUTE_TYPE> {
                unsafe { self.$field.GetItemType(key) }
            }
            fn CompareItem(
                &self,
                key: *const windows::core::GUID,
                value: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> windows::core::Result<windows::core::BOOL> {
                unsafe { self.$field.CompareItem(key, value) }
            }
            fn Compare(
                &self,
                theirs: windows::core::Ref<windows::Win32::Media::MediaFoundation::IMFAttributes>,
                kind: windows::Win32::Media::MediaFoundation::MF_ATTRIBUTES_MATCH_TYPE,
            ) -> windows::core::Result<windows::core::BOOL> {
                unsafe { self.$field.Compare(theirs.as_ref(), kind) }
            }
            fn GetUINT32(&self, key: *const windows::core::GUID) -> windows::core::Result<u32> {
                unsafe { self.$field.GetUINT32(key) }
            }
            fn GetUINT64(&self, key: *const windows::core::GUID) -> windows::core::Result<u64> {
                unsafe { self.$field.GetUINT64(key) }
            }
            fn GetDouble(&self, key: *const windows::core::GUID) -> windows::core::Result<f64> {
                unsafe { self.$field.GetDouble(key) }
            }
            fn GetGUID(&self, key: *const windows::core::GUID) -> windows::core::Result<windows::core::GUID> {
                unsafe { self.$field.GetGUID(key) }
            }
            fn GetStringLength(&self, key: *const windows::core::GUID) -> windows::core::Result<u32> {
                unsafe { self.$field.GetStringLength(key) }
            }
            fn GetString(
                &self,
                key: *const windows::core::GUID,
                value: windows::core::PWSTR,
                size: u32,
                length: *mut u32,
            ) -> windows::core::Result<()> {
                let buffer = if value.is_null() {
                    &mut [][..]
                } else {
                    unsafe { std::slice::from_raw_parts_mut(value.0, size as usize) }
                };
                unsafe { self.$field.GetString(key, buffer, (!length.is_null()).then_some(length)) }
            }
            fn GetAllocatedString(
                &self,
                key: *const windows::core::GUID,
                value: *mut windows::core::PWSTR,
                length: *mut u32,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.GetAllocatedString(key, value, length) }
            }
            fn GetBlobSize(&self, key: *const windows::core::GUID) -> windows::core::Result<u32> {
                unsafe { self.$field.GetBlobSize(key) }
            }
            fn GetBlob(
                &self,
                key: *const windows::core::GUID,
                buffer: *mut u8,
                size: u32,
                length: *mut u32,
            ) -> windows::core::Result<()> {
                let buffer = if buffer.is_null() {
                    &mut [][..]
                } else {
                    unsafe { std::slice::from_raw_parts_mut(buffer, size as usize) }
                };
                unsafe { self.$field.GetBlob(key, buffer, (!length.is_null()).then_some(length)) }
            }
            fn GetAllocatedBlob(
                &self,
                key: *const windows::core::GUID,
                buffer: *mut *mut u8,
                size: *mut u32,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.GetAllocatedBlob(key, buffer, size) }
            }
            fn GetUnknown(
                &self,
                key: *const windows::core::GUID,
                iid: *const windows::core::GUID,
                out: *mut *mut core::ffi::c_void,
            ) -> windows::core::Result<()> {
                use windows::core::Interface;
                unsafe { (self.$field.vtable().GetUnknown)(self.$field.as_raw(), key, iid, out).ok() }
            }
            fn SetItem(
                &self,
                key: *const windows::core::GUID,
                value: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetItem(key, value) }
            }
            fn DeleteItem(&self, key: *const windows::core::GUID) -> windows::core::Result<()> {
                unsafe { self.$field.DeleteItem(key) }
            }
            fn DeleteAllItems(&self) -> windows::core::Result<()> {
                unsafe { self.$field.DeleteAllItems() }
            }
            fn SetUINT32(&self, key: *const windows::core::GUID, value: u32) -> windows::core::Result<()> {
                unsafe { self.$field.SetUINT32(key, value) }
            }
            fn SetUINT64(&self, key: *const windows::core::GUID, value: u64) -> windows::core::Result<()> {
                unsafe { self.$field.SetUINT64(key, value) }
            }
            fn SetDouble(&self, key: *const windows::core::GUID, value: f64) -> windows::core::Result<()> {
                unsafe { self.$field.SetDouble(key, value) }
            }
            fn SetGUID(
                &self,
                key: *const windows::core::GUID,
                value: *const windows::core::GUID,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetGUID(key, value) }
            }
            fn SetString(
                &self,
                key: *const windows::core::GUID,
                value: &windows::core::PCWSTR,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetString(key, *value) }
            }
            fn SetBlob(
                &self,
                key: *const windows::core::GUID,
                buffer: *const u8,
                size: u32,
            ) -> windows::core::Result<()> {
                let buffer = if buffer.is_null() {
                    &[][..]
                } else {
                    unsafe { std::slice::from_raw_parts(buffer, size as usize) }
                };
                unsafe { self.$field.SetBlob(key, buffer) }
            }
            fn SetUnknown(
                &self,
                key: *const windows::core::GUID,
                value: windows::core::Ref<windows::core::IUnknown>,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetUnknown(key, value.as_ref()) }
            }
            fn LockStore(&self) -> windows::core::Result<()> {
                unsafe { self.$field.LockStore() }
            }
            fn UnlockStore(&self) -> windows::core::Result<()> {
                unsafe { self.$field.UnlockStore() }
            }
            fn GetCount(&self) -> windows::core::Result<u32> {
                unsafe { self.$field.GetCount() }
            }
            fn GetItemByIndex(
                &self,
                index: u32,
                key: *mut windows::core::GUID,
                value: *mut windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.GetItemByIndex(index, key, (!value.is_null()).then_some(value)) }
            }
            fn CopyAllItems(
                &self,
                to: windows::core::Ref<windows::Win32::Media::MediaFoundation::IMFAttributes>,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.CopyAllItems(to.as_ref()) }
            }
        }
    };
}

pub(crate) use forward_attributes;
