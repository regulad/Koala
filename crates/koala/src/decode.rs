//! The camera's video decoded, for the virtual camera: HEVC to NV12 pictures,
//! by Windows' own decoder where there is one, else by FFmpeg's (Parker,
//! 2026-11-21).
//!
//! - **Windows'** is the first Media Foundation decoder, in Windows' order of
//!   merit, that takes HEVC and gives NV12 (`MFTEnumEx`, synchronous ones):
//!   Microsoft's, from the HEVC Video Extensions. Given a Direct3D 11 device
//!   it decodes on the GPU (DXVA); where it won't take one, on the CPU.
//! - **FFmpeg's** is libavcodec's own `hevc` decoder (LGPL, in the build
//!   `tools/ffmpeg.ps1` fetches), on the CPU: where Windows has no HEVC
//!   decoder, where Windows' fails before its first picture, or where the
//!   user forces software decoding.
//!
//! H.265 defines its decoding exactly, so the two give the same pictures,
//! bit for bit; the tests hold them to it.

use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use ffmpeg::{Packet, codec, format::Pixel, frame};
use ffmpeg_next as ffmpeg;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
    ID3D11Texture2D,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{Interface, PWSTR};

/// A picture's length at 30 a second, in 100 ns units: what the decoders
/// are told each lasts.
const PICTURE: i64 = 333_333;

/// A decoded picture, in NV12: the luma plane, `width` bytes a row and
/// `height` rows, then the chroma plane, Cb and Cr interleaved, `width`
/// bytes a row and `height / 2` rows.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub nv12: Vec<u8>,
    /// Its time, in 100 ns units: the time it was given to the decoder with.
    pub time: i64,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Picture({}x{}, {} bytes, at {})", self.width, self.height, self.nv12.len(), self.time)
    }
}

/// Decodes one HEVC stream, its access units in decoding order.
pub struct Decoder {
    backend: Backend,
    /// The access units given to Windows' decoder before its first picture,
    /// for FFmpeg's to start again from if it fails.
    pending: Option<Vec<(Vec<u8>, i64)>>,
}

enum Backend {
    Windows(Box<WindowsDecoder>),
    Ffmpeg(FfmpegDecoder),
}

impl Decoder {
    /// A decoder for a stream of pictures `width` by `height` (after the
    /// SPS's cropping): Windows', unless `software` or Windows has none.
    pub fn new(software: bool, width: u32, height: u32) -> Result<Self, String> {
        if !software {
            match WindowsDecoder::new(width, height) {
                Ok(windows) => {
                    return Ok(Self { backend: Backend::Windows(Box::new(windows)), pending: Some(Vec::new()) });
                }
                Err(e) => println!("decode: Windows can't decode HEVC here ({e}); FFmpeg will"),
            }
        }
        Ok(Self { backend: Backend::Ffmpeg(FfmpegDecoder::new()?), pending: None })
    }

    /// Which decoder this is, in a few words.
    pub fn describe(&self) -> String {
        match &self.backend {
            Backend::Windows(w) => {
                format!("Windows' {}, {}", w.name, if w.manager.is_some() { "on the GPU" } else { "on the CPU" })
            }
            Backend::Ffmpeg(_) => "FFmpeg's, on the CPU".into(),
        }
    }

    /// Whether this is FFmpeg's.
    pub fn is_software(&self) -> bool {
        matches!(self.backend, Backend::Ffmpeg(_))
    }

    /// One access unit, in Annex B (each NAL unit after a start code), with
    /// its time in 100 ns units. Returns the pictures it completes.
    pub fn decode(&mut self, access_unit: &[u8], time: i64) -> Result<Vec<Picture>, String> {
        match &mut self.backend {
            Backend::Ffmpeg(f) => f.decode(access_unit, time),
            Backend::Windows(w) => {
                let result = w.decode(access_unit, time);
                let Some(pending) = &mut self.pending else { return result };
                match result {
                    Ok(pictures) if pictures.is_empty() => {
                        pending.push((access_unit.to_vec(), time));
                        Ok(pictures)
                    }
                    Ok(pictures) => {
                        self.pending = None;
                        Ok(pictures)
                    }
                    // Failed before its first picture: FFmpeg's starts again
                    // from the first access unit.
                    Err(e) => {
                        println!("decode: Windows' {} failed ({e}); FFmpeg's takes over", w.name);
                        let mut pending = self.pending.take().unwrap_or_default();
                        pending.push((access_unit.to_vec(), time));
                        let mut ffmpeg = FfmpegDecoder::new()?;
                        let mut pictures = Vec::new();
                        for (unit, time) in &pending {
                            pictures.extend(ffmpeg.decode(unit, *time)?);
                        }
                        self.backend = Backend::Ffmpeg(ffmpeg);
                        Ok(pictures)
                    }
                }
            }
        }
    }
}

