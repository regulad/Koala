//! The camera's one stream: NV12, 1920×1080, 30 pictures a second.
//!
//! The camera service asks for samples (`RequestSample`, with a token
//! each), and a thread of the stream's answers them: it waits for Koala's
//! next picture, or a picture's time (1/30 s) without one, and gives the
//! oldest request a sample of the latest picture, timed with the system
//! clock. So it runs at Koala's rate while Koala sends, and at 30 a second,
//! black, while Koala doesn't. Its shared memory ([`koala_vcam::frames`]) is
//! made when the stream starts, and tried again each second until it can be.
//!
//! **The samples are its own, in memory, never the service's** (the source
//! reports `MFSampleAllocatorUsage_UsesCustomAllocator`, as Microsoft's
//! `HWMediaSource` does for its own). Where an app gives the service a GPU
//! (the Camera app does), the service's allocator makes Direct3D surfaces,
//! and writing one takes the service's device lock: from this thread, not
//! the service's, while the service may be stopping streams and waiting on
//! that lock itself. Microsoft's `SimpleMediaSource` writes them in
//! `RequestSample`, on the service's thread; this stream's thread writes
//! memory, and the service moves the samples to the GPU. That costs a copy
//! a picture. (Trial 30's crash began with a stream's Stop waiting on a
//! locked device; the stream was the built-in camera's, and this source had
//! been given no GPU then: `docs/virtual-camera.md`, "Tried".)
//!
//! A crash around the camera may not be this code's at all. A stream hung
//! in Stop makes the camera service's watchdog ask for a live kernel dump,
//! and while WSL's virtual machine runs, gathering the processors for one
//! can stall until the DPC watchdog stops Windows (bugcheck 0x133, in
//! `nt!IopLiveDumpCorralDpc` and `winhvr!WinHvpVpDispatchLoop`): Windows'
//! own bug, microsoft/WSL#10843, and trial 30's crash. Look at a crash dump
//! for those frames before suspecting this stream.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use koala_vcam::frames::{self, FRAME_SIZE, HEIGHT, Names, Reader, WIDTH};
use windows::Win32::Foundation::{E_POINTER, S_OK};
use windows::Win32::Media::KernelStreaming::PINNAME_VIDEO_CAPTURE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::{ComObject, GUID, HRESULT, IUnknown, Interface, Ref, implement};

use crate::Counted;
use crate::source::now;

pub const STREAM_ID: u32 = 0;

/// A picture's length, in 100 ns units.
const PICTURE: i64 = 333_333;
const PICTURE_MS: u32 = 33;
/// Samples the allocator keeps, as the samples have.
const SAMPLES: u32 = 10;

/// What the stream and its thread share.
struct State {
    queue: Option<IMFMediaEventQueue>,
    /// The source, while the stream runs.
    source: Option<IMFMediaSource>,
    running: MF_STREAM_STATE,
    /// The stream's own allocator, of samples in memory.
    allocator: Option<IMFVideoSampleAllocatorEx>,
    /// The tokens of requests not yet answered, oldest first.
    requests: VecDeque<Option<IUnknown>>,
    media_type: Option<IMFMediaType>,
}

// SAFETY: the COM objects held are Media Foundation's, which are
// free-threaded, and the stream's thread is in the multithreaded apartment.
unsafe impl Send for State {}

