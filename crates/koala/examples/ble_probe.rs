//! Can Windows broadcast Apple-format BLE data? Publishes manufacturer data
//! with Apple's company ID (0x004C) for a while, printing the publisher's
//! status, so the iPhone's log can say whether it heard it.
//!
//! ```text
//! ble_probe [seconds] [hex payload after the company ID]
//! ```
//!
//! The default payload is a Continuity Nearby Action (type 0x0F, length 5):
//! flags 0, action type 0x32 (an RDLink "needs AWDL" request), a zero auth
//! tag. Without the right auth tag the iPhone can't identify the sender, but
//! its log still shows whether the advert arrives.

use std::time::{Duration, Instant};

use windows::Devices::Bluetooth::Advertisement::{BluetoothLEAdvertisementPublisher, BluetoothLEManufacturerData};
use windows::Storage::Streams::DataWriter;

const APPLE: u16 = 0x004c;

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(30);
    let payload: Vec<u8> = match args.get(1) {
        Some(hex) => (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex")).collect(),
        None => vec![0x0f, 0x05, 0x00, 0x32, 0x00, 0x00, 0x00],
    };

    let data = BluetoothLEManufacturerData::new()?;
    data.SetCompanyId(APPLE)?;
    let writer = DataWriter::new()?;
    writer.WriteBytes(&payload)?;
    data.SetData(&writer.DetachBuffer()?)?;

    let publisher = BluetoothLEAdvertisementPublisher::new()?;
    publisher.Advertisement()?.ManufacturerData()?.Append(&data)?;
    publisher.StatusChanged(&windows::Foundation::TypedEventHandler::new(
        |_,
         args: windows::core::Ref<
            windows::Devices::Bluetooth::Advertisement::BluetoothLEAdvertisementPublisherStatusChangedEventArgs,
        >| {
            if let Some(args) = args.as_ref() {
                println!("status {:?}, error {:?}", args.Status()?, args.Error()?);
            }
            Ok(())
        },
    ))?;
    let hex: String = payload.iter().map(|b| format!("{b:02x}")).collect();
    println!("publishing company 0x{APPLE:04x}, data {hex}, for {seconds} s");
    publisher.Start()?;
    let started = Instant::now();
    // A third argument: data to swap in halfway, while the advert runs, to see
    // whether Windows updates it in place (same address) or not at all.
    let mut update: Option<Vec<u8>> = args
        .get(2)
        .map(|hex| (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex")).collect());
    while started.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_secs(2));
        if started.elapsed() >= Duration::from_secs(seconds / 2)
            && let Some(new) = update.take()
        {
            let manufacturer = publisher.Advertisement()?.ManufacturerData()?;
            manufacturer.Clear()?;
            let data = BluetoothLEManufacturerData::new()?;
            data.SetCompanyId(APPLE)?;
            let writer = DataWriter::new()?;
            writer.WriteBytes(&new)?;
            data.SetData(&writer.DetachBuffer()?)?;
            match manufacturer.Append(&data) {
                Ok(()) => println!("swapped the data while running"),
                Err(e) => println!("the swap failed: {e}"),
            }
        }
        println!("{:>3} s: {:?}", started.elapsed().as_secs(), publisher.Status()?);
    }
    publisher.Stop()?;
    Ok(())
}