/// Media Foundation, started once for the process and left running.
fn media_foundation() -> Result<(), String> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.map_err(|e| format!("MFStartup: {e}")))
        .clone()
}

/// Windows' decoder: a synchronous Media Foundation transform.
struct WindowsDecoder {
    transform: IMFTransform,
    name: String,
    /// The Direct3D 11 device manager the decoder took, if it took one.
    manager: Option<IMFDXGIDeviceManager>,
    /// The picture's size after cropping, as the SPS gives it.
    width: u32,
    height: u32,
    output: OutputFormat,
}

/// What the decoder's output type says of its pictures.
#[derive(Clone, Copy, Debug)]
struct OutputFormat {
    /// Whether the decoder makes its output samples (on the GPU it does).
    provides_samples: bool,
    sample_size: u32,
    /// The decoded size, before cropping: whole coding blocks.
    coded_width: u32,
    coded_height: u32,
    stride: Option<i32>,
    /// The part shown: its left, top, width and height.
    aperture: Option<(u32, u32, u32, u32)>,
}

impl WindowsDecoder {
    fn new(width: u32, height: u32) -> Result<Self, String> {
        // This thread in the multithreaded apartment, for Media Foundation.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        media_foundation()?;
        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_HEVC };
        let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut activates,
                &mut count,
            )
        }
        .map_err(|e| format!("MFTEnumEx: {e}"))?;
        let candidates: Vec<IMFActivate> = if activates.is_null() {
            Vec::new()
        } else {
            // SAFETY: MFTEnumEx gave an array of `count`, which is ours to
            // release and free.
            let taken = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) }
                .iter_mut()
                .filter_map(Option::take)
                .collect();
            unsafe { CoTaskMemFree(Some(activates.cast())) };
            taken
        };
        if candidates.is_empty() {
            return Err("no Media Foundation decoder takes HEVC".into());
        }
        let mut failures = Vec::new();
        for activate in candidates {
            let name = friendly_name(&activate);
            match Self::open(&activate, &name, width, height) {
                Ok(decoder) => return Ok(decoder),
                Err(e) => failures.push(format!("{name}: {e}")),
            }
        }
        Err(failures.join("; "))
    }

    fn open(activate: &IMFActivate, name: &str, width: u32, height: u32) -> Result<Self, String> {
        let transform: IMFTransform = unsafe { activate.ActivateObject() }.map_err(|e| format!("activating: {e}"))?;
        let attributes = unsafe { transform.GetAttributes() }.ok();
        // One picture out for each in, at once.
        if let Some(a) = &attributes {
            let _ = unsafe { a.SetUINT32(&MF_LOW_LATENCY, 1) };
        }
        let d3d11 = attributes.as_ref().is_some_and(|a| unsafe { a.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) != 0);
        let manager = if d3d11 {
            match d3d11_manager() {
                Ok(manager) => {
                    match unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize) } {
                        Ok(()) => Some(manager),
                        Err(e) => {
                            println!("decode: {name} won't decode on the GPU ({e}); on the CPU");
                            None
                        }
                    }
                }
                Err(e) => {
                    println!("decode: no Direct3D 11 device for {name} ({e}); on the CPU");
                    None
                }
            }
        } else {
            None
        };
        let input = unsafe { MFCreateMediaType() }.map_err(|e| e.to_string())?;
        unsafe {
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC).map_err(|e| e.to_string())?;
            input
                .SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(width) << 32) | u64::from(height))
                .map_err(|e| e.to_string())?;
            input.SetUINT64(&MF_MT_FRAME_RATE, (30 << 32) | 1).map_err(|e| e.to_string())?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| e.to_string())?;
            transform.SetInputType(0, &input, 0).map_err(|e| format!("the HEVC input type: {e}"))?;
        }
        let output = output_format(&transform)?;
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(|e| e.to_string())?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(|e| e.to_string())?;
        }
        Ok(Self { transform, name: name.to_owned(), manager, width, height, output })
    }

    fn decode(&mut self, access_unit: &[u8], time: i64) -> Result<Vec<Picture>, String> {
        let sample = memory_sample(access_unit)?;
        unsafe {
            sample.SetSampleTime(time).map_err(|e| e.to_string())?;
            sample.SetSampleDuration(PICTURE).map_err(|e| e.to_string())?;
        }
        let mut pictures = Vec::new();
        if let Err(e) = unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            if e.code() != MF_E_NOTACCEPTING {
                return Err(format!("ProcessInput: {e}"));
            }
            // Full: its pictures out first, then the access unit again.
            self.drain(&mut pictures)?;
            unsafe { self.transform.ProcessInput(0, &sample, 0) }.map_err(|e| format!("ProcessInput: {e}"))?;
        }
        self.drain(&mut pictures)?;
        Ok(pictures)
    }

    /// Takes every picture the decoder has ready.
    fn drain(&mut self, pictures: &mut Vec<Picture>) -> Result<(), String> {
        loop {
            let given = if self.output.provides_samples {
                None
            } else {
                let sample = unsafe { MFCreateSample() }.map_err(|e| e.to_string())?;
                let buffer = unsafe { MFCreateMemoryBuffer(self.output.sample_size) }.map_err(|e| e.to_string())?;
                unsafe { sample.AddBuffer(&buffer) }.map_err(|e| e.to_string())?;
                Some(sample)
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(given),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            // SAFETY: each taken or dropped once, here.
            let sample = unsafe { ManuallyDrop::take(&mut buffers[0].pSample) };
            unsafe { ManuallyDrop::drop(&mut buffers[0].pEvents) };
            match result {
                Ok(()) => {
                    let Some(sample) = sample else { return Err("ProcessOutput gave no sample".into()) };
                    pictures.push(self.picture(&sample)?);
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                // The stream's format is known now (the first picture's):
                // the output type again, then the picture.
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.output = output_format(&self.transform)?,
                Err(e) => return Err(format!("ProcessOutput: {e}")),
            }
        }
    }

    /// A decoded sample's picture, cropped to the part shown.
    fn picture(&self, sample: &IMFSample) -> Result<Picture, String> {
        let time = unsafe { sample.GetSampleTime() }.unwrap_or(0);
        let buffer = unsafe { sample.GetBufferByIndex(0) }.map_err(|e| e.to_string())?;
        let format = &self.output;
        let (left, top, width, height) = format.aperture.unwrap_or((0, 0, self.width, self.height));
        // The luma plane's height in the buffer, which the chroma follows: a
        // texture's, on the GPU, which may be taller than the coded size.
        let mut rows = format.coded_height;
        if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
            let mut texture: *mut std::ffi::c_void = std::ptr::null_mut();
            if unsafe { dxgi.GetResource(&ID3D11Texture2D::IID, &mut texture) }.is_ok() && !texture.is_null() {
                // SAFETY: GetResource gave a counted reference to a texture.
                let texture = unsafe { ID3D11Texture2D::from_raw(texture) };
                let mut desc = Default::default();
                unsafe { texture.GetDesc(&mut desc) };
                rows = desc.Height;
            }
        }
        let (width, height) = (width.min(format.coded_width), height.min(rows));
        if left % 2 != 0 || top % 2 != 0 || top + height > rows || left + width > format.coded_width {
            return Err(format!("a display area ({left}, {top}, {width}x{height}) outside the picture"));
        }
        let copy = |data: *const u8, pitch: usize, length: Option<usize>| -> Result<Vec<u8>, String> {
            let (w, h, top, left, rows) = (width as usize, height as usize, top as usize, left as usize, rows as usize);
            let needed = pitch * (rows + rows / 2);
            if pitch < left + w || length.is_some_and(|l| l < needed - (pitch - left - w)) {
                return Err(format!("a buffer of pitch {pitch} too small for {w}x{h}"));
            }
            let mut nv12 = Vec::with_capacity(w * h * 3 / 2);
            // SAFETY: the buffer holds the luma plane's `rows` rows of
            // `pitch` bytes, then the chroma plane's `rows / 2`: checked
            // above where its length is known.
            unsafe {
                for row in top..top + h {
                    nv12.extend_from_slice(std::slice::from_raw_parts(data.add(row * pitch + left), w));
                }
                for row in top / 2..(top + h) / 2 {
                    nv12.extend_from_slice(std::slice::from_raw_parts(data.add((rows + row) * pitch + left), w));
                }
            }
            Ok(nv12)
        };
        let nv12 = if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
            let (mut data, mut pitch) = (std::ptr::null_mut(), 0i32);
            unsafe { two_d.Lock2D(&mut data, &mut pitch) }.map_err(|e| format!("Lock2D: {e}"))?;
            let result = usize::try_from(pitch)
                .map_err(|_| format!("a bottom-up picture (pitch {pitch})"))
                .and_then(|pitch| copy(data, pitch, None));
            let _ = unsafe { two_d.Unlock2D() };
            result?
        } else {
            let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
            unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }.map_err(|e| format!("Lock: {e}"))?;
            let pitch = format.stride.and_then(|s| usize::try_from(s).ok()).unwrap_or(format.coded_width as usize);
            let result = copy(data, pitch, Some(length as usize));
            let _ = unsafe { buffer.Unlock() };
            result?
        };
        Ok(Picture { width, height, nv12, time })
    }
}

