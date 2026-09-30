//! # Stream
//!
//! Online streaming control flow.
//!
//! A stream is started by subscribing to the PMD data characteristic, sending
//! the start command with the selected settings, and then feeding parsed frames
//! into a channel. The returned stream is the receiving end of that channel.
//!
//! The stream ends, with an error, when:
//!
//! * the device disconnects,
//! * the device stops the measurement on its own,
//! * a frame cannot be parsed, or
//! * the consumer drops the stream (in which case the device is told to stop).

use crate::connection::Connection;
use crate::control;
use crate::pmd::{self, AccSample, GyroSample, MagSample, PmdFrame, PpiSample};
use crate::polar_uuid::{MeasurementType, PMD_CP_UUID, PMD_DATA_UUID};
use crate::settings::StreamSettings;
use crate::{Error, PolarResult};

use futures::stream::{BoxStream, StreamExt};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// A batch of parsed samples delivered by a stream.
#[derive(Debug, Clone)]
pub enum StreamFrame {
    /// A batch of pulse-to-pulse interval samples
    Ppi(Vec<PpiSample>),
    /// A batch of accelerometer samples
    Acc(Vec<AccSample>),
    /// A batch of gyroscope samples
    Gyro(Vec<GyroSample>),
    /// A batch of magnetometer samples
    Mag(Vec<MagSample>),
}

impl StreamFrame {
    /// The measurement type of the frame.
    pub fn measurement_type(&self) -> MeasurementType {
        match self {
            StreamFrame::Ppi(_) => MeasurementType::Ppi,
            StreamFrame::Acc(_) => MeasurementType::Acc,
            StreamFrame::Gyro(_) => MeasurementType::Gyro,
            StreamFrame::Mag(_) => MeasurementType::Mag,
        }
    }
}

/// Per-measurement-type parsing state carried across frames.
///
/// Sample timestamps are computed relative to the previous frame's timestamp,
/// so the last timestamp seen for each type and frame type must be retained.
#[derive(Default)]
struct StreamState {
    previous_timestamps: HashMap<(MeasurementType, u8), u64>,
}

impl StreamState {
    /// Computes per-sample timestamps and records this frame's timestamp as the
    /// previous timestamp for the next frame of the same type and frame type.
    fn timestamps(
        &mut self,
        ty: MeasurementType,
        frame_type: u8,
        frame_timestamp: u64,
        sample_count: usize,
        sample_rate: u32,
    ) -> PolarResult<Vec<u64>> {
        let key = (ty, frame_type);
        let previous = self.previous_timestamps.get(&key).copied().unwrap_or(0);
        let timestamps =
            pmd::compute_timestamps(previous, frame_timestamp, sample_count, sample_rate)?;
        self.previous_timestamps.insert(key, frame_timestamp);
        Ok(timestamps)
    }
}

