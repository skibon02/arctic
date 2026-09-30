//! # PMD
//!
//! Parsing of Polar Measurement Data (PMD) frames delivered over the PMD data
//! characteristic during online streaming.
//!
//! Each notification is a single PMD frame: a 10-byte header
//! (`[type(1)][timestamp(8)][frame_type(1)]`) followed by the frame payload.
//! Raw frames carry the samples directly; delta (compressed) frames carry a
//! reference sample followed by signed deltas, which are accumulated to
//! reconstruct the original samples.
//!
//! This module only decodes frame payloads into raw integer channel samples.
//! Unit conversion, factor scaling, and timestamp assignment are done by the
//! streaming layer.

use crate::polar_uuid::MeasurementType;
use crate::{Error, PolarResult};

/// Length of the PMD frame header in bytes
pub(crate) const FRAME_HEADER_LENGTH: usize = 10;
/// Bit set in the frame type byte for delta (compressed) frames
const DELTA_FRAME_BIT: u8 = 0x80;
/// Size of a single PPI sample in bytes
const PPI_SAMPLE_CHUNK: usize = 6;

/// A parsed PMD frame header and its payload.
pub(crate) struct PmdFrame<'a> {
    /// The measurement type of the frame
    pub(crate) measurement_type: MeasurementType,
    /// Timestamp of the last sample in the frame, in nanoseconds
    pub(crate) timestamp: u64,
    /// The frame type byte, with the delta bit stripped
    pub(crate) frame_type: u8,
    /// Whether the frame is delta (compressed) encoded
    pub(crate) is_compressed: bool,
    /// The frame payload following the header
    pub(crate) data_content: &'a [u8],
}

impl<'a> PmdFrame<'a> {
    /// Parses a PMD frame from a raw notification value.
    pub(crate) fn parse(data: &'a [u8]) -> PolarResult<PmdFrame<'a>> {
        if data.len() < FRAME_HEADER_LENGTH {
            return Err(Error::InvalidData);
        }
        let measurement_type = MeasurementType::from_id(data[0]).ok_or(Error::InvalidData)?;
        let timestamp = u64::from_le_bytes(data[1..9].try_into().unwrap());
        let frame_type_byte = data[9];

        Ok(PmdFrame {
            measurement_type,
            timestamp,
            frame_type: frame_type_byte & !DELTA_FRAME_BIT,
            is_compressed: frame_type_byte & DELTA_FRAME_BIT != 0,
            data_content: &data[FRAME_HEADER_LENGTH..],
        })
    }
}

/// A single pulse-to-pulse interval sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PpiSample {
    /// Sample timestamp in nanoseconds
    pub timestamp: u64,
    /// Heart rate in beats per minute
    pub hr: u8,
    /// Pulse-to-pulse interval in milliseconds
    pub pp_in_ms: u16,
    /// Error estimate of the PP interval in milliseconds
    pub pp_error_estimate: u16,
    /// Whether movement was detected during acquisition
    pub blocker: bool,
    /// Whether skin contact was present
    pub skin_contact: bool,
    /// Whether the skin contact flag is supported by the device
    pub skin_contact_supported: bool,
}

/// A single accelerometer sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccSample {
    /// Sample timestamp in nanoseconds
    pub timestamp: u64,
    /// X axis acceleration in milli-G
    pub x: i32,
    /// Y axis acceleration in milli-G
    pub y: i32,
    /// Z axis acceleration in milli-G
    pub z: i32,
}

/// A single gyroscope sample.
#[derive(Debug, Clone, PartialEq)]
pub struct GyroSample {
    /// Sample timestamp in nanoseconds
    pub timestamp: u64,
    /// X axis angular velocity in degrees per second
    pub x: f32,
    /// Y axis angular velocity in degrees per second
    pub y: f32,
    /// Z axis angular velocity in degrees per second
    pub z: f32,
}

/// A single magnetometer sample.
#[derive(Debug, Clone, PartialEq)]
pub struct MagSample {
    /// Sample timestamp in nanoseconds
    pub timestamp: u64,
    /// X axis magnetic field in Gauss
    pub x: f32,
    /// Y axis magnetic field in Gauss
    pub y: f32,
    /// Z axis magnetic field in Gauss
    pub z: f32,
    /// Calibration status reported by the device, if any
    pub calibration: Option<MagCalibration>,
}

