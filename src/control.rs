//! # Control
//!
//! Commands sent over the PMD control point: starting and stopping offline
//! recording, requesting stream settings, and starting and stopping online
//! streaming.

use crate::connection::Connection;
use crate::polar_uuid::{MeasurementType, PMD_CP_UUID, PMD_DATA_UUID};
use crate::settings::StreamSettings;
use crate::{Error, PolarResult};

use btleplug::api::{Peripheral as _, WriteType};

/// Control point command opcodes (client to service)
const REQUEST_MEASUREMENT_START: u8 = 0x02;
const STOP_MEASUREMENT: u8 = 0x03;
const GET_MEASUREMENT_SETTINGS: u8 = 0x01;
const GET_MEASUREMENT_STATUS: u8 = 0x05;

/// Bit set in the type byte to request offline (vs online) recording
const OFFLINE_BIT: u8 = 0x80;

/// Mask for the measurement type id in a status response byte
const MEASUREMENT_TYPE_MASK: u8 = 0x3F;
/// Mask for the active state in a status response byte
const ACTIVE_STATE_MASK: u8 = 0xC0;
/// Active state value indicating an offline recording is running
const OFFLINE_ACTIVE: u8 = 0x80;

/// Control point response marker byte
const CP_RESPONSE: u8 = 0xF0;
/// Control point command byte sent by the device when a stream stops
const ONLINE_MEASUREMENT_STOPPED: u8 = 0xF1;

/// Error code returned when the device is already in the requested state
pub(crate) const ERROR_ALREADY_IN_STATE: u8 = 6;

/// Starts offline recording of the given measurement type.
pub(crate) async fn start_offline_recording(
    conn: &Connection,
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

    let response = send_command(conn, command).await?;
    ensure_success(&response)?;
    Ok(())
}

/// Stops the measurement of the given type.
///
/// If the device reports "already in state" (error 6), the measurement is
/// already stopped and the request is treated as a no-op success.
pub(crate) async fn stop_measurement(conn: &Connection, ty: MeasurementType) -> PolarResult<()> {
    let response = send_command(conn, vec![STOP_MEASUREMENT, ty.as_u8()]).await?;
    match response.error_code {
        0 | ERROR_ALREADY_IN_STATE => Ok(()),
        code => Err(Error::ControlPointError(code)),
    }
}

/// Requests the stream settings available for the given measurement type.
pub(crate) async fn request_stream_settings(
    conn: &Connection,
    ty: MeasurementType,
) -> PolarResult<StreamSettings> {
    let response = send_command(
        conn,
        vec![GET_MEASUREMENT_SETTINGS, ty.as_u8()],
    )
    .await?;
    ensure_success(&response)?;
    StreamSettings::parse_available(&response.parameters)
}

/// Starts online (streaming) measurement of the given type with the given
/// settings, returning the settings the device reports for the active stream.
///
/// If the device reports "already in state" (error 6), a measurement of this
/// type is already running, possibly left over from an earlier session. It is
/// stopped and the start is retried so that the device streams to this session
/// with the requested settings.
pub(crate) async fn start_online_streaming(
    conn: &Connection,
    ty: MeasurementType,
    settings: &mut StreamSettings,
) -> PolarResult<()> {
    let mut command = vec![REQUEST_MEASUREMENT_START, ty.as_u8()];
    command.extend_from_slice(&settings.encode_selected());

    let response = send_command(conn, command.clone()).await?;
    match response.error_code {
        0 => {
            settings.update_from_start_response(&response.parameters)?;
            Ok(())
        }
        ERROR_ALREADY_IN_STATE => {
            stop_measurement(conn, ty).await?;
            let response = send_command(conn, command).await?;
            ensure_success(&response)?;
            settings.update_from_start_response(&response.parameters)?;
            Ok(())
        }
        code => Err(Error::ControlPointError(code)),
    }
}

/// Queries the device for the currently active measurements.
///
/// Returns the raw parameter bytes of the response, each encoding a measurement
/// type (low 6 bits) and its active state (high 2 bits).
pub(crate) async fn get_measurement_status(conn: &Connection) -> PolarResult<Vec<u8>> {
    let response = send_command(conn, vec![GET_MEASUREMENT_STATUS]).await?;
    ensure_success(&response)?;
    Ok(response.parameters)
}

/// Returns whether an offline recording of the given type is currently active.
pub(crate) async fn is_offline_recording_active(
    conn: &Connection,
    ty: MeasurementType,
) -> PolarResult<bool> {
    let status = get_measurement_status(conn).await?;
    let type_byte = ty.as_u8();

    Ok(status.iter().any(|byte| {
        (byte & MEASUREMENT_TYPE_MASK) == type_byte
            && (byte & ACTIVE_STATE_MASK) == OFFLINE_ACTIVE
    }))
}

/// Writes a command to the PMD control point and waits for the response.
///
/// The control point and data characteristics are subscribed for the duration
/// of the call. The control point response is delivered by the connection's
/// notification dispatcher.
async fn send_command(conn: &Connection, command: Vec<u8>) -> PolarResult<ControlResponse> {
    // The PMD data characteristic must also be subscribed before the control
    // point accepts commands.
    let mut cp_rx = conn.subscribe(PMD_CP_UUID).await?;
    let _data_rx = conn.subscribe(PMD_DATA_UUID).await?;

    let characteristic = crate::find_characteristic(conn.device(), PMD_CP_UUID).await?;
    let mut disconnected = conn.disconnected();

    conn.device()
        .write(&characteristic, &command, WriteType::WithResponse)
        .await
        .map_err(Error::BleError)?;

    loop {
        tokio::select! {
            _ = disconnected.recv() => {
                return Err(Error::Disconnected);
            }
            notification = tokio::time::timeout(std::time::Duration::from_secs(10), cp_rx.recv()) => {
                match notification {
                    Ok(Some(value)) => {
                        log::debug!("arctic: send_command: response={:02x?}", value);
                        if value.first() == Some(&CP_RESPONSE) {
                            return ControlResponse::new(&value);
                        }
                    }
                    Ok(None) => {
                        log::error!("arctic: send_command: control point channel closed");
                        return Err(Error::InvalidData);
                    }
                    Err(_) => {
                        log::error!("arctic: send_command: timed out waiting for response");
                        return Err(Error::InvalidData);
                    }
                }
            }
        }
    }
}

/// A parsed response from the PMD control point.
pub(crate) struct ControlResponse {
    pub(crate) error_code: u8,
    pub(crate) parameters: Vec<u8>,
}

impl ControlResponse {
    fn new(data: &[u8]) -> PolarResult<ControlResponse> {
        // Layout: [0xF0, opcode, type, error_code, more, params...]
        if data.len() < 5 || data[0] != CP_RESPONSE {
            return Err(Error::InvalidData);
        }
        Ok(ControlResponse {
            error_code: data[3],
            parameters: data[5..].to_vec(),
        })
    }
}

/// Returns the measurement type from a device-initiated stop command, if the
/// notification is one.
pub(crate) fn parse_online_measurement_stopped(data: &[u8]) -> Option<MeasurementType> {
    if data.first() == Some(&ONLINE_MEASUREMENT_STOPPED) {
        data.get(1).and_then(|byte| MeasurementType::from_id(*byte))
    } else {
        None
    }
}

fn ensure_success(response: &ControlResponse) -> PolarResult<()> {
    if response.error_code == 0 {
        Ok(())
    } else {
        Err(Error::ControlPointError(response.error_code))
    }
}
