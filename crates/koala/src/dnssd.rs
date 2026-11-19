//! Registering a service with Windows' own DNS-SD (`DnsServiceRegister`),
//! on one interface: the Warlock adapter, where Warlock puts it in its AWDL
//! frames (Warlock's `docs/service-discovery.md`).

use std::ffi::c_void;
use std::net::Ipv6Addr;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use windows_sys::Win32::Foundation::DNS_REQUEST_PENDING;
use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_REQUEST_VERSION1, DNS_SERVICE_INSTANCE, DNS_SERVICE_REGISTER_REQUEST, DnsServiceConstructInstance,
    DnsServiceDeRegister, DnsServiceFreeInstance, DnsServiceRegister, IP6_ADDRESS,
};

/// A registered service, withdrawn (with a goodbye) when dropped.
pub struct Registration {
    request: Box<DNS_SERVICE_REGISTER_REQUEST>,
    instance: *mut DNS_SERVICE_INSTANCE,
    context: *mut Sender<u32>,
}

// The raw pointers are only used by this value and Windows' callbacks.
unsafe impl Send for Registration {}

impl Registration {
    /// Registers `instance` (a full name, such as `RDLink-…._rdlink._tcp.local`)
    /// at `host:port`, `host` having `address`, with the TXT record `txt`, on
    /// interface `index`. Waits for Windows to confirm it.
    pub fn register(
        index: u32,
        instance: &str,
        host: &str,
        address: Ipv6Addr,
        port: u16,
        txt: &[(&str, &str)],
    ) -> Result<Self, String> {
        let (name, host) = (wide(instance), wide(host));
        let keys: Vec<Vec<u16>> = txt.iter().map(|(k, _)| wide(k)).collect();
        let values: Vec<Vec<u16>> = txt.iter().map(|(_, v)| wide(v)).collect();
        let key_ptrs: Vec<*const u16> = keys.iter().map(|k| k.as_ptr()).collect();
        let value_ptrs: Vec<*const u16> = values.iter().map(|v| v.as_ptr()).collect();
        let ip6 = IP6_ADDRESS { IP6Byte: address.octets() };
        // Copies everything it is given.
        let instance = unsafe {
            DnsServiceConstructInstance(
                name.as_ptr(),
                host.as_ptr(),
                std::ptr::null(),
                &ip6,
                port,
                0,
                0,
                txt.len() as u32,
                key_ptrs.as_ptr(),
                value_ptrs.as_ptr(),
            )
        };
        if instance.is_null() {
            return Err("DnsServiceConstructInstance failed".into());
        }
        let (tx, rx) = channel();
        let context = Box::into_raw(Box::new(tx));
        let request = Box::new(DNS_SERVICE_REGISTER_REQUEST {
            Version: DNS_QUERY_REQUEST_VERSION1,
            InterfaceIndex: index,
            pServiceInstance: instance,
            pRegisterCompletionCallback: Some(registered),
            pQueryContext: context.cast(),
            hCredentials: std::ptr::null_mut(),
            unicastEnabled: 0,
        });
        let registration = Self { request, instance, context };
        let status = unsafe { DnsServiceRegister(&*registration.request, std::ptr::null_mut()) };
        if status != DNS_REQUEST_PENDING as u32 {
            return Err(format!("DnsServiceRegister: {status}"));
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(0) => Ok(registration),
            Ok(status) => Err(format!("registration failed: {status}")),
            Err(_) => Err("no answer from DnsServiceRegister".into()),
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        unsafe {
            DnsServiceDeRegister(&*self.request, std::ptr::null_mut());
            DnsServiceFreeInstance(self.instance);
        }
        // The context leaks on purpose: a late callback may still use it.
        let _ = self.context;
    }
}

unsafe extern "system" fn registered(status: u32, context: *const c_void, instance: *const DNS_SERVICE_INSTANCE) {
    if !instance.is_null() {
        unsafe { DnsServiceFreeInstance(instance.cast_mut()) };
    }
    if !context.is_null() {
        let tx = unsafe { &*context.cast::<Sender<u32>>() };
        let _ = tx.send(status);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
