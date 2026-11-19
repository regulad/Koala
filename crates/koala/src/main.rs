//! Koala: an iPhone as a Windows microphone (PLAN.md).
//!
//! For now, one command, the K2 trial:
//!
//! ```text
//! koala guest
//! ```
//!
//! It plays an Apple TV offering guest pairing (`docs/guest-pairing.md`):
//! shows a QR code for the iPhone's camera, advertises `_rdlink._tcp` over
//! AWDL through Warlock, and pairs with the iPhone that connects, printing
//! every frame. It needs Warlock running, and Administrator rights or
//! membership of `warlock-users`.

mod ble;
mod capture;
mod dnssd;
mod guest;
mod hci;
mod nearby;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["guest"] => guest::run(600).await,
        ["guest", seconds] if seconds.parse::<u64>().is_ok() => guest::run(seconds.parse().unwrap()).await,
        _ => {
            eprintln!("usage: koala guest [seconds to wait, default 600]");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("koala: {e}");
        std::process::exit(1);
    }
}

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
