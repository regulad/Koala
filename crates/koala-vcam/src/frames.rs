//! The pictures from Koala to the camera: shared memory that the media
//! source makes when an app starts the camera, and Koala opens and writes
//! each picture to.
//!
//! The source runs in Windows' camera service, as LocalService in session 0
//! (`docs/virtual-camera.md`). Only a process in session 0, or one with
//! `SeCreateGlobalPrivilege`, can make an object in the global namespace,
//! which Koala, in the user's session, has no other way to share with it;
//! any process can open one its security allows. So the source makes them:
//! a section of the paging file and an event, `Global\Koala.Camera.1.<SID>`
//! and `…<SID>.Frame`, for the user whose Koala made the camera (its SID
//! comes in the camera's attributes, [`crate::USER_SID`]), and allows them
//! to the system, LocalService and that user only. Prior art for the
//! arrangement: DeskCam and VL.Video.VirtualCamera (see the doc).
//!
//! The section is a header and three slots of one NV12 picture each. Koala
//! writes into the slot after the latest, under a sequence lock (odd while
//! it writes, even when done), then makes it the latest and sets the event;
//! the source copies the latest, and checks the sequence again for a torn
//! copy. No lock is shared: a writer that dies leaves nothing held.

use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, GetCurrentProcess, OpenEventW, OpenProcessToken, SetEvent, WaitForSingleObject,
};
use windows::core::{HSTRING, PWSTR};

/// The camera's picture: what the media source offers, and the slots hold.
pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
/// One NV12 picture's bytes.
pub const FRAME_SIZE: usize = (WIDTH * HEIGHT * 3 / 2) as usize;

const MAGIC: u32 = u32::from_le_bytes(*b"KVC1");
const SLOTS: usize = 3;
/// The header's and each slot header's bytes: a cache line.
const LINE: usize = 64;
const SECTION_SIZE: usize = LINE + SLOTS * (LINE + FRAME_SIZE);

/// How long after its last picture a writer is taken to have gone.
pub const STALE_MS: u64 = 2_000;

#[repr(C)]
struct Header {
    magic: u32,
    width: u32,
    height: u32,
    slots: u32,
    /// The latest whole picture's number, from 1; 0 before the first.
    latest: AtomicU64,
    /// When the latest was written (`GetTickCount64`).
    written_at: AtomicU64,
    /// When the source last read one, for the writer's information.
    read_at: AtomicU64,
}

/// The names of a user's section and event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Names {
    pub section: String,
    pub event: String,
}

impl Names {
    /// The user's, in the global namespace.
    pub fn for_user(sid: &str) -> Self {
        Self::with_prefix(r"Global\", sid)
    }

    /// Under another prefix (`Local\`, for tests in one process).
    pub fn with_prefix(prefix: &str, sid: &str) -> Self {
        let section = format!("{prefix}Koala.Camera.1.{sid}");
        Self { event: format!("{section}.Frame"), section }
    }
}

/// The section, mapped, and the event.
struct Shared {
    section: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    event: HANDLE,
}

// SAFETY: the handles and the view are the process's, usable from any
// thread; the memory is accessed through atomics and the sequence lock.
unsafe impl Send for Shared {}

impl Shared {
    fn header(&self) -> &Header {
        // SAFETY: the view starts with the header, mapped for the view's life.
        unsafe { &*self.view.Value.cast::<Header>() }
    }

    /// Slot `n`: its sequence word and its picture.
    fn slot(&self, n: usize) -> (&AtomicU64, *mut u8) {
        let base = self.view.Value.cast::<u8>();
        // SAFETY: inside the section, whose size has every slot.
        unsafe {
            let slot = base.add(LINE + n * (LINE + FRAME_SIZE));
            (&*slot.cast::<AtomicU64>(), slot.add(LINE))
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.event);
            let _ = CloseHandle(self.section);
        }
    }
}

/// The media source's end: makes the section and the event, and reads.
pub struct Reader {
    shared: Shared,
    /// The number of the picture last copied.
    copied: u64,
}

