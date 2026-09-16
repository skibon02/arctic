//! Example: start offline PPI recording, then list and download recordings.
//!
//! Usage: `cargo run -p offline-ppi -- <device-id>`
//!
//! The device ID is the 8-character code written on the device.

use arctic::{MeasurementType, PolarSensor};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let device_id = std::env::args()
        .nth(1)
        .expect("usage: offline-ppi <device-id>");

    let mut sensor = PolarSensor::new(device_id).await?;

    println!("Connecting...");
    while !sensor.is_connected().await {
        match sensor.connect().await {
            Ok(()) => {}
            Err(arctic::Error::NoBleAdaptor) => {
                eprintln!("No bluetooth adapter found");
                return Ok(());
            }
            Err(why) => println!("Could not connect: {:?}", why),
        }
    }
    println!("Connected.");

    // Start recording PPI to the device's internal memory.
    println!("Starting offline PPI recording...");
    sensor
        .start_offline_recording(MeasurementType::Ppi, None)
        .await?;
    println!("Recording started. The device records while disconnected.");

    // In a real application you would disconnect here and let the device
    // record. For the example, stop immediately and download.
    println!("Stopping recording...");
    sensor.stop_offline_recording(MeasurementType::Ppi).await?;

    println!("Listing recordings...");
    let recordings = sensor.list_offline_recordings().await?;
    println!("Found {} recording(s).", recordings.len());

    for entry in &recordings {
        println!("  {} ({} bytes)", entry.path, entry.size);
        let ppi = sensor.get_offline_record(entry, None).await?;
        println!("  start time: {}", ppi.start_time);
        println!("  samples: {}", ppi.samples.len());
        for sample in ppi.samples.iter().take(10) {
            println!(
                "    pp={} ms, hr={}, error={} ms, blocker={}",
                sample.pp_in_ms, sample.hr, sample.pp_error_estimate, sample.blocker
            );
        }
    }

    Ok(())
}
