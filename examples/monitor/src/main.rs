//! Example: scan for the first Polar Verity Sense, connect, and stream every
//! requested measurement type concurrently until interrupted.
//!
//! All selected types stream at the same time; each stream yields only its own
//! frames. For every elapsed second:
//!
//! * PPI samples are printed individually,
//! * ACC samples are averaged,
//! * MAG samples are averaged,
//! * GYRO samples are reported as per-axis minimum and maximum.
//!
//! Press Ctrl+C to stop streaming and disconnect.
//!
//! Usage: `cargo run -p monitor [-- --once] [--phase <secs>] [--types <list>]`
//!
//! * `--once` streams for one phase and exits, instead of looping.
//! * `--phase <secs>` sets how long to stream (default 10).
//! * `--types <list>` selects the types to stream, comma-separated
//!   (default `ppi,acc,mag,gyro`).

use arctic::{MeasurementType, PolarSensor, StreamFrame};
use futures::stream::{BoxStream, StreamExt};
use std::time::Duration;

/// Scan for at most this long before giving up.
const SCAN_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut once = false;
    let mut phase_duration = Duration::from_secs(10);
    let mut types = vec![
        MeasurementType::Ppi,
        MeasurementType::Acc,
        MeasurementType::Mag,
        MeasurementType::Gyro,
    ];

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--once" => once = true,
            "--phase" => {
                i += 1;
                let secs: u64 = args.get(i).expect("--phase needs a value").parse()?;
                phase_duration = Duration::from_secs(secs);
            }
            "--types" => {
                i += 1;
                let list = args.get(i).expect("--types needs a value");
                types = list.split(',').map(parse_type).collect();
            }
            other => panic!("unknown argument: {other}"),
        }
        i += 1;
    }

    // A placeholder id is required to construct the sensor; `discover` ignores
    // it and looks for the first Verity Sense instead.
    let mut sensor = PolarSensor::new("00000000".to_string()).await?;

    println!("Scanning for a Polar Verity Sense (max {}s)...", SCAN_TIMEOUT.as_secs());
    sensor.discover(SCAN_TIMEOUT).await?;
    println!("Connected.");

    loop {
        let interrupted = match stream_phase(&sensor, &types, phase_duration).await {
            Ok(interrupted) => interrupted,
            Err(why) => {
                eprintln!("Stream error: {:?}", why);
                false
            }
        };
        if interrupted {
            println!("\nInterrupted. Disconnecting...");
            sensor.disconnect().await;
            return Ok(());
        }
        if once {
            break;
        }
    }

    println!("\nDone. Disconnecting...");
    sensor.disconnect().await;
    Ok(())
}

/// Parses a measurement type name.
fn parse_type(name: &str) -> MeasurementType {
    match name.trim() {
        "ppi" => MeasurementType::Ppi,
        "acc" => MeasurementType::Acc,
        "mag" => MeasurementType::Mag,
        "gyro" => MeasurementType::Gyro,
        other => panic!("unknown measurement type: {other}"),
    }
}