impl Reader {
    /// Makes the section and event allowed to `sid` (or opens them, if they
    /// exist already: a writer may be holding them).
    pub fn create(names: &Names, sid: &str) -> Result<Self, String> {
        // Protected: the system and LocalService everything, the user read
        // and write, and nobody else.
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GRGW;;;{sid})"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut descriptor, None) }
            .map_err(|e| format!("the security descriptor for {sid}: {e}"))?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let made = (|| {
            let section = unsafe {
                CreateFileMappingW(
                    HANDLE(-1isize as _),
                    Some(&attributes),
                    PAGE_READWRITE,
                    0,
                    SECTION_SIZE as u32,
                    &HSTRING::from(&names.section),
                )
            }
            .map_err(|e| format!("{}: {e}", names.section))?;
            let event = match unsafe { CreateEventW(Some(&attributes), false, false, &HSTRING::from(&names.event)) } {
                Ok(event) => event,
                Err(e) => {
                    unsafe { CloseHandle(section) }.ok();
                    return Err(format!("{}: {e}", names.event));
                }
            };
            map(section, event, FILE_MAP_READ | FILE_MAP_WRITE)
        })();
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        let shared = made?;
        let header = shared.header();
        // A new section is zeroes; one a writer already holds is set up.
        if header.magic == 0 {
            // SAFETY: nobody else has written it: only a reader sets it up,
            // and the writer waits for the magic.
            let header = unsafe { &mut *shared.view.Value.cast::<Header>() };
            header.width = WIDTH;
            header.height = HEIGHT;
            header.slots = SLOTS as u32;
            std::sync::atomic::fence(Ordering::Release);
            header.magic = MAGIC;
        }
        Ok(Self { shared, copied: 0 })
    }

    /// Waits up to `ms` for a picture to be written.
    pub fn wait(&self, ms: u32) -> bool {
        let woken = unsafe { WaitForSingleObject(self.shared.event, ms) };
        woken == WAIT_OBJECT_0
    }

    /// Whether a writer has written within [`STALE_MS`].
    pub fn writer_present(&self) -> bool {
        let at = self.shared.header().written_at.load(Ordering::Acquire);
        at != 0 && unsafe { GetTickCount64() }.saturating_sub(at) < STALE_MS
    }

    /// Copies the latest picture into `out` ([`FRAME_SIZE`] bytes), if
    /// there's one newer than the last copied. Its number, or `None`.
    pub fn copy_latest(&mut self, out: &mut [u8]) -> Option<u64> {
        let header = self.shared.header();
        let latest = header.latest.load(Ordering::Acquire);
        if latest == 0 || latest == self.copied || out.len() < FRAME_SIZE {
            return None;
        }
        let (sequence, data) = self.shared.slot((latest % SLOTS as u64) as usize);
        let before = sequence.load(Ordering::Acquire);
        if before != 2 * latest {
            return None;
        }
        // SAFETY: the slot holds FRAME_SIZE bytes; a write racing this copy
        // is caught by the sequence check after it, and the copy discarded.
        unsafe { std::ptr::copy_nonoverlapping(data, out.as_mut_ptr(), FRAME_SIZE) };
        std::sync::atomic::fence(Ordering::Acquire);
        if sequence.load(Ordering::Relaxed) != before {
            return None;
        }
        self.copied = latest;
        header.read_at.store(unsafe { GetTickCount64() }, Ordering::Release);
        Some(latest)
    }
}

/// Koala's end: opens the section and the event, and writes.
pub struct Writer {
    shared: Shared,
}