/// Magnetometer calibration status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagCalibration {
    /// Calibration status unknown
    Unknown,
    /// Poor calibration
    Poor,
    /// Acceptable calibration
    Ok,
    /// Good calibration
    Good,
}

impl MagCalibration {
    /// Maps a raw calibration id to a status, if recognised.
    pub(crate) fn from_id(id: i32) -> Option<MagCalibration> {
        match id {
            0 => Some(MagCalibration::Unknown),
            1 => Some(MagCalibration::Poor),
            2 => Some(MagCalibration::Ok),
            3 => Some(MagCalibration::Good),
            _ => None,
        }
    }
}

/// Parses a raw PPI frame (frame type 0) into samples.
///
/// The frame timestamp is the time of the *last* sample; earlier samples are
/// spaced backward by their own PP intervals.
pub(crate) fn parse_ppi(data: &[u8], timestamp: u64) -> PolarResult<Vec<PpiSample>> {
    if data.is_empty() || !data.len().is_multiple_of(PPI_SAMPLE_CHUNK) {
        return Err(Error::InvalidData);
    }

    let mut samples: Vec<PpiSample> = data
        .as_chunks::<PPI_SAMPLE_CHUNK>()
        .0
        .iter()
        .map(|chunk| PpiSample {
            timestamp: 0,
            hr: chunk[0],
            pp_in_ms: u16::from_le_bytes([chunk[1], chunk[2]]),
            pp_error_estimate: u16::from_le_bytes([chunk[3], chunk[4]]),
            blocker: chunk[5] & 0x01 != 0,
            skin_contact: chunk[5] & 0x02 != 0,
            skin_contact_supported: chunk[5] & 0x04 != 0,
        })
        .collect();

    if timestamp != 0 {
        let mut current = timestamp;
        for sample in samples.iter_mut().rev() {
            sample.timestamp = current;
            current = current.saturating_sub(sample.pp_in_ms as u64 * 1_000_000);
        }
    }

    Ok(samples)
}

/// Decodes an accelerometer frame into raw channel samples.
///
/// Raw frames (types 0, 1, 2) carry 8/16/24-bit signed samples. Delta frames
/// (type 128) carry 16-bit deltas accumulated from a reference sample.
pub(crate) fn parse_acc(frame: &PmdFrame) -> PolarResult<Vec<[i32; 3]>> {
    let channels = 3;
    let raw = if frame.is_compressed {
        parse_delta_frames(frame.data_content, channels, 16)?
    } else {
        let sample_bytes = match frame.frame_type {
            0 => 1,
            1 => 2,
            2 => 3,
            _ => return Err(Error::InvalidData),
        };
        parse_raw_samples(frame.data_content, channels, sample_bytes)?
    };
    Ok(raw.into_iter().map(|s| [s[0], s[1], s[2]]).collect())
}

/// Decodes a gyroscope frame into raw channel samples.
///
/// The Verity Sense only sends gyroscope data as delta frames (type 128).
pub(crate) fn parse_gyro(frame: &PmdFrame) -> PolarResult<Vec<[i32; 3]>> {
    if !frame.is_compressed {
        return Err(Error::InvalidData);
    }
    let raw = parse_delta_frames(frame.data_content, 3, 16)?;
    Ok(raw.into_iter().map(|s| [s[0], s[1], s[2]]).collect())
}

/// Decodes a magnetometer frame into raw channel samples plus calibration.
///
/// The Verity Sense only sends magnetometer data as delta frames (type 128).
/// Type 0 deltas have 3 channels; type 1 deltas have 4 channels where the extra
/// channel is a calibration status.
pub(crate) fn parse_mag(frame: &PmdFrame) -> PolarResult<Vec<([i32; 3], Option<MagCalibration>)>> {
    if !frame.is_compressed {
        return Err(Error::InvalidData);
    }
    let channels = match frame.frame_type {
        0 => 3,
        1 => 4,
        _ => return Err(Error::InvalidData),
    };
    let raw = parse_delta_frames(frame.data_content, channels, 16)?;

    Ok(raw
        .into_iter()
        .map(|s| {
            let calibration = if channels == 4 {
                MagCalibration::from_id(s[3])
            } else {
                None
            };
            ([s[0], s[1], s[2]], calibration)
        })
        .collect())
}