/// The state, shared with the thread.
#[derive(Clone)]
struct Shared(Arc<Mutex<State>>);

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The thread answering requests, while the stream runs.
struct Delivery {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

#[implement(IMFMediaStream2)]
pub struct Stream {
    descriptor: IMFStreamDescriptor,
    attributes: IMFAttributes,
    /// The user whose Koala feeds the camera, if the camera says.
    sid: Option<String>,
    shared: Shared,
    delivery: Mutex<Option<Delivery>>,
    _counted: Counted,
}

impl Stream {
    pub fn create(sid: Option<String>) -> windows::core::Result<ComObject<Self>> {
        let media_type = nv12()?;
        let descriptor = unsafe { MFCreateStreamDescriptor(STREAM_ID, &[Some(media_type.clone())])? };
        unsafe { descriptor.GetMediaTypeHandler()?.SetCurrentMediaType(&media_type)? };
        let attributes = crate::attributes::store()?;
        // On the stream's attributes and its descriptor alike: a capture
        // stream (the only category a virtual camera may have), of colour,
        // shared between apps.
        for store in [&attributes, &descriptor.cast::<IMFAttributes>()?] {
            unsafe {
                store.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
                store.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, STREAM_ID)?;
                store.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
                store.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color.0 as u32)?;
            }
        }
        let queue = unsafe { MFCreateEventQueue()? };
        Ok(ComObject::new(Self {
            descriptor,
            attributes,
            sid,
            shared: Shared(Arc::new(Mutex::new(State {
                queue: Some(queue),
                source: None,
                running: MF_STREAM_STATE_STOPPED,
                allocator: None,
                requests: VecDeque::new(),
                media_type: Some(media_type),
            }))),
            delivery: Mutex::new(None),
            _counted: Counted::new(),
        }))
    }

    pub fn descriptor(&self) -> IMFStreamDescriptor {
        self.descriptor.clone()
    }

    pub fn attributes(&self) -> IMFAttributes {
        self.attributes.clone()
    }

    pub fn set_source(&self, source: Option<IMFMediaSource>) {
        self.shared.lock().source = source;
    }

    /// Starts the stream in `media_type` (its only one), and its thread.
    pub fn start(&self, media_type: &IMFMediaType) -> windows::core::Result<()> {
        {
            let mut state = self.shared.lock();
            let queue = state.queue.clone().ok_or_else(|| windows::core::Error::from(MF_E_SHUTDOWN))?;
            let allocator = match state.allocator.clone() {
                Some(allocator) => allocator,
                None => {
                    // No device manager given it: its samples are in memory.
                    let mut raw = std::ptr::null_mut();
                    unsafe { MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)? };
                    // SAFETY: a counted reference to the interface asked for.
                    let allocator = unsafe { IMFVideoSampleAllocatorEx::from_raw(raw) };
                    state.allocator = Some(allocator.clone());
                    allocator
                }
            };
            unsafe { allocator.InitializeSampleAllocator(SAMPLES, media_type)? };
            crate::log("the stream started");
            state.media_type = Some(media_type.clone());
            state.running = MF_STREAM_STATE_RUNNING;
            unsafe { queue.QueueEventParamVar(MEStreamStarted.0 as u32, &GUID::zeroed(), S_OK, &now())? };
        }
        let mut delivery = self.delivery.lock().unwrap_or_else(|p| p.into_inner());
        if delivery.is_none() {
            let stop = Arc::new(AtomicBool::new(false));
            let (shared, sid, flag) = (self.shared.clone(), self.sid.clone(), stop.clone());
            let thread = std::thread::Builder::new()
                .name("koala-vcam delivery".into())
                .spawn(move || deliver(shared, sid, flag))
                .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY))?;
            *delivery = Some(Delivery { stop, thread });
        }
        Ok(())
    }

    /// Stops the stream: its thread, its requests, its allocator's samples.
    pub fn stop(&self) -> windows::core::Result<()> {
        self.end_delivery();
        crate::log("the stream stopped");
        let mut state = self.shared.lock();
        let was = std::mem::replace(&mut state.running, MF_STREAM_STATE_STOPPED);
        state.requests.clear();
        state.source = None;
        if let Some(allocator) = &state.allocator {
            let _ = unsafe { allocator.UninitializeSampleAllocator() };
        }
        if was != MF_STREAM_STATE_STOPPED
            && let Some(queue) = &state.queue
        {
            unsafe { queue.QueueEventParamVar(MEStreamStopped.0 as u32, &GUID::zeroed(), S_OK, &now())? };
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        let _ = self.stop();
        let mut state = self.shared.lock();
        if let Some(queue) = state.queue.take() {
            let _ = unsafe { queue.Shutdown() };
        }
        state.allocator = None;
    }

    fn end_delivery(&self) {
        let delivery = self.delivery.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(Delivery { stop, thread }) = delivery {
            stop.store(true, Ordering::SeqCst);
            let _ = thread.join();
        }
    }
}

