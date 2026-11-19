//! The Bluetooth controller's HCI traffic, from Windows' own trace: the ETW
//! provider `Microsoft-Windows-BTH-BTHPORT`, whose event 402 carries each
//! HCI packet (the provider Microsoft's Bluetooth sniffer reads). Needs
//! Administrator rights.
//!
//! Koala uses it for one thing: the random address Windows gives an advert.
//! For each WinRT advert Windows creates an extended advertising set, sets
//! its data (LE Set Extended Advertising Data, 0x2037), then a new random
//! address (LE Set Advertising Set Random Address, 0x2035), and enables it.
//! An app is never told the address (`docs/guest-pairing.md`).

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, CloseTrace, ControlTraceW, EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD,
    EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, EnableTraceEx2,
    OpenTraceW, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME, ProcessTrace, StartTraceW,
    WNODE_FLAG_TRACED_GUID,
};
use windows_sys::core::GUID;

/// `Microsoft-Windows-BTH-BTHPORT`.
const BTHPORT: GUID = GUID::from_u128(0x8a1f9517_3a8c_4a9e_a018_4f17a200f277);
/// Its event carrying an HCI packet.
const HCI_PACKET: u16 = 402;
const SESSION: &str = "Koala-HCI";

/// A running trace session; stopped when dropped.
pub struct HciTrace {
    packets: Mutex<Receiver<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
}

impl HciTrace {
    pub fn start() -> Result<Self, String> {
        let name = wide(SESSION);
        // A session left by a run that didn't end cleanly.
        let mut stale = Properties::new();
        unsafe {
            ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, name.as_ptr(), stale.as_mut_ptr(), EVENT_TRACE_CONTROL_STOP)
        };
        let mut properties = Properties::new();
        let mut control = CONTROLTRACE_HANDLE { Value: 0 };
        let status = unsafe { StartTraceW(&mut control, name.as_ptr(), properties.as_mut_ptr()) };
        if status != 0 {
            return Err(format!("starting the HCI trace: error {status} (it needs Administrator rights)"));
        }
        let status = unsafe {
            EnableTraceEx2(
                control,
                &BTHPORT,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER,
                0xff,
                u64::MAX,
                0,
                0,
                std::ptr::null(),
            )
        };
        if status != 0 {
            stop();
            return Err(format!("enabling BTHPORT's trace: error {status}"));
        }
        let (tx, rx) = channel();
        let thread = std::thread::spawn(move || consume(tx));
        Ok(Self { packets: Mutex::new(rx), thread: Some(thread) })
    }

    /// Waits for the advertising set whose data contains `data` (the
    /// manufacturer data as sent, after the company ID) and returns its
    /// handle and random address, most significant byte first.
    pub fn advert_address(&self, data: &[u8], timeout: Duration) -> Option<(u8, [u8; 6])> {
        let packets = self.packets.lock().unwrap();
        let started = Instant::now();
        let mut ours = None;
        while let Some(left) = timeout.checked_sub(started.elapsed()) {
            let Ok(packet) = packets.recv_timeout(left) else { break };
            // LE Set Extended Advertising Data: 37 20 <len> <handle> … <data>.
            if let Some(at) = find(&packet, &[0x37, 0x20])
                && find(&packet[at..], data).is_some()
            {
                ours = packet.get(at + 3).copied();
            }
            // LE Set Advertising Set Random Address: 35 20 07 <handle> <6 bytes, LSB first>.
            if let Some(at) = find(&packet, &[0x35, 0x20, 0x07])
                && let Some(handle) = ours
                && packet.get(at + 3) == Some(&handle)
                && let Some(bytes) = packet.get(at + 4..at + 10)
            {
                let mut address: [u8; 6] = bytes.try_into().unwrap();
                address.reverse();
                return Some((handle, address));
            }
        }
        None
    }
}

impl Drop for HciTrace {
    fn drop(&mut self) {
        stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn stop() {
    let name = wide(SESSION);
    let mut properties = Properties::new();
    unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE { Value: 0 },
            name.as_ptr(),
            properties.as_mut_ptr(),
            EVENT_TRACE_CONTROL_STOP,
        )
    };
}

/// Reads the session until it stops, sending each HCI packet event's data.
fn consume(tx: Sender<Vec<u8>>) {
    let context = Box::into_raw(Box::new(tx));
    let mut name = wide(SESSION);
    let mut logfile: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
    logfile.LoggerName = name.as_mut_ptr();
    logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.Anonymous2.EventRecordCallback = Some(on_event);
    logfile.Context = context.cast();
    let handle = unsafe { OpenTraceW(&mut logfile) };
    if handle.Value != u64::MAX {
        unsafe {
            ProcessTrace(&handle, 1, std::ptr::null(), std::ptr::null());
            CloseTrace(handle);
        }
    }
    drop(unsafe { Box::from_raw(context) });
}

unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    let record = unsafe { &*record };
    if record.EventHeader.EventDescriptor.Id != HCI_PACKET || record.UserData.is_null() {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(record.UserData.cast::<u8>(), record.UserDataLength as usize) };
    let tx = unsafe { &*record.UserContext.cast::<Sender<Vec<u8>>>() };
    let _ = tx.send(data.to_vec());
}

/// `EVENT_TRACE_PROPERTIES` with room for the session's name after it.
struct Properties(Vec<u8>);

impl Properties {
    fn new() -> Self {
        let header = std::mem::size_of::<EVENT_TRACE_PROPERTIES>();
        let mut buffer = vec![0u8; header + 2 * 1024];
        let properties = buffer.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
        unsafe {
            (*properties).Wnode.BufferSize = buffer.len() as u32;
            (*properties).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*properties).Wnode.ClientContext = 1;
            (*properties).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            (*properties).LoggerNameOffset = header as u32;
        }
        Self(buffer)
    }

    fn as_mut_ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
        self.0.as_mut_ptr().cast()
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