impl Writer {
    /// Opens the user's section and event: `None` while they don't exist,
    /// which is until an app first starts the camera.
    pub fn open(names: &Names) -> Result<Option<Self>, String> {
        let section = match unsafe {
            OpenFileMappingW((FILE_MAP_READ | FILE_MAP_WRITE).0, false, &HSTRING::from(&names.section))
        } {
            Ok(section) => section,
            Err(e) if e.code() == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", names.section)),
        };
        let event = match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(&names.event)) } {
            Ok(event) => event,
            Err(e) => {
                unsafe { CloseHandle(section) }.ok();
                return Err(format!("{}: {e}", names.event));
            }
        };
        let shared = map(section, event, FILE_MAP_READ | FILE_MAP_WRITE)?;
        let header = shared.header();
        if header.magic != MAGIC || header.width != WIDTH || header.height != HEIGHT || header.slots != SLOTS as u32 {
            return Err(format!("{} isn't a camera section this Koala knows", names.section));
        }
        Ok(Some(Self { shared }))
    }

    /// Says the writer is still here, without a picture: the source goes on
    /// showing its last, where after [`STALE_MS`] it would show black (as
    /// while the iPhone's camera is paused).
    pub fn keep_alive(&mut self) {
        self.shared.header().written_at.store(unsafe { GetTickCount64() }, Ordering::Release);
    }

    /// Writes one picture, NV12 `width` by `height`: as it is if it's the
    /// camera's size, else centered on black, cut to fit.
    pub fn write(&mut self, width: u32, height: u32, nv12: &[u8]) {
        let header = self.shared.header();
        let number = header.latest.load(Ordering::Acquire) + 1;
        let (sequence, data) = self.shared.slot((number % SLOTS as u64) as usize);
        sequence.store(2 * number - 1, Ordering::Release);
        std::sync::atomic::fence(Ordering::Release);
        // SAFETY: the slot holds FRAME_SIZE bytes, which this writer alone
        // writes, and a reader copying it sees the odd sequence.
        let slot = unsafe { std::slice::from_raw_parts_mut(data, FRAME_SIZE) };
        fit(width, height, nv12, slot);
        sequence.store(2 * number, Ordering::Release);
        header.latest.store(number, Ordering::Release);
        header.written_at.store(unsafe { GetTickCount64() }, Ordering::Release);
        let _ = unsafe { SetEvent(self.shared.event) };
    }

    /// When the source last took a picture (`GetTickCount64`), if ever.
    pub fn read_at(&self) -> Option<u64> {
        Some(self.shared.header().read_at.load(Ordering::Acquire)).filter(|&t| t != 0)
    }
}

fn map(section: HANDLE, event: HANDLE, access: windows::Win32::System::Memory::FILE_MAP) -> Result<Shared, String> {
    let view = unsafe { MapViewOfFile(section, access, 0, 0, SECTION_SIZE) };
    if view.Value.is_null() {
        let e = windows::core::Error::from_thread();
        unsafe {
            let _ = CloseHandle(event);
            let _ = CloseHandle(section);
        }
        return Err(format!("mapping the camera's section: {e}"));
    }
    Ok(Shared { section, view, event })
}

/// Black, in NV12's limited range: luma 16, chroma 128.
pub fn black(out: &mut [u8]) {
    let luma = (WIDTH * HEIGHT) as usize;
    out[..luma].fill(16);
    out[luma..FRAME_SIZE].fill(128);
}

/// A picture `width` by `height` into the camera's: centered, on black
/// where it's smaller, cut where it's larger.
pub fn fit(width: u32, height: u32, nv12: &[u8], out: &mut [u8]) {
    let (w, h) = (width as usize & !1, height as usize & !1);
    if nv12.len() < w * h * 3 / 2 {
        return black(out);
    }
    let (cw, ch) = (WIDTH as usize, HEIGHT as usize);
    if (w, h) == (cw, ch) && nv12.len() >= FRAME_SIZE {
        out[..FRAME_SIZE].copy_from_slice(&nv12[..FRAME_SIZE]);
        return;
    }
    black(out);
    // The overlap, and where it starts in each, on even rows and columns.
    let (copy_w, copy_h) = (w.min(cw), h.min(ch));
    let (from_x, to_x) = (((w - copy_w) / 2) & !1, ((cw - copy_w) / 2) & !1);
    let (from_y, to_y) = (((h - copy_h) / 2) & !1, ((ch - copy_h) / 2) & !1);
    let stride_in = width as usize;
    for row in 0..copy_h {
        let src = (from_y + row) * stride_in + from_x;
        let dst = (to_y + row) * cw + to_x;
        out[dst..dst + copy_w].copy_from_slice(&nv12[src..src + copy_w]);
    }
    let (chroma_in, chroma_out) = (stride_in * height as usize, cw * ch);
    for row in 0..copy_h / 2 {
        let src = chroma_in + (from_y / 2 + row) * stride_in + from_x;
        let dst = chroma_out + (to_y / 2 + row) * cw + to_x;
        out[dst..dst + copy_w].copy_from_slice(&nv12[src..src + copy_w]);
    }
}

