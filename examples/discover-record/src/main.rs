//! Example: discover the first Polar Verity Sense and ensure an offline PPI
//! recording is running.
//!
//! If no recording is active, one is started. If a recording is already active,
//! it is stopped first and the result is reported.
//!
//! Unlike the `offline-ppi` example, this does not require a device ID. It
//! scans for any device advertising the name "Polar Sense" and connects to the
//! first one found.
//!
//! Usage: `cargo run -p discover-record`

use arctic::{MeasurementType, PolarSensor};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A placeholder id is required to construct the sensor; `discover` ignores
    // it and looks for the first Verity Sense instead.
    let mut sensor = PolarSensor::new("00000000".to_string()).await?;

    println!("Scanning for a Polar Verity Sense...");
    sensor.discover().await?;
    println!("Connected to a Verity Sense.");

    if sensor.is_offline_recording_active(MeasurementType::Ppi).await? {
        println!("An offline PPI recording is already active; stopping it...");
        match sensor.stop_offline_recording(MeasurementType::Ppi).await {
            Ok(()) => println!("Recording stopped successfully."),
            Err(why) => println!("Failed to stop recording: {:?}", why),
        }
    } else {
        println!("No offline PPI recording is active; starting one...");
        sensor
            .start_offline_recording(MeasurementType::Ppi, None)
            .await?;
        println!("Recording started. The device records while disconnected.");
    }

    Ok(())
}