/// Parses a raw (uncompressed) frame into channel samples.
///
/// `sample_bytes` is the width of each channel value in bytes.
fn parse_raw_samples(
    data: &[u8],
    channels: usize,
    sample_bytes: usize,
) -> PolarResult<Vec<Vec<i32>>> {
    let sample_size = channels * sample_bytes;
    if data.is_empty() || !data.len().is_multiple_of(sample_size) {
        return Err(Error::InvalidData);
    }

    let mut samples = Vec::with_capacity(data.len() / sample_size);
    for chunk in data.chunks_exact(sample_size) {
        let mut channels_out = Vec::with_capacity(channels);
        for channel in chunk.chunks_exact(sample_bytes) {
            channels_out.push(sign_extend(channel));
        }
        samples.push(channels_out);
    }
    Ok(samples)
}

/// Reconstructs samples from a delta frame payload.
///
/// The payload starts with a reference sample (one value per channel, each
/// `ceil(resolution/8)` bytes), followed by repeated blocks of
/// `[delta_size_bits(1)][sample_count(1)][deltas...]`. Each delta is a signed
/// value of `delta_size_bits` bits per channel; samples are reconstructed by
/// accumulating deltas onto the previous sample.
pub(crate) fn parse_delta_frames(
    data: &[u8],
    channels: usize,
    resolution: usize,
) -> PolarResult<Vec<Vec<i32>>> {
    let resolution_bytes = resolution.div_ceil(8);
    let ref_len = channels * resolution_bytes;
    if channels == 0 || data.len() < ref_len {
        return Err(Error::InvalidData);
    }

    let mut samples = Vec::new();
    let mut reference = Vec::with_capacity(channels);
    for channel in data[..ref_len].chunks_exact(resolution_bytes) {
        reference.push(sign_extend(channel));
    }
    samples.push(reference);

    let mut offset = ref_len;
    while offset < data.len() {
        if offset + 2 > data.len() {
            return Err(Error::InvalidData);
        }
        let delta_size = data[offset] as usize;
        let sample_count = data[offset + 1] as usize;
        offset += 2;

        if delta_size == 0 {
            continue;
        }

        let bit_length = sample_count * delta_size * channels;
        let byte_length = bit_length.div_ceil(8);
        if offset + byte_length > data.len() {
            return Err(Error::InvalidData);
        }

        let deltas = parse_delta_block(
            &data[offset..offset + byte_length],
            channels,
            delta_size,
            bit_length,
        );
        offset += byte_length;

        for delta in deltas {
            let last = samples.last().unwrap();
            let mut next = Vec::with_capacity(channels);
            for channel in 0..channels {
                next.push(last[channel].wrapping_add(delta[channel]));
            }
            samples.push(next);
        }
    }

    Ok(samples)
}

/// Parses a single delta block into per-sample channel deltas.
fn parse_delta_block(
    data: &[u8],
    channels: usize,
    bit_width: usize,
    total_bits: usize,
) -> Vec<Vec<i32>> {
    let bits: Vec<bool> = data
        .iter()
        .flat_map(|byte| (0..8).map(move |i| byte & (1 << i) != 0))
        .collect();

    let channel_bits = bit_width * channels;
    let mut samples = Vec::with_capacity(total_bits / channel_bits);
    let mut start = 0;
    while start + channel_bits <= total_bits && start + channel_bits <= bits.len() {
        let mut sample = Vec::with_capacity(channels);
        for channel in 0..channels {
            let offset = start + channel * bit_width;
            let mut value: i32 = 0;
            for (i, bit) in bits[offset..offset + bit_width].iter().enumerate() {
                if *bit {
                    value |= 1 << i;
                }
            }
            if bit_width < 32 && value & (1 << (bit_width - 1)) != 0 {
                value |= !0 << bit_width;
            }
            sample.push(value);
        }
        samples.push(sample);
        start += channel_bits;
    }
    samples
}

/// Interprets a little-endian byte slice as a sign-extended integer.
fn sign_extend(bytes: &[u8]) -> i32 {
    let mut value: i32 = 0;
    for (i, byte) in bytes.iter().enumerate() {
        value |= (*byte as i32) << (i * 8);
    }
    let bits = bytes.len() * 8;
    if bits < 32 && value & (1 << (bits - 1)) != 0 {
        value |= !0 << bits;
    }
    value
}