impl Drop for WindowsDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
        }
    }
}

/// A decoder's name, as it registered it.
fn friendly_name(activate: &IMFActivate) -> String {
    let (mut name, mut length) = (PWSTR::null(), 0u32);
    if unsafe { activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut length) }.is_err() {
        return "an unnamed decoder".into();
    }
    let text = unsafe { name.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
    text
}

/// A Direct3D 11 device on the default adapter, for video, behind a
/// Media Foundation device manager.
fn d3d11_manager() -> Result<IMFDXGIDeviceManager, String> {
    let mut device: Option<ID3D11Device> = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
    let device = device.ok_or("D3D11CreateDevice gave no device")?;
    // The decoder uses the device from its own threads.
    if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
        let _ = unsafe { multithread.SetMultithreadProtected(true) };
    }
    let (mut token, mut manager) = (0u32, None);
    unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }.map_err(|e| e.to_string())?;
    let manager = manager.ok_or("MFCreateDXGIDeviceManager gave no manager")?;
    unsafe { manager.ResetDevice(&device, token) }.map_err(|e| e.to_string())?;
    Ok(manager)
}

/// Sets the decoder's output to NV12, and reads what it says of it.
fn output_format(transform: &IMFTransform) -> Result<OutputFormat, String> {
    let mut index = 0;
    let chosen = loop {
        let available = unsafe { transform.GetOutputAvailableType(0, index) }
            .map_err(|e| format!("no NV12 output among {index} types ({e})"))?;
        if unsafe { available.GetGUID(&MF_MT_SUBTYPE) }.is_ok_and(|g| g == MFVideoFormat_NV12) {
            break available;
        }
        index += 1;
    };
    unsafe { transform.SetOutputType(0, &chosen, 0) }.map_err(|e| format!("the NV12 output type: {e}"))?;
    let size = unsafe { chosen.GetUINT64(&MF_MT_FRAME_SIZE) }.map_err(|e| format!("the output's size: {e}"))?;
    let stride = unsafe { chosen.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.ok().map(|s| s as i32);
    let mut area = MFVideoArea::default();
    // SAFETY: an MFVideoArea is plain data, as the blob is.
    let blob =
        unsafe { std::slice::from_raw_parts_mut((&raw mut area).cast::<u8>(), std::mem::size_of::<MFVideoArea>()) };
    let aperture = unsafe { chosen.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, blob, None) }.ok().and_then(|()| {
        let (x, y) = (u32::try_from(area.OffsetX.value).ok()?, u32::try_from(area.OffsetY.value).ok()?);
        let (w, h) = (u32::try_from(area.Area.cx).ok()?, u32::try_from(area.Area.cy).ok()?);
        (w > 0 && h > 0).then_some((x, y, w, h))
    });
    let info = unsafe { transform.GetOutputStreamInfo(0) }.map_err(|e| e.to_string())?;
    let provides = MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32;
    Ok(OutputFormat {
        provides_samples: info.dwFlags & provides != 0,
        sample_size: info.cbSize,
        coded_width: (size >> 32) as u32,
        coded_height: size as u32,
        stride,
        aperture,
    })
}

