//! Lists the Apple BLE adverts this PC hears: each Continuity message in the
//! manufacturer data (company 0x004C), by type, with the sender's address and
//! signal, to compare Koala's own adverts with real ones.
//!
//! ```text
//! ble_watch [seconds] [minimum RSSI, default -60]
//! ```

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher, BluetoothLEScanningMode,
};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::DataReader;

const APPLE: u16 = 0x004c;

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(30);
    let min_rssi: i16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(-60);
    let seen = Arc::new(Mutex::new(HashSet::<String>::new()));

    let watcher = BluetoothLEAdvertisementWatcher::new()?;
    watcher.SetScanningMode(BluetoothLEScanningMode::Active)?;
    let handler =
        TypedEventHandler::new(move |_, args: windows::core::Ref<BluetoothLEAdvertisementReceivedEventArgs>| {
            let Some(args) = args.as_ref() else { return Ok(()) };
            let rssi = args.RawSignalStrengthInDBm()?;
            if rssi < min_rssi {
                return Ok(());
            }
            let address = args.BluetoothAddress()?;
            for data in args.Advertisement()?.ManufacturerData()? {
                if data.CompanyId()? != APPLE {
                    continue;
                }
                let buffer = data.Data()?;
                let reader = DataReader::FromBuffer(&buffer)?;
                let mut bytes = vec![0u8; buffer.Length()? as usize];
                reader.ReadBytes(&mut bytes)?;
                let line = format!("{address:012x} {rssi:>4} dBm  {}", describe(&bytes));
                // Each distinct advert once.
                if seen.lock().unwrap().insert(line.clone()) {
                    println!("{line}");
                }
            }
            Ok(())
        });
    watcher.Received(&handler)?;
    watcher.Start()?;
    std::thread::sleep(Duration::from_secs(seconds));
    watcher.Stop()?;
    Ok(())
}

/// The Continuity messages in Apple's manufacturer data: type, length, value.
fn describe(bytes: &[u8]) -> String {
    let mut out = Vec::new();
    let mut rest = bytes;
    while let [kind, len, tail @ ..] = rest {
        let len = (*len as usize).min(tail.len());
        let (value, tail) = tail.split_at(len);
        let hex: String = value.iter().map(|b| format!("{b:02x}")).collect();
        out.push(format!("{}={hex}", name(*kind)));
        rest = tail;
    }
    out.join(" ")
}

fn name(kind: u8) -> String {
    match kind {
        0x05 => "AirDrop".into(),
        0x07 => "ProximityPairing".into(),
        0x09 => "AirPlayTarget".into(),
        0x0a => "AirPlaySource".into(),
        0x0c => "Handoff".into(),
        0x0d => "TetheringTarget".into(),
        0x0e => "TetheringSource".into(),
        0x0f => "NearbyAction".into(),
        0x10 => "NearbyInfo".into(),
        0x12 => "FindMy".into(),
        other => format!("type{other:#04x}"),
    }
}
