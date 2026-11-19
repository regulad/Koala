//! Bluetooth LE adverts in Apple's Continuity format, through Windows' own
//! publisher (WinRT `BluetoothLEAdvertisementPublisher`).
//!
//! A tvOS host asks a paired iPhone onto AWDL with a Nearby Action advert of
//! action type 0x32 (`docs/guest-pairing.md`, "Phase B"); Koala, with no
//! shared Wi-Fi, has only this way.

use windows::Devices::Bluetooth::Advertisement::{BluetoothLEAdvertisementPublisher, BluetoothLEManufacturerData};
use windows::Storage::Streams::DataWriter;

/// Apple's Bluetooth company identifier.
pub const APPLE: u16 = 0x004c;

/// An advert being broadcast; it stops when dropped.
pub struct Advert {
    publisher: BluetoothLEAdvertisementPublisher,
}

impl Advert {
    /// Broadcasts `data` as Apple manufacturer data (after the company ID).
    pub fn start(data: &[u8]) -> windows::core::Result<Self> {
        let manufacturer = BluetoothLEManufacturerData::new()?;
        manufacturer.SetCompanyId(APPLE)?;
        let writer = DataWriter::new()?;
        writer.WriteBytes(data)?;
        manufacturer.SetData(&writer.DetachBuffer()?)?;
        let publisher = BluetoothLEAdvertisementPublisher::new()?;
        publisher.Advertisement()?.ManufacturerData()?.Append(&manufacturer)?;
        publisher.Start()?;
        Ok(Self { publisher })
    }
}

impl Drop for Advert {
    fn drop(&mut self) {
        let _ = self.publisher.Stop();
    }
}
