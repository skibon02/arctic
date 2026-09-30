//! # Offline
//!
//! Parsing of offline recording files downloaded from the device.
//!
//! An offline recording file starts with a security strategy byte, followed by
//! an (optionally encrypted) metadata block and a payload of PMD data frames.
//! Only PPI recordings are parsed here.

use crate::pmd::{self, PpiSample};
use crate::polar_uuid::MeasurementType;
use crate::{Error, PolarResult};

/// The magic value at the start of the offline recording header
const OFFLINE_HEADER_MAGIC: u32 = 0x3D7C_4C2B;
/// Length of the offline recording header in bytes
const OFFLINE_HEADER_LENGTH: usize = 16;
/// Length of the start time field in bytes
const DATE_TIME_LENGTH: usize = 20;
/// Size of the PMD data frame header prepended to each frame's samples
const PMD_FRAME_HEADER_LENGTH: usize = 10;

/// Security strategies supported for offline recordings
enum SecurityStrategy {
    None,
    Xor,
}

impl SecurityStrategy {
    fn from_byte(byte: u8) -> PolarResult<SecurityStrategy> {
        match byte {
            0 => Ok(SecurityStrategy::None),
            1 => Ok(SecurityStrategy::Xor),
            _ => Err(Error::InvalidRecording),
        }
    }
}

/// An entry in the device's list of offline recordings.
#[derive(Debug, Clone)]
pub struct OfflineRecord {
    /// Full path of the recording file on the device
    pub path: String,
    /// Size of the recording in bytes
    pub size: u64,
    /// The measurement type of the recording
    pub ty: MeasurementType,
}

/// A parsed offline recording of PPI data.
#[derive(Debug, Clone)]
pub struct PpiRecord {
    /// The recording start time as an ISO8601 string
    pub start_time: String,
    /// The PPI samples
    pub samples: Vec<PpiSample>,
}

/// Parses an offline recording file containing PPI data.
pub(crate) fn parse_ppi_record(data: &[u8], secret: Option<&[u8]>) -> PolarResult<PpiRecord> {
    if data.is_empty() {
        return Err(Error::InvalidRecording);
    }

    let strategy = SecurityStrategy::from_byte(data[0])?;
    let offset = 1;

    // The metadata (header, start time, settings, security info) is encrypted
    // with the secret. Decrypt it first.
    let metadata = match strategy {
        SecurityStrategy::None => data[offset..].to_vec(),
        SecurityStrategy::Xor => {
            let key = secret.ok_or(Error::InvalidRecording)?;
            if key.is_empty() {
                return Err(Error::InvalidRecording);
            }
            data[offset..].iter().map(|b| b ^ key[0]).collect()
        }
    };

    // Header
    if metadata.len() < OFFLINE_HEADER_LENGTH {
        return Err(Error::InvalidRecording);
    }
    let magic = u32::from_le_bytes(metadata[0..4].try_into().unwrap());
    if magic != OFFLINE_HEADER_MAGIC {
        return Err(Error::InvalidRecording);
    }
    let mut meta_offset = OFFLINE_HEADER_LENGTH;

    // Start time (20 bytes, ISO8601 with a space separator)
    if metadata.len() < meta_offset + DATE_TIME_LENGTH {
        return Err(Error::InvalidRecording);
    }
    let start_time_raw = &metadata[meta_offset..meta_offset + DATE_TIME_LENGTH];
    let start_time = String::from_utf8_lossy(start_time_raw)
        .replace(' ', "T")
        .trim_end_matches('\0')
        .to_string();
    meta_offset += DATE_TIME_LENGTH;

    // Settings (1-byte length + bytes)
    if metadata.len() < meta_offset + 1 {
        return Err(Error::InvalidRecording);
    }
    let settings_len = metadata[meta_offset] as usize;
    meta_offset += 1;
    meta_offset += settings_len;

    // Security info (1-byte length + bytes)
    if metadata.len() < meta_offset + 1 {
        return Err(Error::InvalidRecording);
    }
    let security_info_len = metadata[meta_offset] as usize;
    meta_offset += 1;
    meta_offset += security_info_len;

    // Packet size (2 bytes)
    if metadata.len() < meta_offset + 2 {
        return Err(Error::InvalidRecording);
    }
    let packet_size =
        u16::from_le_bytes(metadata[meta_offset..meta_offset + 2].try_into().unwrap()) as usize;
    meta_offset += 2;

    // The payload follows the metadata. The payload is also XOR-encrypted if a
    // secret was used.
    let payload = data[offset + meta_offset..].to_vec();
    let payload = match strategy {
        SecurityStrategy::None => payload,
        SecurityStrategy::Xor => {
            let key = secret.ok_or(Error::InvalidRecording)?;
            payload.iter().map(|b| b ^ key[0]).collect()
        }
    };

    // Parse PPI data frames. The first frame length is the packet size read
    // from the metadata; each subsequent frame is preceded by a 2-byte length.
    // Each frame begins with a 10-byte PMD header (measurement type, timestamp,
    // frame type) followed by the raw PPI samples.
    let mut samples = Vec::new();
    let mut pos = 0;
    let mut frame_size = packet_size;
    while pos < payload.len() && frame_size > 0 {
        let frame_end = (pos + frame_size).min(payload.len());
        let frame = &payload[pos..frame_end];
        if frame.len() > PMD_FRAME_HEADER_LENGTH {
            let timestamp = u64::from_le_bytes(frame[1..9].try_into().unwrap());
            samples.extend(pmd::parse_ppi(&frame[PMD_FRAME_HEADER_LENGTH..], timestamp)?);
        }
        pos = frame_end;

        // Read the next frame's length if present.
        if pos + 2 <= payload.len() {
            frame_size = u16::from_le_bytes(payload[pos..pos + 2].try_into().unwrap()) as usize;
            pos += 2;
        } else {
            break;
        }
    }

    Ok(PpiRecord { start_time, samples })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parses_ppi_frame() {
        // hr=60, pp=1000ms, error=5ms, flags=0b111 (blocker + contact + supported)
        let frame = [60, 0xE8, 0x03, 0x05, 0x00, 0x07];
        let samples = pmd::parse_ppi(&frame, 0).unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].hr, 60);
        assert_eq!(samples[0].pp_in_ms, 1000);
        assert_eq!(samples[0].pp_error_estimate, 5);
        assert!(samples[0].blocker);
        assert!(samples[0].skin_contact);
        assert!(samples[0].skin_contact_supported);
    }

    #[test]
    fn rejects_bad_frame_length() {
        let frame = [60, 0xE8, 0x03, 0x05];
        assert!(pmd::parse_ppi(&frame, 0).is_err());
    }
}
