//! The source in this process, driven as the camera service drives it:
//! activated with the camera's attributes, started, given Koala's picture
//! through the shared memory, asked for a sample.

use std::time::{Duration, Instant};

use koala_vcam::frames::{self, FRAME_SIZE, HEIGHT, WIDTH, Writer};
use windows::Win32::Media::KernelStreaming::PINNAME_VIDEO_CAPTURE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::{HSTRING, IUnknown, Interface};

/// The next event of `kind` from a generator, within a second.
fn event(generator: &IMFMediaEventGenerator, kind: i32) -> IMFMediaEvent {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match unsafe { generator.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
            Ok(e) if unsafe { e.GetType() }.unwrap() == kind as u32 => return e,
            Ok(e) => println!("event {} while waiting for {kind}", unsafe { e.GetType() }.unwrap()),
            Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => panic!("GetEvent: {e}"),
        }
        assert!(Instant::now() < deadline, "no event {kind} within a second");
    }
}

/// An event's object.
fn object<T: Interface>(event: &IMFMediaEvent) -> T {
    let value = unsafe { event.GetValue() }.unwrap();
    IUnknown::try_from(&value).unwrap().cast().unwrap()
}

/// The NV12 picture in a sample, rows unpadded.
fn picture(sample: &IMFSample) -> Vec<u8> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.unwrap();
    let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
    unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }.unwrap();
    let out = unsafe { std::slice::from_raw_parts(data, length as usize) }.to_vec();
    unsafe { buffer.Unlock() }.unwrap();
    out
}

#[test]
fn a_started_source_answers_a_request_with_koalas_picture() {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.unwrap();
    let sid = frames::current_user_sid().unwrap();
    let activate = crate::activate::create().unwrap();
    // As Koala sets it on the camera, and the camera service passes it on.
    unsafe { activate.SetString(&koala_vcam::USER_SID, &HSTRING::from(&sid)) }.unwrap();
    let source: IMFMediaSourceEx = unsafe { activate.ActivateObject() }.unwrap();
    // What the camera service reads before starting it.
    let attributes = unsafe { source.GetSourceAttributes() }.unwrap();
    assert!(unsafe { attributes.GetUnknown::<IUnknown>(&MF_DEVICEMFT_SENSORPROFILE_COLLECTION) }.is_ok());
    let stream_attributes = unsafe { source.GetStreamAttributes(0) }.unwrap();
    assert_eq!(unsafe { stream_attributes.GetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY) }.unwrap(), PINNAME_VIDEO_CAPTURE);
    let descriptor = unsafe { source.CreatePresentationDescriptor() }.unwrap();
    let (mut selected, mut stream_descriptor) = (Default::default(), None);
    unsafe { descriptor.GetStreamDescriptorByIndex(0, &mut selected, &mut stream_descriptor) }.unwrap();
    let media_type =
        unsafe { stream_descriptor.unwrap().GetMediaTypeHandler().unwrap().GetCurrentMediaType() }.unwrap();
    assert_eq!(unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }.unwrap(), MFVideoFormat_NV12);
    assert_eq!(
        unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap(),
        (u64::from(WIDTH) << 32) | u64::from(HEIGHT)
    );

    unsafe { source.Start(&descriptor, std::ptr::null(), &PROPVARIANT::default()) }.unwrap();
    let generator: IMFMediaEventGenerator = source.cast().unwrap();
    let stream: IMFMediaStream = object(&event(&generator, MENewStream.0));
    event(&generator, MESourceStarted.0);
    let stream_events: IMFMediaEventGenerator = stream.cast().unwrap();
    event(&stream_events, MEStreamStarted.0);

    // Black, before Koala has written.
    unsafe { stream.RequestSample(None) }.unwrap();
    let sample: IMFSample = object(&event(&stream_events, MEMediaSample.0));
    let got = picture(&sample);
    assert_eq!(got.len(), FRAME_SIZE);
    assert!(got[..(WIDTH * HEIGHT) as usize].iter().all(|&y| y == 16), "black luma");
    assert!(unsafe { sample.GetSampleTime() }.unwrap() > 0);

    // Koala's picture, once the stream has made its shared memory.
    let names = crate::stream::names(&sid);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut writer = loop {
        if let Some(writer) = Writer::open(&names).unwrap() {
            break writer;
        }
        assert!(Instant::now() < deadline, "the stream made no shared memory");
        std::thread::sleep(Duration::from_millis(10));
    };
    let koala: Vec<u8> = (0..FRAME_SIZE).map(|i| (i % 253) as u8).collect();
    writer.write(WIDTH, HEIGHT, &koala);
    std::thread::sleep(Duration::from_millis(50));
    unsafe { stream.RequestSample(None) }.unwrap();
    let sample: IMFSample = object(&event(&stream_events, MEMediaSample.0));
    assert!(picture(&sample) == koala, "Koala's picture, as written");

    unsafe { source.Stop() }.unwrap();
    event(&stream_events, MEStreamStopped.0);
    event(&generator, MESourceStopped.0);
    unsafe { source.Shutdown() }.unwrap();
}