/// A sample holding `data` in memory.
fn memory_sample(data: &[u8]) -> Result<IMFSample, String> {
    let length = u32::try_from(data.len()).map_err(|_| "an access unit over 4 GB")?;
    let buffer = unsafe { MFCreateMemoryBuffer(length) }.map_err(|e| e.to_string())?;
    let mut target = std::ptr::null_mut();
    unsafe { buffer.Lock(&mut target, None, None) }.map_err(|e| e.to_string())?;
    // SAFETY: the buffer holds at least `length` bytes, locked.
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len()) };
    unsafe {
        buffer.Unlock().map_err(|e| e.to_string())?;
        buffer.SetCurrentLength(length).map_err(|e| e.to_string())?;
    }
    let sample = unsafe { MFCreateSample() }.map_err(|e| e.to_string())?;
    unsafe { sample.AddBuffer(&buffer) }.map_err(|e| e.to_string())?;
    Ok(sample)
}

/// FFmpeg's decoder.
struct FfmpegDecoder {
    decoder: ffmpeg::decoder::Video,
    frame: frame::Video,
}

impl FfmpegDecoder {
    fn new() -> Result<Self, String> {
        ffmpeg::init().map_err(|e| e.to_string())?;
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
        let codec = ffmpeg::decoder::find(codec::Id::HEVC).ok_or("FFmpeg has no HEVC decoder")?;
        let decoder = codec::context::Context::new_with_codec(codec)
            .decoder()
            .video()
            .map_err(|e| format!("FFmpeg's HEVC: {e}"))?;
        Ok(Self { decoder, frame: frame::Video::empty() })
    }

