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

/// Root directory holding offline recordings on the device
const RECORDINGS_ROOT: &str = "/U/0/";

/// Path of the LED configuration file on the device
const LED_CONFIG_PATH: &str = "/LEDCFG.BIN";

/// Writes the LED configuration to the device, disabling the PPI-mode LED.
///
/// The config file holds two bytes: the SDK-mode LED state and the PPI-mode LED
/// state. The SDK-mode LED is left enabled while the PPI-mode LED is disabled
/// so it does not blink during PPI measurements.
pub(crate) async fn disable_ppi_led(device: &Peripheral) -> PolarResult<()> {
    let contents = [0x01, 0x00];
    put_file(device, LED_CONFIG_PATH, &contents).await
}

/// Lists offline recordings by walking the device's directory tree.
///
/// The Verity Sense stores recordings under `/U/0/<date8>/R/<time6>/<TYPE><n>.REC`.
/// Each directory level is read with a PFTP GET that returns a protobuf
/// `PbPFtpDirectory` listing its entries.
pub(crate) async fn list_offline_recordings(
    device: &Peripheral,
) -> PolarResult<Vec<OfflineRecord>> {
    let mut records = Vec::new();

    // A device with no recordings reports error 103 (NO_SUCH_FILE_OR_DIRECTORY)
    // for the root directory, which we treat as an empty list.
    let dates = match list_directory(device, RECORDINGS_ROOT).await {
        Ok(entries) => entries,
        Err(Error::PftpError(103)) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    for (date, _) in dates {
        // Skip entries that are not 8-digit date directories.
        if date.len() != 9 || !date.ends_with('/') {
            continue;
        }

        let date_path = format!("{}{}", RECORDINGS_ROOT, date);
        let subs = list_directory(device, &date_path).await?;
        for (sub, _) in subs {
            if sub != "R/" {
                continue;
            }

            let time_path = format!("{}{}", date_path, sub);
            let times = list_directory(device, &time_path).await?;
            for (time, _) in times {
                if time.len() != 7 || !time.ends_with('/') {
                    continue;
                }

                let rec_path = format!("{}{}", time_path, time);
                let files = list_directory(device, &rec_path).await?;
                for (name, size) in files {
                    if !name.ends_with(".REC") {
                        continue;
                    }
                    let prefix: String =
                        name.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
                    let ty = match MeasurementType::from_file_prefix(&prefix) {
                        Some(t) => t,
                        None => continue,
                    };

                    records.push(OfflineRecord {
                        path: format!("{}{}", rec_path, name),
                        size,
                        ty,
                    });
                }
            }
        }
    }

    Ok(records)
}

/// Lists the entries of a directory via a PFTP GET and parses the returned
/// `PbPFtpDirectory` protobuf into `(name, size)` pairs.
async fn list_directory(device: &Peripheral, path: &str) -> PolarResult<Vec<(String, u64)>> {
    let data = get_file(device, path).await?;
    parse_directory(&data)
}

/// Parses a `PbPFtpDirectory` protobuf message.
///
/// The message is a repeated field of `PbPFtpEntry` messages (field 1,
/// length-delimited). Each entry has a name (field 1, length-delimited) and a
/// size (field 2, varint).
fn parse_directory(data: &[u8]) -> PolarResult<Vec<(String, u64)>> {
    let mut entries = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        // Each entry is wrapped in a field 1 length-delimited tag.
        if data[pos] != 0x0A {
            return Err(Error::InvalidData);
        }
        pos += 1;
        let (len, next) = read_varint(data, pos)?;
        pos = next;
        let end = pos + len as usize;
        if end > data.len() {
            return Err(Error::InvalidData);
        }

        entries.push(parse_entry(&data[pos..end])?);
        pos = end;
    }

    Ok(entries)
}

/// Parses a single `PbPFtpEntry` message into a `(name, size)` pair.
fn parse_entry(data: &[u8]) -> PolarResult<(String, u64)> {
    let mut name = String::new();
    let mut size = 0u64;
    let mut cursor = 0;

    while cursor < data.len() {
        let tag = data[cursor];
        let field = tag >> 3;
        let wire_type = tag & 0x07;
        cursor += 1;

        match (field, wire_type) {
            // name (field 1, length-delimited)
            (1, 2) => {
                let (len, next) = read_varint(data, cursor)?;
                cursor = next;
                let end = cursor + len as usize;
                if end > data.len() {
                    return Err(Error::InvalidData);
                }
                name = String::from_utf8_lossy(&data[cursor..end]).to_string();
                cursor = end;
            }
            // size (field 2, varint)
            (2, 0) => {
                let (value, next) = read_varint(data, cursor)?;
                cursor = next;
                size = value;
            }
            // Unknown field: skip it.
            (_, 0) => {
                let (_, next) = read_varint(data, cursor)?;
                cursor = next;
            }
            (_, 2) => {
                let (len, next) = read_varint(data, cursor)?;
                cursor = next + len as usize;
                if cursor > data.len() {
                    return Err(Error::InvalidData);
                }
            }
            _ => return Err(Error::InvalidData),
        }
    }

    Ok((name, size))
}

/// Reads a base-128 varint starting at `pos`, returning its value and the
/// offset just past it.
fn read_varint(data: &[u8], pos: usize) -> PolarResult<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0;
    let mut cursor = pos;

    loop {
        if cursor >= data.len() {
            return Err(Error::InvalidData);
        }
        let byte = data[cursor];
        cursor += 1;
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(Error::InvalidData);
        }
    }

    Ok((value, cursor))
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

    // Get the notification stream before writing so the response is not missed.
    let mut notifications = device.notifications().await.map_err(Error::BleError)?;

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