/// This process's user's SID, as a string (`S-1-5-21-…`).
pub fn current_user_sid() -> Result<String, String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|e| format!("OpenProcessToken: {e}"))?;
    let result = (|| {
        let mut length = 0u32;
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut length) };
        let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
        unsafe { GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr().cast()), length, &mut length) }
            .map_err(|e| format!("GetTokenInformation: {e}"))?;
        // SAFETY: the buffer holds a TOKEN_USER, aligned.
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut text = PWSTR::null();
        unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }
            .map_err(|e| format!("ConvertSidToStringSidW: {e}"))?;
        let sid = unsafe { text.to_string() }.map_err(|e| e.to_string());
        unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
        sid
    })();
    unsafe { CloseHandle(token) }.ok();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(test: &str) -> Names {
        Names::with_prefix(r"Local\", &format!("test-{test}-{}", std::process::id()))
    }

    #[test]
    fn a_picture_goes_from_writer_to_reader() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        let names = names("through");
        assert!(Writer::open(&names).unwrap().is_none(), "nothing until the source makes it");
        let mut reader = Reader::create(&names, &sid).unwrap();
        let mut writer = Writer::open(&names).unwrap().expect("the source made it");
        let mut out = vec![0u8; FRAME_SIZE];
        assert_eq!(reader.copy_latest(&mut out), None, "no picture yet");
        assert!(!reader.writer_present());
        let picture: Vec<u8> = (0..FRAME_SIZE).map(|i| (i % 251) as u8).collect();
        writer.write(WIDTH, HEIGHT, &picture);
        assert!(reader.wait(0), "the event is set");
        assert_eq!(reader.copy_latest(&mut out), Some(1));
        assert!(out == picture);
        assert!(reader.writer_present());
        assert!(writer.read_at().is_some());
        assert_eq!(reader.copy_latest(&mut out), None, "nothing newer");
        for _ in 0..4 {
            writer.write(WIDTH, HEIGHT, &picture);
        }
        assert_eq!(reader.copy_latest(&mut out), Some(5), "the latest, past the slots' wrap");
        // Its last picture long ago, then kept alive without a new one: the
        // source keeps showing the last.
        writer.shared.header().written_at.store(1, Ordering::Release);
        assert!(!reader.writer_present(), "stale");
        writer.keep_alive();
        assert!(reader.writer_present(), "kept alive");
        assert_eq!(reader.copy_latest(&mut out), None, "with nothing new to copy");
    }

    #[test]
    fn a_smaller_picture_is_centered_on_black() {
        let (w, h) = (4u32, 2u32);
        // Luma 200, chroma (90, 240).
        let small: Vec<u8> = [vec![200; 8], vec![90, 240, 90, 240]].concat();
        let mut out = vec![0u8; FRAME_SIZE];
        fit(w, h, &small, &mut out);
        let (cw, ch) = (WIDTH as usize, HEIGHT as usize);
        // On even rows and columns, for the chroma's.
        let (x, y) = (((cw - 4) / 2) & !1, ((ch - 2) / 2) & !1);
        assert_eq!(out[0], 16, "black around it");
        assert_eq!(&out[y * cw + x..y * cw + x + 4], &[200; 4]);
        assert_eq!(&out[(y + 1) * cw + x..(y + 1) * cw + x + 4], &[200; 4]);
        let chroma = cw * ch + (y / 2) * cw + x;
        assert_eq!(&out[chroma..chroma + 4], &[90, 240, 90, 240]);
        assert_eq!(out[cw * ch], 128);
    }
}
