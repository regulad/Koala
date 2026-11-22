//! The media source: one stream ([`crate::stream`]), live, started and
//! stopped by the camera service. Its events, attributes and states follow
//! "Frame Server Custom Media Source" and Microsoft's `SimpleMediaSource`:
//! - on Start, for the stream selected: `MENewStream` the first time,
//!   `MEUpdatedStream` after, then the stream's `MEStreamStarted`, then
//!   `MESourceStarted`; on Stop, `MEStreamStopped` and `MESourceStopped`;
//!   no Pause, seeking or end of stream;
//! - its attributes, live (not a copy): the camera's, from the activator,
//!   and a sensor profile collection with the legacy profile, which the
//!   sample calls mandatory;
//! - `IKsControl` with no property sets, `IMFGetService` with no services;
//! - no Direct3D device, and the stream's own allocator, in memory
//!   ([`crate::stream`] says why).

use std::sync::Mutex;

use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, ERROR_SET_NOT_FOUND, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSCAMERAPROFILE_Legacy, KSIDENTIFIER};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{BOOL, ComObject, GUID, HRESULT, IUnknown, Interface, PWSTR, Ref, implement, w};

use crate::Counted;
use crate::stream::{STREAM_ID, Stream};

struct State {
    queue: Option<IMFMediaEventQueue>,
    descriptor: Option<IMFPresentationDescriptor>,
    /// Whether the stream has been announced (`MENewStream`) yet.
    announced: bool,
}

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl, IMFSampleAllocatorControl)]
pub struct Source {
    attributes: IMFAttributes,
    state: Mutex<State>,
    stream: ComObject<Stream>,
    _counted: Counted,
}

/// The source, from the camera's attributes (the activator's store).
pub fn create(camera: &IMFAttributes) -> windows::core::Result<IMFMediaSource> {
    let attributes = crate::attributes::store()?;
    let sid = unsafe {
        camera.CopyAllItems(&attributes)?;
        let profiles = MFCreateSensorProfileCollection()?;
        let legacy = MFCreateSensorProfile(&KSCAMERAPROFILE_Legacy, 0, None)?;
        legacy.AddProfileFilter(STREAM_ID, w!("((RES==;FRT<=30,1;SUT==))"))?;
        profiles.AddProfile(&legacy)?;
        attributes.SetUnknown(&MF_DEVICEMFT_SENSORPROFILE_COLLECTION, &profiles)?;
        string(camera, &koala_vcam::USER_SID)
    };
    crate::log(if sid.is_some() { "a source made" } else { "a source made, with no user: it shows black" });
    let stream = Stream::create(sid)?;
    let descriptor = unsafe { MFCreatePresentationDescriptor(Some(&[Some(stream.descriptor())]))? };
    // Selected from the start, as Microsoft's sample has it: a descriptor's
    // streams are made deselected.
    unsafe { descriptor.SelectStream(0)? };
    let queue = unsafe { MFCreateEventQueue()? };
    let source = ComObject::new(Source {
        attributes,
        state: Mutex::new(State { queue: Some(queue), descriptor: Some(descriptor), announced: false }),
        stream,
        _counted: Counted::new(),
    });
    source.to_interface::<IMFMediaSourceEx>().cast()
}

/// A string attribute, if it's there.
unsafe fn string(attributes: &IMFAttributes, key: &GUID) -> Option<String> {
    let (mut text, mut length) = (PWSTR::null(), 0u32);
    unsafe { attributes.GetAllocatedString(key, &mut text, &mut length) }.ok()?;
    let value = unsafe { text.to_string() }.ok();
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(text.0 as *const _)) };
    value
}

impl Source_Impl {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn queue(&self) -> windows::core::Result<IMFMediaEventQueue> {
        self.state().queue.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn descriptor(&self) -> windows::core::Result<IMFPresentationDescriptor> {
        self.state().descriptor.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }
}

/// The system time, as an event's value.
pub fn now() -> PROPVARIANT {
    PROPVARIANT::from(unsafe { MFGetSystemTime() })
}

impl IMFMediaEventGenerator_Impl for Source_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> windows::core::Result<IMFMediaEvent> {
        unsafe { self.queue()?.GetEvent(flags.0) }
    }

    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> windows::core::Result<()> {
        unsafe { self.queue()?.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }

    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> windows::core::Result<IMFMediaEvent> {
        unsafe { self.queue()?.EndGetEvent(result.as_ref()) }
    }

    fn QueueEvent(
        &self,
        kind: u32,
        extended: *const GUID,
        status: HRESULT,
        value: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        unsafe { self.queue()?.QueueEventParamVar(kind, extended, status, value) }
    }
}

impl IMFMediaSource_Impl for Source_Impl {
    fn GetCharacteristics(&self) -> windows::core::Result<u32> {
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> windows::core::Result<IMFPresentationDescriptor> {
        unsafe { self.descriptor()?.Clone() }
    }

    fn Start(
        &self,
        descriptor: Ref<IMFPresentationDescriptor>,
        time_format: *const GUID,
        _start: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        let descriptor = descriptor.ok()?;
        if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
            return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
        }
        let (queue, ours) = (self.queue()?, self.descriptor()?);
        if unsafe { descriptor.GetStreamDescriptorCount()? } != 1 {
            return Err(E_INVALIDARG.into());
        }
        let (mut selected, mut stream) = (BOOL::default(), None);
        unsafe { descriptor.GetStreamDescriptorByIndex(0, &mut selected, &mut stream)? };
        let stream = stream.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
        if unsafe { stream.GetStreamIdentifier()? } != STREAM_ID {
            return Err(E_INVALIDARG.into());
        }
        if selected.as_bool() {
            let media_type = unsafe { stream.GetMediaTypeHandler()?.GetCurrentMediaType()? };
            unsafe { ours.SelectStream(0)? };
            // The stream holds the source while it runs; Shutdown lets go.
            let this: IMFMediaSource =
                windows::core::IUnknownImpl::to_object(self).to_interface::<IMFMediaSourceEx>().cast()?;
            self.stream.set_source(Some(this));
            let announced = std::mem::replace(&mut self.state().announced, true);
            let kind = if announced { MEUpdatedStream } else { MENewStream };
            let stream: IMFMediaStream2 = self.stream.to_interface();
            unsafe { queue.QueueEventParamUnk(kind.0 as u32, &GUID::zeroed(), S_OK, &stream)? };
            self.stream.start(&media_type)?;
        } else {
            unsafe { ours.DeselectStream(0)? };
            self.stream.stop()?;
        }
        unsafe { queue.QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, &now()) }
    }

