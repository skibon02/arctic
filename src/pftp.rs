//! # PFTP
//!
//! Polar Simple File Transfer Protocol (PS-FTP) client used to list and
//! download offline recordings from the device.
//!
//! Files are transferred over the MTU characteristic using RFC76 message
//! framing. Requests are protobuf-encoded `PbPFtpOperation` messages.

use crate::polar_uuid::{MeasurementType, PSFTP_D2H_UUID, PSFTP_MTU_UUID};
use crate::{find_characteristic, Error, OfflineRecord, PolarResult};

use btleplug::api::{Peripheral as _, WriteType};
use btleplug::platform::Peripheral;
use futures::stream::StreamExt;

/// RFC76 frame status bits
const STATUS_ERROR_OR_RESPONSE: u8 = 0x00;
const STATUS_LAST: u8 = 0x01;
const STATUS_MORE: u8 = 0x03;

/// Path of the file listing all offline recordings
const PMD_FILES_PATH: &str = "/PMDFILES.TXT";

/// Lists offline recordings by fetching and parsing the PMDFILES.TXT index.
pub(crate) async fn list_offline_recordings(
    device: &Peripheral,
) -> PolarResult<Vec<OfflineRecord>> {
    let data = get_file(device, PMD_FILES_PATH).await?;
    let text = String::from_utf8_lossy(&data);

    let mut records = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Each line: "<size> <path>"
        let mut parts = line.splitn(2, ' ');
        let size: u64 = match parts.next().and_then(|s| s.parse().ok()) {
            Some(s) => s,
            None => continue,
        };
        let path = match parts.next() {
            Some(p) => p.trim().to_string(),
            None => continue,
        };

        // Path format: /U/0/<date8>/R/<time6>/<TYPE><n>.REC
        let components: Vec<&str> = path.split('/').collect();
        if components.len() < 6 {
            continue;
        }
        let file_name = components[5];
        let prefix: String = file_name
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        let ty = match MeasurementType::from_file_prefix(&prefix) {
            Some(t) => t,
            None => continue,
        };

        records.push(OfflineRecord {
            path,
            size,
            ty,
        });
    }

    Ok(records)
}

/// Downloads a file from the device over PS-FTP.
pub(crate) async fn get_file(device: &Peripheral, path: &str) -> PolarResult<Vec<u8>> {
    let mtu = find_characteristic(device, PSFTP_MTU_UUID).await?;
    let d2h = find_characteristic(device, PSFTP_D2H_UUID).await?;

    device.subscribe(&mtu).await.map_err(Error::BleError)?;
    device.subscribe(&d2h).await.map_err(Error::BleError)?;

    // Build the protobuf PbPFtpOperation { command = GET (0), path = <path> }.
    let operation = encode_pftp_operation(path);
    // Prefix with RFC60 2-byte little-endian length.
    let mut message = Vec::with_capacity(2 + operation.len());
    message.push((operation.len() & 0xff) as u8);
    message.push(((operation.len() >> 8) & 0x7f) as u8);
    message.extend_from_slice(&operation);

    // Split the message into RFC76 frames and write them. The first frame has
    // the `next` bit clear; subsequent frames set it. The Polar PFTP MTU
    // characteristic expects write-without-response for these data frames.
    let mtu_size = mtu_size(device).await;
    let mut seq = 0u8;
    let mut offset = 0usize;
    let mut next = 0u8;
    loop {
        let remaining = message.len() - offset;
        let chunk_len = remaining.min(mtu_size - 1);
        let last = remaining < mtu_size;

        let mut frame = Vec::with_capacity(chunk_len + 1);
        let status = if last { STATUS_LAST } else { STATUS_MORE };
        frame.push((seq << 4) | (status << 1) | next);
        frame.extend_from_slice(&message[offset..offset + chunk_len]);

        device
            .write(&mtu, &frame, WriteType::WithoutResponse)
            .await
            .map_err(Error::BleError)?;

        offset += chunk_len;
        seq = (seq + 1) & 0x0F;
        next = 1;

        if last {
            break;
        }
    }

    // Read the response frames and reassemble the payload.
    let mut notifications = device.notifications().await.map_err(Error::BleError)?;
    let mut payload = Vec::new();

    loop {
        // Guard against a device that never responds: time out after 10s.
        let data = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            notifications.next(),
        )
        .await
        {
            Ok(Some(d)) if d.uuid == PSFTP_MTU_UUID => d.value,
            Ok(Some(_)) => continue,
            Ok(None) => return Err(Error::InvalidData),
            Err(_) => return Err(Error::InvalidData),
        };

        if data.is_empty() {
            return Err(Error::InvalidData);
        }

        let header = data[0];
        let status = (header >> 1) & 0x03;

        match status {
            STATUS_ERROR_OR_RESPONSE => {
                let code = if data.len() >= 3 {
                    data[1] as u16 | ((data[2] as u16) << 8)
                } else {
                    0
                };
                // error code 0 means the request succeeded.
                if code == 0 {
                    break;
                }
                return Err(Error::PftpError(code));
            }
            STATUS_LAST | STATUS_MORE => {
                if data.len() > 1 {
                    payload.extend_from_slice(&data[1..]);
                }
                if status == STATUS_LAST {
                    break;
                }
            }
            _ => return Err(Error::InvalidData),
        }
    }

    Ok(payload)
}

/// Encodes a `PbPFtpOperation` protobuf message with a GET command.
fn encode_pftp_operation(path: &str) -> Vec<u8> {
    // field 1 (command) = varint 0x08, value 0 (GET);
    // field 2 (path) = length-delimited 0x12, length, string bytes.
    let mut out = vec![0x08, 0x00, 0x12, path.len() as u8];
    out.extend_from_slice(path.as_bytes());
    out
}

/// Determines the ATT MTU size to use for framing.
async fn mtu_size(_device: &Peripheral) -> usize {
    // btleplug does not expose the negotiated MTU directly; use a conservative
    // value that works across platforms.
    23
}