/// Starts online streaming and returns a stream of parsed frames.
pub(crate) async fn start(
    conn: &Connection,
    ty: MeasurementType,
    mut settings: StreamSettings,
) -> PolarResult<BoxStream<'static, PolarResult<StreamFrame>>> {
    let mut disconnected = conn.disconnected();

    control::start_online_streaming(conn, ty, &mut settings).await?;

    // Subscribe to the data and control point characteristics after starting.
    // The start command registers its own routes for these characteristics and
    // drops their receivers on return, so subscribing afterwards is required to
    // receive frames and device-initiated stop commands.
    let mut data_rx = conn.subscribe(PMD_DATA_UUID).await?;
    let mut cp_rx = conn.subscribe(PMD_CP_UUID).await?;

    let sample_rate = settings.selected_sample_rate();
    let factor = settings.factor();

    let (tx, rx) = mpsc::channel(16);
    let conn = conn.clone();

    log::debug!("arctic: stream {ty:?} task started");
    tokio::spawn(async move {
        let mut state = StreamState::default();

        loop {
            let value = tokio::select! {
                _ = disconnected.recv() => {
                    log::debug!("arctic: stream {ty:?} ended: device disconnected");
                    let _ = tx.send(Err(Error::Disconnected)).await;
                    return;
                }
                Some(data) = cp_rx.recv() => {
                    // The device can stop the measurement on its own.
                    if let Some(stopped) = control::parse_online_measurement_stopped(&data) {
                        if stopped == ty {
                            let _ = tx.send(Err(Error::StreamStopped)).await;
                            return;
                        }
                    }
                    continue;
                }
                value = data_rx.recv() => match value {
                    Some(value) => value,
                    None => {
                        log::debug!("arctic: stream {ty:?} ended: data channel closed");
                        let _ = tx.send(Err(Error::Disconnected)).await;
                        return;
                    }
                },
            };

            let frame = match PmdFrame::parse(&value) {
                Ok(frame) => frame,
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                    return;
                }
            };

            // Only frames of the requested type belong to this stream.
            if frame.measurement_type != ty {
                continue;
            }

            match parse_frame(&frame, &mut state, sample_rate, factor) {
                Ok(Some(frame)) => {
                    if tx.send(Ok(frame)).await.is_err() {
                        // The consumer dropped the stream; stop the device.
                        let _ = control::stop_measurement(&conn, ty).await;
                        return;
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                    return;
                }
            }
        }
    });

    Ok(ReceiverStream::new(rx).boxed())
}

/// Parses a single frame into a [`StreamFrame`] batch.
fn parse_frame(
    frame: &PmdFrame,
    state: &mut StreamState,
    sample_rate: u32,
    factor: f32,
) -> PolarResult<Option<StreamFrame>> {
    let batch = match frame.measurement_type {
        MeasurementType::Ppi => {
            StreamFrame::Ppi(pmd::parse_ppi(frame.data_content, frame.timestamp)?)
        }
        MeasurementType::Acc => {
            let raw = pmd::parse_acc(frame)?;
            let timestamps =
                state.timestamps(frame.measurement_type, frame.frame_type, frame.timestamp, raw.len(), sample_rate)?;
            // Raw ACC is already in milli-G. Compressed type 0 arrives in G and
            // is scaled to milli-G; compressed type 1 arrives in milli-G.
            let scale = match (frame.is_compressed, frame.frame_type) {
                (true, 0) => factor * 1000.0,
                (true, _) => factor,
                (false, _) => 1.0,
            };
            StreamFrame::Acc(
                raw.into_iter()
                    .zip(timestamps)
                    .map(|([x, y, z], timestamp)| AccSample {
                        timestamp,
                        x: (x as f32 * scale) as i32,
                        y: (y as f32 * scale) as i32,
                        z: (z as f32 * scale) as i32,
                    })
                    .collect(),
            )
        }
        MeasurementType::Gyro => {
            let raw = pmd::parse_gyro(frame)?;
            let timestamps =
                state.timestamps(frame.measurement_type, frame.frame_type, frame.timestamp, raw.len(), sample_rate)?;
            StreamFrame::Gyro(
                raw.into_iter()
                    .zip(timestamps)
                    .map(|([x, y, z], timestamp)| GyroSample {
                        timestamp,
                        x: x as f32 * factor,
                        y: y as f32 * factor,
                        z: z as f32 * factor,
                    })
                    .collect(),
            )
        }
        MeasurementType::Mag => {
            let raw = pmd::parse_mag(frame)?;
            let timestamps =
                state.timestamps(frame.measurement_type, frame.frame_type, frame.timestamp, raw.len(), sample_rate)?;
            // Type 1 MAG deltas are in milli-Gauss; type 0 are in Gauss.
            let scale = if frame.frame_type == 1 { factor / 1000.0 } else { factor };
            StreamFrame::Mag(
                raw.into_iter()
                    .zip(timestamps)
                    .map(|(([x, y, z], calibration), timestamp)| MagSample {
                        timestamp,
                        x: x as f32 * scale,
                        y: y as f32 * scale,
                        z: z as f32 * scale,
                        calibration,
                    })
                    .collect(),
            )
        }
        MeasurementType::Ppg => return Ok(None),
    };

    Ok(Some(batch))
}
