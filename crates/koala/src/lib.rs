//! Koala: an iPhone as a Windows microphone (PLAN.md).
//!
//! Two programs share this library:
//! - `koala`, the console tool for trials (`koala guest …`);
//! - `koala-tray`, the notification-area app ([`tray`]).
//!
//! Both play an Apple TV offering guest pairing (`docs/guest-pairing.md`):
//! a QR code for the iPhone's camera, `_rdlink._tcp` over AWDL through
//! Warlock, and the capture session that follows. They need Warlock running,
//! and Administrator rights or membership of `warlock-users`.

pub mod camera;
pub mod capture;
pub mod decode;
pub mod diagnostics;
mod dnssd;
pub mod guest;
pub mod mic;
pub mod mkv;
pub mod settings;
pub mod tray;
pub mod usb_mics;
pub mod vcam;

/// A random (version 4) UUID, as `-[NSUUID UUIDString]` writes one:
/// uppercase hexadecimal with hyphens.
pub fn uuid() -> String {
    let mut b = [0u8; 16];
    random(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

pub fn random(buf: &mut [u8]) {
    getrandom::fill(buf).expect("the system's random number generator failed");
}

#[cfg(test)]
mod tests {
    #[test]
    fn uuids_look_like_nsuuids() {
        let u = super::uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u.as_bytes()[14], b'4');
        assert!(u.chars().all(|c| c == '-' || c.is_ascii_digit() || c.is_ascii_uppercase()));
        assert_ne!(u, super::uuid());
    }
}