    fn decode(&mut self, access_unit: &[u8], time: i64) -> Result<Vec<Picture>, String> {
        let mut packet = Packet::copy(access_unit);
        packet.set_pts(Some(time));
        packet.set_dts(Some(time));
        self.decoder.send_packet(&packet).map_err(|e| format!("FFmpeg: {e}"))?;
        let mut pictures = Vec::new();
        while self.decoder.receive_frame(&mut self.frame).is_ok() {
            pictures.push(nv12(&self.frame, time)?);
        }
        Ok(pictures)
    }
}

/// FFmpeg's 4:2:0 planar picture as NV12.
fn nv12(frame: &frame::Video, time: i64) -> Result<Picture, String> {
    if !matches!(frame.format(), Pixel::YUV420P | Pixel::YUVJ420P) {
        return Err(format!("FFmpeg decoded to {:?}, not 8-bit 4:2:0", frame.format()));
    }
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    let (luma, stride) = (frame.data(0), frame.stride(0));
    for row in 0..h {
        out.extend_from_slice(&luma[row * stride..][..w]);
    }
    let (cb, cb_stride, cr, cr_stride) = (frame.data(1), frame.stride(1), frame.data(2), frame.stride(2));
    for row in 0..h / 2 {
        let (cb, cr) = (&cb[row * cb_stride..][..w / 2], &cr[row * cr_stride..][..w / 2]);
        out.extend(cb.iter().zip(cr).flat_map(|(b, r)| [*b, *r]));
    }
    Ok(Picture { width: w as u32, height: h as u32, nv12: out, time: frame.pts().unwrap_or(time) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten pictures of FFmpeg's `testsrc2`, 192×112 cropped to 108 by the
    /// SPS's conformance window, encoded by libkvazaar low-delay, with no
    /// pictures reordered, as the iPhone sends. Made by `tools/ffmpeg.ps1`'s
    /// FFmpeg:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=size=192x112:rate=30 -frames:v 10 -pix_fmt yuv420p
    ///   -c:v libkvazaar -kvazaar-params preset=ultrafast,period=0,gop=0,threads=0,owf=0
    ///   -bsf:v hevc_metadata=crop_bottom=4 -f hevc testsrc2-192x108.hevc
    /// ```
    const CLIP: &[u8] = include_bytes!("../tests/data/testsrc2-192x108.hevc");

    /// An Annex B stream's access units: one begins at a parameter set, an
    /// access unit delimiter or a prefix SEI after a picture's slices, or at
    /// a picture's first slice.
    fn access_units(stream: &[u8]) -> Vec<Vec<u8>> {
        // Each start code's `00 00 01`; a four-byte one's leading zero ends
        // the NAL unit before it.
        let starts: Vec<usize> =
            (0..stream.len().saturating_sub(2)).filter(|&i| stream[i..i + 3] == [0, 0, 1]).collect();
        let nals: Vec<&[u8]> = starts
            .iter()
            .enumerate()
            .map(|(n, &s)| {
                let end = starts.get(n + 1).map_or(stream.len(), |&e| if stream[e - 1] == 0 { e - 1 } else { e });
                &stream[s + 3..end]
            })
            .collect();
        let mut units: Vec<Vec<u8>> = Vec::new();
        let mut has_slice = false;
        for nal in nals {
            let kind = (nal[0] >> 1) & 0x3f;
            let first_slice = kind < 32 && nal.get(2).is_some_and(|b| b & 0x80 != 0);
            let starts_unit = (matches!(kind, 32..=35 | 39) || first_slice) && has_slice;
            if starts_unit || units.is_empty() {
                units.push(Vec::new());
                has_slice = false;
            }
            has_slice |= kind < 32;
            let unit = units.last_mut().unwrap();
            unit.extend_from_slice(&[0, 0, 0, 1]);
            unit.extend_from_slice(nal);
        }
        units
    }

    fn decode_all(software: bool) -> (String, Vec<Picture>) {
        let mut decoder = Decoder::new(software, 192, 108).unwrap();
        let mut pictures = Vec::new();
        for (n, unit) in access_units(CLIP).iter().enumerate() {
            pictures.extend(decoder.decode(unit, n as i64 * PICTURE).unwrap());
        }
        (decoder.describe(), pictures)
    }

    #[test]
    fn the_clip_has_ten_access_units() {
        assert_eq!(access_units(CLIP).len(), 10);
    }

    #[test]
    fn ffmpeg_decodes_every_picture_cropped() {
        let (described, pictures) = decode_all(true);
        assert_eq!(described, "FFmpeg's, on the CPU");
        assert_eq!(pictures.len(), 10);
        for (n, p) in pictures.iter().enumerate() {
            assert_eq!((p.width, p.height, p.nv12.len()), (192, 108, 192 * 108 * 3 / 2));
            assert_eq!(p.time, n as i64 * PICTURE);
        }
        assert_ne!(pictures[0].nv12, pictures[9].nv12, "testsrc2 moves");
    }

    /// A trial's recording (`koala guest --camera trial.mkv`) decoded by
    /// both, every picture compared, and each timed:
    /// `KOALA_CAMERA_MKV=trial.mkv cargo test --release -p koala decode --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "needs a trial's recording"]
    fn a_trials_recording_decodes_the_same_both_ways() {
        let Some(path) = std::env::var_os("KOALA_CAMERA_MKV") else { return };
        let units: Vec<Vec<u8>> = crate::mkv::read(std::path::Path::new(&path))
            .unwrap()
            .iter()
            .map(|unit| koala_capture::video::annex_b(&unit.nals))
            .collect();
        let mut results = Vec::new();
        for software in [false, true] {
            let mut decoder = Decoder::new(software, 1920, 1080).unwrap();
            let started = std::time::Instant::now();
            let mut pictures = Vec::new();
            for (n, unit) in units.iter().enumerate() {
                pictures.extend(decoder.decode(unit, n as i64 * PICTURE).unwrap());
            }
            let seconds = started.elapsed().as_secs_f64();
            println!(
                "{}: {} pictures in {seconds:.2} s, {:.0} a second",
                decoder.describe(),
                pictures.len(),
                pictures.len() as f64 / seconds
            );
            results.push(pictures);
        }
        assert_eq!(results[0].len(), units.len());
        let differing = results[0].iter().zip(&results[1]).filter(|(a, b)| a != b).count();
        assert_eq!(differing, 0, "pictures that differ");
    }

    /// Windows' decoder, where this PC has one, gives FFmpeg's pictures
    /// exactly, one out for each access unit in.
    #[test]
    fn windows_decodes_as_ffmpeg_does() {
        let (described, pictures) = decode_all(false);
        println!("decoded by {described}");
        if described.starts_with("FFmpeg") {
            println!("this PC has no HEVC decoder of Windows'");
            return;
        }
        let (_, expected) = decode_all(true);
        assert_eq!(pictures.len(), expected.len());
        for (n, (got, want)) in pictures.iter().zip(&expected).enumerate() {
            assert_eq!((got.width, got.height, got.time), (want.width, want.height, want.time), "picture {n}");
            assert!(got.nv12 == want.nv12, "picture {n} differs");
        }
    }
}