/// Streams every requested type concurrently for `phase_duration` or until
/// Ctrl+C.
///
/// Returns whether the streams were interrupted by Ctrl+C.
async fn stream_phase(
    sensor: &PolarSensor,
    types: &[MeasurementType],
    phase_duration: Duration,
) -> arctic::PolarResult<bool> {
    let mut streams: Vec<(MeasurementType, BoxStream<'static, arctic::PolarResult<StreamFrame>>)> =
        Vec::new();

    for &ty in types {
        let mut settings = sensor.request_stream_settings(ty).await?;
        println!(
            "settings {:?}: rates={:?} resolutions={:?} ranges={:?} channels={:?}",
            ty,
            settings.sample_rates(),
            settings.resolutions(),
            settings.ranges(),
            settings.channels()
        );
        settings.select_max();
        let stream = sensor.start_streaming(ty, settings).await?;
        streams.push((ty, stream));
    }

    let deadline = tokio::time::Instant::now() + phase_duration;
    let mut window = Window::new();
    let mut interrupted = false;

    // The signal future must be created once and kept alive across loop
    // iterations, otherwise a signal delivered between iterations is missed.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let sleep = tokio::time::sleep_until(deadline);
    tokio::pin!(sleep);

    loop {
        // Poll every stream for the next frame. `next()` is cancel-safe, so a
        // frame buffered in a stream that loses the race is not lost.
        let next = futures::future::select_all(
            streams.iter_mut().map(|(_, stream)| stream.next()),
        );

        tokio::select! {
            _ = &mut ctrl_c => {
                interrupted = true;
                break;
            }
            _ = &mut sleep => {
                eprintln!("arctic: phase deadline reached");
                break;
            }
            (frame, index, _) = next => {
                let (ty, _) = &streams[index];
                match frame {
                    Some(Ok(frame)) => window.push(frame),
                    Some(Err(why)) => {
                        eprintln!("arctic: {ty:?} stream error: {why:?}");
                        break;
                    }
                    None => {
                        eprintln!("arctic: {ty:?} stream ended (None)");
                        break;
                    }
                }
            }
        }
    }

    window.flush();

    // Stop each type independently; the others are unaffected.
    for (ty, _) in &streams {
        if let Err(why) = sensor.stop_streaming(*ty).await {
            eprintln!("arctic: stop {ty:?} failed: {why:?}");
        }
    }

    Ok(interrupted)
}

/// Accumulates samples and reports an aggregate once per second.
#[derive(Default)]
struct Window {
    acc: Vec<[f64; 3]>,
    mag: Vec<[f64; 3]>,
    gyro_min: Option<[f32; 3]>,
    gyro_max: Option<[f32; 3]>,
    last_report: Option<tokio::time::Instant>,
}

impl Window {
    fn new() -> Window {
        Window {
            last_report: Some(tokio::time::Instant::now()),
            ..Default::default()
        }
    }

    /// Adds a frame's samples and reports if a second has elapsed.
    fn push(&mut self, frame: StreamFrame) {
        match frame {
            StreamFrame::Ppi(samples) => {
                for sample in samples {
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
            }
            StreamFrame::Acc(samples) => {
                self.acc
                    .extend(samples.iter().map(|s| [s.x as f64, s.y as f64, s.z as f64]));
            }
            StreamFrame::Mag(samples) => {
                self.mag
                    .extend(samples.iter().map(|s| [s.x as f64, s.y as f64, s.z as f64]));
            }
            StreamFrame::Gyro(samples) => {
                for sample in samples {
                    let value = [sample.x, sample.y, sample.z];
                    self.gyro_min = Some(match self.gyro_min {
                        Some(min) => [min[0].min(value[0]), min[1].min(value[1]), min[2].min(value[2])],
                        None => value,
                    });
                    self.gyro_max = Some(match self.gyro_max {
                        Some(max) => [max[0].max(value[0]), max[1].max(value[1]), max[2].max(value[2])],
                        None => value,
                    });
                }
            }
        }

        if self.last_report.map(|t| t.elapsed() >= Duration::from_secs(1)).unwrap_or(false) {
            self.flush();
            self.last_report = Some(tokio::time::Instant::now());
        }
    }

    /// Reports the aggregates collected so far and resets the window.
    fn flush(&mut self) {
        if !self.acc.is_empty() {
            let avg = average(&self.acc);
            println!(
                "acc avg  x={:.1} y={:.1} z={:.1} mG ({} samples)",
                avg[0],
                avg[1],
                avg[2],
                self.acc.len()
            );
            self.acc.clear();
        }
        if !self.mag.is_empty() {
            let avg = average(&self.mag);
            println!(
                "mag avg  x={:.4} y={:.4} z={:.4} G ({} samples)",
                avg[0],
                avg[1],
                avg[2],
                self.mag.len()
            );
            self.mag.clear();
        }
        if let (Some(min), Some(max)) = (self.gyro_min, self.gyro_max) {
            println!(
                "gyro min x={:.3} y={:.3} z={:.3} dps | max x={:.3} y={:.3} z={:.3} dps",
                min[0], min[1], min[2], max[0], max[1], max[2]
            );
            self.gyro_min = None;
            self.gyro_max = None;
        }
    }
}

/// Averages each component of a set of samples.
fn average(samples: &[[f64; 3]]) -> [f64; 3] {
    let mut sum = [0.0f64; 3];
    for sample in samples {
        for i in 0..3 {
            sum[i] += sample[i];
        }
    }
    let count = samples.len() as f64;
    [sum[0] / count, sum[1] / count, sum[2] / count]
}