/// Removes a file or directory from the device over PS-FTP.
///
/// The request uses the same framing as `get_file` but with a REMOVE command
/// and no payload. The device responds with an error code (0 on success).
pub(crate) async fn remove_file(device: &Peripheral, path: &str) -> PolarResult<()> {
    let mtu = find_characteristic(device, PSFTP_MTU_UUID).await?;
    let d2h = find_characteristic(device, PSFTP_D2H_UUID).await?;

    device.subscribe(&mtu).await.map_err(Error::BleError)?;
    device.subscribe(&d2h).await.map_err(Error::BleError)?;

    // Build the protobuf PbPFtpOperation { command = REMOVE (3), path = <path> }.
    let operation = encode_pftp_operation_with_command(path, 3);
    // Prefix with RFC60 2-byte little-endian length.
    let mut message = Vec::with_capacity(2 + operation.len());
    message.push((operation.len() & 0xff) as u8);
    message.push(((operation.len() >> 8) & 0x7f) as u8);
    message.extend_from_slice(&operation);

    // Get the notification stream before writing so the response is not missed.
    let mut notifications = device.notifications().await.map_err(Error::BleError)?;

    // Split the message into RFC76 frames and write them.
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

    // Read the response frame and check for errors.
    loop {
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
                if code == 0 {
                    return Ok(());
                }
                return Err(Error::PftpError(code));
            }
            _ => return Err(Error::InvalidData),
        }
    }
}

/// Encodes a `PbPFtpOperation` protobuf message with a GET command.
fn encode_pftp_operation(path: &str) -> Vec<u8> {
    encode_pftp_operation_with_command(path, 0)
}

/// Encodes a `PbPFtpOperation` protobuf message with the given command.
///
/// The command is a varint: GET = 0, PUT = 1, REMOVE = 3.
fn encode_pftp_operation_with_command(path: &str, command: u8) -> Vec<u8> {
    // field 1 (command) = varint 0x08, value <command>;
    // field 2 (path) = length-delimited 0x12, length, string bytes.
    let mut out = vec![0x08, command, 0x12, path.len() as u8];
    out.extend_from_slice(path.as_bytes());
    out
}

/// Uploads a file to the device over PS-FTP.
///
/// The operation header (a protobuf `PbPFtpOperation` with a PUT command) and
/// the file contents are streamed as a single RFC76 message: header frames
/// first (with the `next` bit clear on the first frame), then the data frames.
pub(crate) async fn put_file(
    device: &Peripheral,
    path: &str,
    contents: &[u8],
) -> PolarResult<()> {
    let mtu = find_characteristic(device, PSFTP_MTU_UUID).await?;
    let d2h = find_characteristic(device, PSFTP_D2H_UUID).await?;

    device.subscribe(&mtu).await.map_err(Error::BleError)?;
    device.subscribe(&d2h).await.map_err(Error::BleError)?;

    // Build the protobuf PbPFtpOperation { command = PUT (1), path = <path> }.
    let operation = encode_pftp_operation_with_command(path, 1);
    // Prefix with RFC60 2-byte little-endian length.
    let mut message = Vec::with_capacity(2 + operation.len() + contents.len());
    message.push((operation.len() & 0xff) as u8);
    message.push(((operation.len() >> 8) & 0x7f) as u8);
    message.extend_from_slice(&operation);
    message.extend_from_slice(contents);

    // Get the notification stream before writing so the response is not missed.
    let mut notifications = device.notifications().await.map_err(Error::BleError)?;

    // Split the message into RFC76 frames and write them. The first frame has
    // the `next` bit clear; subsequent frames set it. Host-to-device frames set
    // the direction bit (status 0x06 for MORE, 0x02 for LAST).
    let mtu_size = mtu_size(device).await;
    let mut seq = 0u8;
    let mut offset = 0usize;
    let mut next = 0u8;
    loop {
        let remaining = message.len() - offset;
        let chunk_len = remaining.min(mtu_size - 1);
        let last = remaining < mtu_size;

        let mut frame = Vec::with_capacity(chunk_len + 1);
        let status = if last { 0x02 } else { 0x06 };
        frame.push((seq << 4) | status | next);
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

    // Read the response frame and check for errors.
    loop {
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
                if code == 0 {
                    return Ok(());
                }
                return Err(Error::PftpError(code));
            }
            _ => return Err(Error::InvalidData),
        }
    }
}

/// Determines the ATT MTU size to use for framing.
async fn mtu_size(_device: &Peripheral) -> usize {
    // btleplug does not expose the negotiated MTU directly; use a conservative
    // value that works across platforms.
    23
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parses_directory_entries() {
        // Two PbPFtpEntry messages wrapped in the repeated field:
        //   { name: "20250101/", size: 123 }
        //   { name: "PPI00.REC", size: 456 }
        let entry1 = {
            let mut e = Vec::new();
            e.push(0x0A); // name field
            e.push(9);
            e.extend_from_slice(b"20250101/");
            e.push(0x10); // size field
            e.push(123);
            e
        };
        let entry2 = {
            let mut e = Vec::new();
            e.push(0x0A);
            e.push(9);
            e.extend_from_slice(b"PPI00.REC");
            e.push(0x10);
            e.push(0xC8); // 456 = varint 0xC8 0x03
            e.push(0x03);
            e
        };

        let mut data = Vec::new();
        data.push(0x0A); // repeated entries field 1
        data.push(entry1.len() as u8);
        data.extend_from_slice(&entry1);
        data.push(0x0A);
        data.push(entry2.len() as u8);
        data.extend_from_slice(&entry2);

        let entries = parse_directory(&data).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("20250101/".to_string(), 123));
        assert_eq!(entries[1], ("PPI00.REC".to_string(), 456));
    }

    #[test]
    fn parses_empty_directory() {
        assert!(parse_directory(&[]).unwrap().is_empty());
    }
}