/// The stream's media type: NV12 1080p30, BT.709 (HD's, which the iPhone's
/// stream doesn't name), limited range (as its SPS says).
fn nv12() -> windows::core::Result<IMFMediaType> {
    let t = unsafe { MFCreateMediaType()? };
    let pack = |high: u32, low: u32| (u64::from(high) << 32) | u64::from(low);
    unsafe {
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(WIDTH, HEIGHT))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(30, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, FRAME_SIZE as u32)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, WIDTH)?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
    }
    Ok(t)
}

/// The user's section and event: global, where the camera service makes
/// them; local, in a test, which can't.
pub fn names(sid: &str) -> Names {
    if cfg!(test) { Names::with_prefix(r"Local\", sid) } else { Names::for_user(sid) }
}

/// The stream's thread: answers requests until stopped.
fn deliver(shared: Shared, sid: Option<String>, stop: Arc<AtomicBool>) {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let names = sid.as_deref().map(names);
    let mut reader: Option<Reader> = None;
    let mut tried: Option<Instant> = None;
    let mut picture = vec![0u8; FRAME_SIZE];
    frames::black(&mut picture);
    let mut black = true;
    let mut reported = false;
    let (mut sent, mut failed) = (0u64, 0u64);
    while !stop.load(Ordering::SeqCst) {
        if reader.is_none()
            && let (Some(names), Some(sid)) = (&names, &sid)
            && tried.is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            tried = Some(Instant::now());
            match Reader::create(names, sid) {
                Ok(made) => {
                    crate::log("the shared memory is made");
                    reader = Some(made);
                }
                Err(e) if !reported => {
                    reported = true;
                    crate::log(&format!("the shared memory: {e}"));
                    crate::diagnostics::error("the shared memory", &e);
                }
                Err(_) => {}
            }
        }
        match &mut reader {
            Some(reader) => {
                reader.wait(PICTURE_MS);
                if reader.copy_latest(&mut picture).is_some() {
                    black = false;
                } else if !black && !reader.writer_present() {
                    frames::black(&mut picture);
                    black = true;
                }
            }
            None => std::thread::sleep(Duration::from_millis(PICTURE_MS.into())),
        }
        let (token, allocator, queue) = {
            let mut state = shared.lock();
            if state.running != MF_STREAM_STATE_RUNNING {
                continue;
            }
            let Some(token) = state.requests.pop_front() else { continue };
            match (state.allocator.clone(), state.queue.clone()) {
                (Some(allocator), Some(queue)) => (token, allocator, queue),
                _ => continue,
            }
        };
        match send(&allocator, &queue, &picture, token.as_ref()) {
            Ok(()) => {
                sent += 1;
                if sent == 1 || sent % 9_000 == 0 {
                    crate::log(&format!("{sent} samples sent"));
                }
            }
            // The allocator's samples all held downstream: this request
            // goes unanswered, as the samples' do.
            Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => {}
            Err(e) => {
                failed += 1;
                if failed <= 3 || failed.is_power_of_two() {
                    crate::log(&format!("a sample ({failed} failed): {e}"));
                }
                // The first of a delivery's: the rest are likely the same.
                if failed == 1 {
                    crate::diagnostics::error("delivering a sample", &e);
                }
            }
        }
    }
    crate::log(&format!("the delivery ended: {sent} samples sent, {failed} failed"));
}

/// A sample of `picture`, answering the request `token` was given with.
fn send(
    allocator: &IMFVideoSampleAllocatorEx,
    queue: &IMFMediaEventQueue,
    picture: &[u8],
    token: Option<&IUnknown>,
) -> windows::core::Result<()> {
    let sample = unsafe { allocator.AllocateSample()? };
    let buffer = unsafe { sample.GetBufferByIndex(0)? };
    fill(&buffer, picture)?;
    unsafe {
        sample.SetSampleTime(MFGetSystemTime())?;
        sample.SetSampleDuration(PICTURE)?;
        if let Some(token) = token {
            sample.SetUnknown(&MFSampleExtension_Token, token)?;
        }
        queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample)
    }
}

