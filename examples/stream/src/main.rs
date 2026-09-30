//! Example: connect to a Polar Verity Sense by device ID, print the available
//! stream settings for a measurement type, and stream samples for a while.
//!
//! Usage: `cargo run -p stream -- <device-id> [type]`
//!
//! The device ID is the 8-character code written on the device. The optional
//! type is one of `acc`, `gyro`, `mag`, or `ppi` (default `acc`).

use arctic::{MeasurementType, PolarSensor, StreamFrame};
use futures::stream::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let device_id = std::env::args()
        .nth(1)
        .expect("usage: stream <device-id> [acc|gyro|mag|ppi]");
    let ty = match std::env::args().nth(2).as_deref() {
        Some("gyro") => MeasurementType::Gyro,
        Some("mag") => MeasurementType::Mag,
        Some("ppi") => MeasurementType::Ppi,
        _ => MeasurementType::Acc,
    };

    let mut sensor = PolarSensor::new(device_id).await?;

    println!("Connecting...");
    sensor.connect().await?;
    println!("Connected.");

    // Discover the available settings and select one of each. PPI has no
    // settings, so the request returns an empty set.
    let mut settings = sensor.request_stream_settings(ty).await?;
    println!("Available sample rates (Hz): {:?}", settings.sample_rates());
    println!("Available resolutions (bits): {:?}", settings.resolutions());
    println!("Available ranges: {:?}", settings.ranges());
    println!("Available channels: {:?}", settings.channels());

    // Select the highest sampling rate, if any are offered.
    if let Some(&rate) = settings.sample_rates().iter().max() {
        settings.set_sample_rate(rate);
    }

    println!("Starting stream...");
    let mut stream = sensor.start_streaming(ty, settings).await?;

    let mut received = 0usize;
    while let Some(frame) = stream.next().await {
        match frame {
            Ok(StreamFrame::Acc(samples)) => {
                for sample in &samples {
                    println!(
                        "acc t={} x={} y={} z={}",
                        sample.timestamp, sample.x, sample.y, sample.z
                    );
                }
                received += samples.len();
            }
            Ok(StreamFrame::Gyro(samples)) => {
                for sample in &samples {
                    println!(
                        "gyro t={} x={:.3} y={:.3} z={:.3}",
                        sample.timestamp, sample.x, sample.y, sample.z
                    );
                }
                received += samples.len();
            }
            Ok(StreamFrame::Mag(samples)) => {
                for sample in &samples {
                    println!(
                        "mag t={} x={:.4} y={:.4} z={:.4} cal={:?}",
                        sample.timestamp, sample.x, sample.y, sample.z, sample.calibration
                    );
                }
                received += samples.len();
            }
            Ok(StreamFrame::Ppi(samples)) => {
                for sample in &samples {
                    println!(
                        "ppi t={} hr={} pp={} ms err={} ms blocker={} contact={}",
                        sample.timestamp,
                        sample.hr,
                        sample.pp_in_ms,
                        sample.pp_error_estimate,
                        sample.blocker,
                        sample.skin_contact
                    );
                }
                received += samples.len();
            }
            Err(why) => {
                eprintln!("Stream ended: {:?}", why);
                break;
            }
        }

        if received >= 200 {
            break;
        }
    }

    println!("Stopping stream...");
    sensor.stop_streaming(ty).await?;
    println!("Received {} sample(s).", received);

    Ok(())
}