    fn Stop(&self) -> windows::core::Result<()> {
        let (queue, ours) = (self.queue()?, self.descriptor()?);
        self.stream.stop()?;
        unsafe {
            ours.DeselectStream(0)?;
            queue.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, &now())
        }
    }

    fn Pause(&self) -> windows::core::Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> windows::core::Result<()> {
        let queue = {
            let mut state = self.state();
            state.descriptor = None;
            state.queue.take()
        };
        let queue = queue.ok_or_else(|| windows::core::Error::from(MF_E_SHUTDOWN))?;
        self.stream.shutdown();
        unsafe { queue.Shutdown() }
    }
}

impl IMFMediaSourceEx_Impl for Source_Impl {
    fn GetSourceAttributes(&self) -> windows::core::Result<IMFAttributes> {
        Ok(self.attributes.clone())
    }

    fn GetStreamAttributes(&self, id: u32) -> windows::core::Result<IMFAttributes> {
        if id != STREAM_ID {
            return Err(E_INVALIDARG.into());
        }
        Ok(self.stream.attributes())
    }

    /// Declined, as Microsoft's `SimpleMediaSource` declines it: the stream
    /// touches no GPU ([`crate::stream`]).
    fn SetD3DManager(&self, manager: Ref<IUnknown>) -> windows::core::Result<()> {
        if manager.as_ref().is_some() {
            crate::log("a Direct3D device offered, and declined");
        }
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
}

impl IMFGetService_Impl for Source_Impl {
    fn GetService(
        &self,
        _service: *const GUID,
        _iid: *const GUID,
        _out: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

/// The stream's samples are its own, in memory ([`crate::stream`]): the
/// camera service's allocator is taken and not used.
impl IMFSampleAllocatorControl_Impl for Source_Impl {
    fn SetDefaultAllocator(&self, stream: u32, _allocator: Ref<IUnknown>) -> windows::core::Result<()> {
        if stream != STREAM_ID {
            return Err(E_INVALIDARG.into());
        }
        Ok(())
    }

    fn GetAllocatorUsage(
        &self,
        stream: u32,
        input: *mut u32,
        usage: *mut MFSampleAllocatorUsage,
    ) -> windows::core::Result<()> {
        if input.is_null() || usage.is_null() {
            return Err(E_POINTER.into());
        }
        if stream != STREAM_ID {
            return Err(E_INVALIDARG.into());
        }
        unsafe {
            *input = stream;
            *usage = MFSampleAllocatorUsage_UsesCustomAllocator;
        }
        Ok(())
    }
}

/// No property, method or event sets: what an absent set answers.
fn not_found(returned: *mut u32) -> windows::core::Result<()> {
    if !returned.is_null() {
        unsafe { *returned = 0 };
    }
    Err(HRESULT::from_win32(ERROR_SET_NOT_FOUND.0).into())
}

impl IKsControl_Impl for Source_Impl {
    fn KsProperty(
        &self,
        _property: *const KSIDENTIFIER,
        _length: u32,
        _data: *mut core::ffi::c_void,
        _data_length: u32,
        returned: *mut u32,
    ) -> windows::core::Result<()> {
        not_found(returned)
    }

    fn KsMethod(
        &self,
        _method: *const KSIDENTIFIER,
        _length: u32,
        _data: *mut core::ffi::c_void,
        _data_length: u32,
        returned: *mut u32,
    ) -> windows::core::Result<()> {
        not_found(returned)
    }

    fn KsEvent(
        &self,
        _event: *const KSIDENTIFIER,
        _length: u32,
        _data: *mut core::ffi::c_void,
        _data_length: u32,
        returned: *mut u32,
    ) -> windows::core::Result<()> {
        not_found(returned)
    }
}
