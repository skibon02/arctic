//! Example: connect to a Polar Verity Sense by device ID and print the
//! available offline recordings, then download and parse each one.
//!
//! Usage: `cargo run -p list-recordings -- <device-id>`
//!
//! The device ID is the 8-character code written on the device.

use arctic::PolarSensor;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let device_id = std::env::args()
        .nth(1)
        .expect("usage: list-recordings <device-id>");

    let mut sensor = PolarSensor::new(device_id).await?;

    println!("Connecting...");
    sensor.connect().await?;
    println!("Connected.");

    println!("Listing recordings...");
    let recordings = sensor.list_offline_recordings().await?;

    if recordings.is_empty() {
        println!("No recordings found.");
    } else {
        println!("Found {} recording(s):", recordings.len());
        for entry in &recordings {
            println!("  {} ({} bytes, {:?})", entry.path, entry.size, entry.ty);
        }

        // Download and parse each recording.
        for entry in &recordings {
            println!("\nDownloading {}...", entry.path);
            let ppi = sensor.get_offline_record(entry, None).await?;
            println!("  start time: {}", ppi.start_time);
            println!("  samples: {}", ppi.samples.len());
            for sample in ppi.samples.iter().take(10) {
                println!(
                    "    pp={} ms, hr={}, error={} ms, blocker={}",
                    sample.pp_in_ms, sample.hr, sample.pp_error_estimate, sample.blocker
                );
            }
            if ppi.samples.len() > 10 {
                println!("    ... ({} more samples)", ppi.samples.len() - 10);
            }
        }
    }

    Ok(())
}