/// Copies an NV12 picture into a buffer, at the buffer's pitch.
fn fill(buffer: &IMFMediaBuffer, picture: &[u8]) -> windows::core::Result<()> {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    if let Ok(two_d) = buffer.cast::<IMF2DBuffer2>() {
        let (mut scan0, mut pitch, mut start, mut length) = (std::ptr::null_mut(), 0i32, std::ptr::null_mut(), 0u32);
        unsafe { two_d.Lock2DSize(MF2DBuffer_LockFlags_Write, &mut scan0, &mut pitch, &mut start, &mut length)? };
        let result = match usize::try_from(pitch) {
            Ok(pitch) if pitch >= w && (length as usize) >= pitch * h * 3 / 2 => {
                // SAFETY: the lock gives `length` bytes from `start`, which
                // hold the luma plane's `h` rows of `pitch`, then the
                // chroma's `h / 2`: checked above.
                unsafe {
                    for row in 0..h {
                        std::ptr::copy_nonoverlapping(picture[row * w..].as_ptr(), scan0.add(row * pitch), w);
                    }
                    for row in 0..h / 2 {
                        let from = picture[w * h + row * w..].as_ptr();
                        std::ptr::copy_nonoverlapping(from, scan0.add((h + row) * pitch), w);
                    }
                }
                Ok(())
            }
            _ => Err(windows::core::Error::from(MF_E_BUFFERTOOSMALL)),
        };
        unsafe { two_d.Unlock2D()? };
        return result;
    }
    let (mut data, mut max) = (std::ptr::null_mut(), 0u32);
    unsafe { buffer.Lock(&mut data, Some(&mut max), None)? };
    let result = if (max as usize) < FRAME_SIZE {
        Err(windows::core::Error::from(MF_E_BUFFERTOOSMALL))
    } else {
        // SAFETY: the buffer holds `max` bytes, at least FRAME_SIZE.
        unsafe { std::ptr::copy_nonoverlapping(picture.as_ptr(), data, FRAME_SIZE) };
        Ok(())
    };
    unsafe {
        buffer.Unlock()?;
        buffer.SetCurrentLength(FRAME_SIZE as u32)?;
    }
    result
}

impl Stream_Impl {
    fn queue(&self) -> windows::core::Result<IMFMediaEventQueue> {
        self.shared.lock().queue.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }
}

impl IMFMediaEventGenerator_Impl for Stream_Impl {
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

impl IMFMediaStream_Impl for Stream_Impl {
    fn GetMediaSource(&self) -> windows::core::Result<IMFMediaSource> {
        self.shared.lock().source.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn GetStreamDescriptor(&self) -> windows::core::Result<IMFStreamDescriptor> {
        Ok(self.descriptor.clone())
    }

    fn RequestSample(&self, token: Ref<IUnknown>) -> windows::core::Result<()> {
        let mut state = self.shared.lock();
        if state.queue.is_none() {
            return Err(MF_E_SHUTDOWN.into());
        }
        if state.running != MF_STREAM_STATE_RUNNING {
            return Err(MF_E_MEDIA_SOURCE_WRONGSTATE.into());
        }
        state.requests.push_back(token.as_ref().cloned());
        Ok(())
    }
}

impl IMFMediaStream2_Impl for Stream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> windows::core::Result<()> {
        let current = self.shared.lock().running;
        match value {
            v if v == current => Ok(()),
            MF_STREAM_STATE_RUNNING => {
                let media_type =
                    self.shared.lock().media_type.clone().ok_or_else(|| windows::core::Error::from(E_POINTER))?;
                self.start(&media_type)
            }
            MF_STREAM_STATE_STOPPED => self.stop(),
            // Paused: no samples, the thread idle.
            _ => {
                self.shared.lock().running = value;
                Ok(())
            }
        }
    }

    fn GetStreamState(&self) -> windows::core::Result<MF_STREAM_STATE> {
        Ok(self.shared.lock().running)
    }
}