/// Computes per-sample timestamps for a frame.
///
/// The frame timestamp is the time of the last sample. When the sampling rate
/// is known, samples are spaced backward from it by `1/sample_rate`. When a
/// previous frame timestamp is available, the spacing is derived from the time
/// between the two frames instead.
pub(crate) fn compute_timestamps(
    previous_timestamp: u64,
    frame_timestamp: u64,
    sample_count: usize,
    sample_rate: u32,
) -> PolarResult<Vec<u64>> {
    if sample_count == 0 {
        return Ok(Vec::new());
    }

    let delta = if previous_timestamp == 0 || previous_timestamp >= frame_timestamp {
        if sample_rate == 0 {
            return Err(Error::InvalidData);
        }
        1_000_000_000.0 / sample_rate as f64
    } else {
        (frame_timestamp - previous_timestamp) as f64 / sample_count as f64
    };

    let start = frame_timestamp as f64 - delta * (sample_count - 1) as f64;
    if start < 0.0 {
        return Err(Error::InvalidData);
    }

    let mut timestamps = Vec::with_capacity(sample_count);
    for i in 0..sample_count - 1 {
        timestamps.push((start + delta * i as f64).round() as u64);
    }
    timestamps.push(frame_timestamp);
    Ok(timestamps)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parses_ppi_frame() {
        // hr=60, pp=1000ms, error=5ms, flags=0b111
        let frame = [60, 0xE8, 0x03, 0x05, 0x00, 0x07];
        let samples = parse_ppi(&frame, 5_000_000_000).unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].hr, 60);
        assert_eq!(samples[0].pp_in_ms, 1000);
        assert_eq!(samples[0].pp_error_estimate, 5);
        assert!(samples[0].blocker);
        assert!(samples[0].skin_contact);
        assert!(samples[0].skin_contact_supported);
        assert_eq!(samples[0].timestamp, 5_000_000_000);
    }

    #[test]
    fn ppi_timestamps_go_backward() {
        // Two samples: pp=1000ms then pp=500ms; last sample at t=2s.
        let frame = [
            60, 0xE8, 0x03, 0x00, 0x00, 0x00, // pp=1000ms
            70, 0xF4, 0x01, 0x00, 0x00, 0x00, // pp=500ms
        ];
        let samples = parse_ppi(&frame, 2_000_000_000).unwrap();
        assert_eq!(samples[1].timestamp, 2_000_000_000);
        assert_eq!(samples[0].timestamp, 1_500_000_000);
    }

    #[test]
    fn rejects_bad_ppi_length() {
        let frame = [60, 0xE8, 0x03, 0x05];
        assert!(parse_ppi(&frame, 0).is_err());
    }

    #[test]
    fn parses_raw_acc_type1() {
        // Two samples of 16-bit xyz: (100, -200, 300), (400, 500, -600).
        let mut data = Vec::new();
        for value in [100i16, -200, 300, 400, 500, -600] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let frame = PmdFrame {
            measurement_type: MeasurementType::Acc,
            timestamp: 0,
            frame_type: 1,
            is_compressed: false,
            data_content: &data,
        };
        let samples = parse_acc(&frame).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0], [100, -200, 300]);
        assert_eq!(samples[1], [400, 500, -600]);
    }

    #[test]
    fn parses_delta_frame() {
        // Reference sample (16-bit): x=0xFFD0(-48), y=0x0165(357), z=0x0FE4(4068).
        // Then delta_size=8, count=2, deltas: (0xFC=-4, 0x07=7, 0xFF=-1),
        // (0x0C=12, 0x13=19, 0xF2=-14).
        let data = [
            0xD0, 0xFF, 0x65, 0x01, 0xE4, 0x0F, // reference
            0x08, 0x02, // delta size 8 bits, 2 samples
            0xFC, 0x07, 0xFF, // delta 0
            0x0C, 0x13, 0xF2, // delta 1
        ];
        let samples = parse_delta_frames(&data, 3, 16).unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0], vec![-48, 357, 4068]);
        assert_eq!(samples[1], vec![-52, 364, 4067]);
        assert_eq!(samples[2], vec![-40, 383, 4053]);
    }

    #[test]
    fn computes_timestamps_from_sample_rate() {
        // 3 samples at 52 Hz, last at t=1s.
        let ts = compute_timestamps(0, 1_000_000_000, 3, 52).unwrap();
        assert_eq!(ts.len(), 3);
        assert_eq!(ts[2], 1_000_000_000);
        let step: f64 = 1_000_000_000.0 / 52.0;
        assert_eq!(ts[0], (1_000_000_000.0 - step * 2.0).round() as u64);
    }
}
