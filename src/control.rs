//! # Control
//!
//! Commands sent over the PMD control point to start and stop offline
//! recording.

use crate::polar_uuid::{MeasurementType, PMD_CP_UUID, PMD_DATA_UUID};
use crate::{find_characteristic, Error, PolarResult};

use btleplug::api::{Peripheral as _, WriteType};
use btleplug::platform::Peripheral;
use futures::stream::StreamExt;

/// Control point command opcodes (client to service)
const REQUEST_MEASUREMENT_START: u8 = 0x02;
const STOP_MEASUREMENT: u8 = 0x03;

/// Bit set in the type byte to request offline (vs online) recording
const OFFLINE_BIT: u8 = 0x80;

/// Control point response marker byte
const CP_RESPONSE: u8 = 0xF0;

/// Starts offline recording of the given measurement type.
pub(crate) async fn start_offline_recording(
    device: &Peripheral,
    ty: MeasurementType,
    secret: Option<&[u8]>,
) -> PolarResult<()> {
    let request_byte = OFFLINE_BIT | ty.as_u8();
    let mut command = vec![REQUEST_MEASUREMENT_START, request_byte];

    // Append the security setting if a secret was provided.
    if let Some(key) = secret {
        // security setting type = 0x05, length = 1 + key length, strategy = xor (0x01)
        command.push(0x05);
        command.push(1 + key.len() as u8);
        command.push(0x01);
        command.extend_from_slice(key);
    }

    let response = send_command(device, command).await?;
    ensure_success(response)?;
    Ok(())
}

/// Stops the measurement of the given type.
pub(crate) async fn stop_measurement(device: &Peripheral, ty: MeasurementType) -> PolarResult<()> {
    let response = send_command(device, vec![STOP_MEASUREMENT, ty.as_u8()]).await?;
    ensure_success(response)?;
    Ok(())
}

/// Writes a command to the PMD control point and waits for the response.
async fn send_command(device: &Peripheral, command: Vec<u8>) -> PolarResult<ControlResponse> {
    let characteristic = find_characteristic(device, PMD_CP_UUID).await?;
    device
        .subscribe(&characteristic)
        .await
        .map_err(Error::BleError)?;

    // The PMD data characteristic must also be subscribed before the control
    // point accepts commands.
    if let Ok(data_char) = find_characteristic(device, PMD_DATA_UUID).await {
        let _ = device.subscribe(&data_char).await;
    }

    device
        .write(&characteristic, &command, WriteType::WithResponse)
        .await
        .map_err(Error::BleError)?;

    let mut notifications = device.notifications().await.map_err(Error::BleError)?;
    loop {
        let data = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            notifications.next(),
        )
        .await
        {
            Ok(Some(d)) => d,
            Ok(None) => return Err(Error::InvalidData),
            Err(_) => return Err(Error::InvalidData),
        };
        if data.uuid == PMD_CP_UUID && data.value.first() == Some(&CP_RESPONSE) {
            return ControlResponse::new(&data.value);
        }
    }
}

/// A parsed response from the PMD control point.
struct ControlResponse {
    error_code: u8,
}

impl ControlResponse {
    fn new(data: &[u8]) -> PolarResult<ControlResponse> {
        // Layout: [0xF0, opcode, type, error_code, more, params...]
        if data.len() < 4 || data[0] != CP_RESPONSE {
            return Err(Error::InvalidData);
        }
        Ok(ControlResponse {
            error_code: data[3],
        })
    }
}

fn ensure_success(response: ControlResponse) -> PolarResult<()> {
    if response.error_code == 0 {
        Ok(())
    } else {
        Err(Error::ControlPointError(response.error_code))
    }
}
