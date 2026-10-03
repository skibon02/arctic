//! # Arctic
//!
//! A Rust library for the Polar Verity Sense optical heart rate sensor.
//!
//! The Verity Sense can record data to its internal memory while disconnected
//! from Bluetooth, and can stream data live over Bluetooth. This library starts
//! and stops offline recording, lists and downloads recordings, and streams
//! online measurement data.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use arctic::{PolarSensor, MeasurementType, StreamFrame};
//! use futures::stream::StreamExt;
//! use std::time::Duration;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a new PolarSensor with the device ID written on the device.
//!     let mut sensor = PolarSensor::new("7B45F72B".to_string()).await?;
//!     // Bound the scan-and-connect attempt so it cannot hang.
//!     sensor.connect(Duration::from_secs(5)).await?;
//!
//!     // Discover the available stream settings, then select one of each.
//!     let mut settings = sensor.request_stream_settings(MeasurementType::Acc).await?;
//!     println!("sampling rates: {:?}", settings.sample_rates());
//!     settings.set_sample_rate(52).set_range(8).set_resolution(16);
//!
//!     let mut stream = sensor.start_streaming(MeasurementType::Acc, settings).await?;
//!     while let Some(frame) = stream.next().await {
//!         match frame? {
//!             StreamFrame::Acc(samples) => {
//!                 for sample in samples {
//!                     println!("x={} y={} z={}", sample.x, sample.y, sample.z);
//!                 }
//!             }
//!             _ => {}
//!         }
//!     }
//!     Ok(())
//! }
//! ```

#![deny(missing_docs)]

mod connection;
mod control;
mod offline;
mod pftp;
mod pmd;
mod polar_uuid;
mod settings;
mod stream;

use btleplug::api::{Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::{BoxStream, StreamExt};
use std::fmt;
use tokio::time::{self, Duration};
use uuid::Uuid;

pub use offline::{OfflineRecord, PpiRecord};
pub use pmd::{AccSample, GyroSample, MagCalibration, MagSample, PpiSample};
pub use polar_uuid::MeasurementType;
pub use settings::StreamSettings;
pub use stream::StreamFrame;

/// Initializes the btleplug Android backend.
///
/// On Android, btleplug's `droidplug` backend must be initialized with a JNI
/// environment before any Bluetooth operation. This binds the given `JavaVM`
/// pointer into btleplug's global state and registers its native callbacks.
///
/// Must be called once, before constructing a [`PolarSensor`].
///
/// # Safety
///
/// `vm` must be a valid pointer to a `JavaVM` for the lifetime of the process.
#[cfg(target_os = "android")]
pub fn init_android(vm: *mut std::ffi::c_void) -> PolarResult<()> {
    use jni::JavaVM;

    let vm = unsafe { JavaVM::from_raw(vm as *mut jni::sys::JavaVM) };

    vm.attach_current_thread(|env| btleplug::platform::init(env))
        .map_err(Error::BleError)
}

/// Error type for general errors and BLE errors from btleplug
#[derive(Debug)]
pub enum Error {
    /// No bluetooth adapter found when trying to scan
    NoBleAdaptor,
    /// Could not find a device when trying to connect
    NoDevice,
    /// An operation did not complete within the configured timeout
    Timeout,
    /// Device is not connected, but function was called that requires it
    NotConnected,
    /// The device disconnected during an operation
    Disconnected,
    /// The device stopped the measurement on its own
    StreamStopped,
    /// Device is missing a characteristic that was used
    CharacteristicNotFound,
    /// Data packets received from device could not be parsed
    InvalidData,
    /// Not enough data was received
    InvalidLength,
    /// The device reported an error in response to a control point command
    ControlPointError(u8),
    /// The PFTP file transfer reported an error
    PftpError(u16),
    /// The offline recording file has an unexpected format
    InvalidRecording,
    /// An error occurred in the underlying BLE library
    BleError(btleplug::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Error::NoBleAdaptor => "No BLE adaptor".to_string(),
            Error::NoDevice => "No device".to_string(),
            Error::Timeout => "Operation timed out".to_string(),
            Error::NotConnected => "Not connected".to_string(),
            Error::Disconnected => "Device disconnected".to_string(),
            Error::StreamStopped => "Device stopped the stream".to_string(),
            Error::CharacteristicNotFound => "Characteristic not found".to_string(),
            Error::InvalidData => "Invalid data".to_string(),
            Error::InvalidLength => "Invalid length".to_string(),
            Error::ControlPointError(code) => format!("Control point error: {}", code),
            Error::PftpError(code) => format!("PFTP error: {}", code),
            Error::InvalidRecording => "Invalid offline recording".to_string(),
            Error::BleError(er) => format!("BLE error: {:?}", er),
        };
        write!(f, "Arctic Error: {}", msg)
    }
}

impl std::error::Error for Error {}

/// Result simplification type
pub type PolarResult<T> = std::result::Result<T, Error>;

/// The core Polar device structure. Keeps track of connection and offline
/// recording operations.
pub struct PolarSensor {
    /// The device id written on the device (e.g, "8C4CAD2D")
    device_id: String,
    /// BLE connection handlers
    ble_manager: Manager,
    /// The active connection, if any
    connection: Option<connection::Connection>,
}

impl PolarSensor {
    /// Creates a new [`PolarSensor`].
    ///
    /// # Errors
    ///
    /// Returns a [`Error::BleError`] if the bluetooth manager could not be
    /// created, or [`Error::InvalidLength`] if the device id is not 8
    /// characters long.
    pub async fn new(device_id: String) -> PolarResult<PolarSensor> {
        let ble_manager = Manager::new().await.map_err(Error::BleError)?;

        if device_id.len() != 8 {
            return Err(Error::InvalidLength);
        }

        Ok(PolarSensor {
            device_id,
            ble_manager,
            connection: None,
        })
    }

    /// Scans for the first Polar Verity Sense and connects to it, regardless of
    /// the device id this instance was created with.
    ///
    /// `timeout` bounds the whole operation: the scan, the connect, and service
    /// discovery. If it elapses the attempt fails with [`Error::Timeout`] or
    /// [`Error::NoDevice`] rather than hanging.
    ///
    /// # Errors
    ///
    /// Returns a [`Error::BleError`] if the bluetooth adapter, scan, or service
    /// discovery fails. Returns [`Error::NoBleAdaptor`] if no adapters are
    /// available, [`Error::NoDevice`] if no Verity Sense was found, and
    /// [`Error::Timeout`] if the operation exceeded `timeout`.
    pub async fn discover(&mut self, timeout: Duration) -> PolarResult<()> {
        let central = self.scan(timeout).await?;

        let device = self
            .find_device_by(&central, |name| name.starts_with("Polar Sense"), timeout)
            .await;

        // Stop scanning before connecting; an active scan interferes with the
        // connection on some platforms. Stop it on the not-found path too, so a
        // failed attempt does not leave a scan running on the adapter.
        let _ = central.stop_scan().await;
        let device = device.ok_or(Error::NoDevice)?;

        log::info!("arctic: connecting to device...");
        self.connection = Some(connection::Connection::connect(&central, device, timeout).await?);
        log::info!("arctic: connected");
        Ok(())
    }

    /// Finds and connects to the device id associated with this instance.
    ///
    /// `timeout` bounds the whole operation: the scan, the connect, and service
    /// discovery. If it elapses the attempt fails with [`Error::Timeout`] or
    /// [`Error::NoDevice`] rather than hanging.
    ///
    /// # Errors
    ///
    /// Returns a [`Error::BleError`] if the bluetooth adapter, scan, or service
    /// discovery fails. Returns [`Error::NoBleAdaptor`] if no adapters are
    /// available, [`Error::NoDevice`] if the device was not found, and
    /// [`Error::Timeout`] if the operation exceeded `timeout`.
    pub async fn connect(&mut self, timeout: Duration) -> PolarResult<()> {
        let central = self.scan(timeout).await?;
        let device_id = self.device_id.clone();

        let device = self
            .find_device_by(
                &central,
                move |name| name.starts_with("Polar") && name.ends_with(&device_id),
                timeout,
            )
            .await;

        // Stop scanning before connecting; an active scan interferes with the
        // connection on some platforms. Stop it on the not-found path too, so a
        // failed attempt does not leave a scan running on the adapter.
        let _ = central.stop_scan().await;
        let device = device.ok_or(Error::NoDevice)?;

        self.connection = Some(connection::Connection::connect(&central, device, timeout).await?);
        Ok(())
    }

    /// Disconnects from the device.
    ///
    /// Any active stream ends with [`Error::Disconnected`].
    pub async fn disconnect(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.disconnect().await;
        }
    }

    /// Returns whether the device is currently connected or not
    pub async fn is_connected(&self) -> bool {
        match &self.connection {
            Some(connection) => connection.is_connected().await,
            None => false,
        }
    }

    /// Starts offline recording of the given measurement type to the device's
    /// internal memory.
    ///
    /// The recording continues even after the Bluetooth connection is closed.
    ///
    /// `secret` is an optional XOR encryption key. If provided, the same key
    /// must be supplied when downloading the recording.
    ///
    /// When recording PPI, the device's PPI-mode LED is disabled first so it
    /// does not blink during the measurement.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the request.
    pub async fn start_offline_recording(
        &self,
        ty: MeasurementType,
        secret: Option<&[u8]>,
    ) -> PolarResult<()> {
        let conn = self.connection()?;
        if ty == MeasurementType::Ppi {
            pftp::disable_ppi_led(conn).await?;
        }
        control::start_offline_recording(conn, ty, secret).await
    }

    /// Stops the offline recording of the given measurement type.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the request.
    pub async fn stop_offline_recording(&self, ty: MeasurementType) -> PolarResult<()> {
        control::stop_measurement(self.connection()?, ty).await
    }

    /// Queries the device for the currently active measurements.
    ///
    /// Returns the raw status bytes reported by the device, each encoding a
    /// measurement type (low 6 bits) and its active state (high 2 bits).
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the request.
    pub async fn get_measurement_status(&self) -> PolarResult<Vec<u8>> {
        control::get_measurement_status(self.connection()?).await
    }

    /// Returns whether an offline recording of the given type is currently
    /// active on the device.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the request.
    pub async fn is_offline_recording_active(
        &self,
        ty: MeasurementType,
    ) -> PolarResult<bool> {
        control::is_offline_recording_active(self.connection()?, ty).await
    }

    /// Lists the offline recordings stored on the device.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::PftpError`] if the file listing fails.
    pub async fn list_offline_recordings(&self) -> PolarResult<Vec<OfflineRecord>> {
        pftp::list_offline_recordings(self.connection()?).await
    }

    /// Downloads and parses an offline recording.
    ///
    /// `secret` must match the XOR key used when starting the recording, if
    /// any.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, [`Error::PftpError`]
    /// if the download fails, or [`Error::InvalidRecording`] if the file cannot
    /// be parsed.
    pub async fn get_offline_record(
        &self,
        entry: &OfflineRecord,
        secret: Option<&[u8]>,
    ) -> PolarResult<PpiRecord> {
        let data = pftp::get_file(self.connection()?, &entry.path).await?;
        offline::parse_ppi_record(&data, secret)
    }

    /// Removes an offline recording from the device.
    ///
    /// The device never frees its memory automatically, so recordings must be
    /// removed after they have been downloaded to avoid running out of space.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::PftpError`] if the removal fails.
    pub async fn remove_offline_record(&self, entry: &OfflineRecord) -> PolarResult<()> {
        pftp::remove_file(self.connection()?, &entry.path).await
    }

    /// Sets the device LED configuration.
    ///
    /// This is a persistent setting on the device. `sdk_mode_led` controls the
    /// LED while the device is in SDK mode, and `ppi_mode_led` controls the LED
    /// that blinks during PPI measurements.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::PftpError`] if the write fails.
    pub async fn set_led(&self, sdk_mode_led: bool, ppi_mode_led: bool) -> PolarResult<()> {
        pftp::set_led(self.connection()?, sdk_mode_led, ppi_mode_led).await
    }

    /// Requests the stream settings available for a measurement type.
    ///
    /// The returned [`StreamSettings`] lists every sampling rate, resolution,
    /// range, and channel count the device offers for the type. Select one of
    /// each with the `set_*` methods before starting a stream. Settings that
    /// are not selected use the device defaults.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the request.
    pub async fn request_stream_settings(
        &self,
        ty: MeasurementType,
    ) -> PolarResult<StreamSettings> {
        control::request_stream_settings(self.connection()?, ty).await
    }

    /// Starts online streaming of the given measurement type.
    ///
    /// The returned stream yields batches of parsed samples. It ends, with an
    /// error, when the device disconnects, when the device stops the
    /// measurement, or when a frame cannot be parsed. Dropping the stream stops
    /// the measurement on the device.
    ///
    /// Streams of several measurement types may run concurrently. Each stream
    /// receives only the frames of its own type, and stopping or dropping one
    /// stream leaves the others running. Use [`PolarSensor::stop_streaming`] to
    /// stop a stream explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the start request.
    pub async fn start_streaming(
        &self,
        ty: MeasurementType,
        settings: StreamSettings,
    ) -> PolarResult<BoxStream<'static, PolarResult<StreamFrame>>> {
        stream::start(self.connection()?, ty, settings).await
    }

    /// Stops online streaming of the given measurement type.
    ///
    /// Only the given type is stopped; streams of other types keep running.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the stop request.
    pub async fn stop_streaming(&self, ty: MeasurementType) -> PolarResult<()> {
        control::stop_measurement(self.connection()?, ty).await
    }

    /// Starts standard BLE Heart Rate streaming and returns a stream of BPM
    /// values.
    ///
    /// Unlike PMD streaming, this uses the standard Heart Rate Service
    /// (`0x180D`) measurement characteristic (`0x2A37`), which delivers only the
    /// current heart rate in beats per minute with no pulse-to-pulse intervals.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::BleError`] if the subscription fails.
    pub async fn start_hr_streaming(&self) -> PolarResult<BoxStream<'static, u8>> {
        let conn = self.connection()?;
        let mut rx = conn
            .subscribe(polar_uuid::HEART_RATE_MEASUREMENT_UUID)
            .await?;

        let stream = async_stream::stream! {
            while let Some(value) = rx.recv().await {
                // Heart Rate Measurement: [flags, bpm, ...]. BPM is the second byte.
                if let Some(bpm) = value.get(1).copied() {
                    yield bpm;
                }
            }
        };

        Ok(stream.boxed())
    }

    /// Stops standard BLE Heart Rate streaming by unsubscribing from the
    /// measurement characteristic.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected.
    pub async fn stop_hr_streaming(&self) -> PolarResult<()> {
        self.connection()?
            .release_all(polar_uuid::HEART_RATE_MEASUREMENT_UUID)
            .await;
        Ok(())
    }

    /// Scans for and returns all matching Polar devices (name and device id).
    ///
    /// The device id is the trailing 8-character suffix of the advertised name.
    /// The scan runs for `timeout` so that nearby devices can be discovered.
    ///
    /// # Errors
    ///
    /// Returns [`Error::BleError`] if the adapter or scan fails, or
    /// [`Error::NoBleAdaptor`] if no adapters are available.
    pub async fn list_devices(&self, timeout: Duration) -> PolarResult<Vec<(String, String)>> {
        let central = self.scan(timeout).await?;

        // Let the adapter discover nearby devices for the scan duration.
        tokio::time::sleep(timeout).await;

        let mut devices = Vec::new();
        let peripherals = tokio::time::timeout(timeout, central.peripherals())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(Error::BleError)?;
        for p in peripherals {
            let props = tokio::time::timeout(timeout, p.properties())
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(Error::BleError)?;
            if let Some(props) = props {
                if let Some(name) = props.local_name {
                    if name.starts_with("Polar") {
                        if let Some(id) = name.split_whitespace().last() {
                            if id.len() == 8 && id.chars().all(|c| c.is_ascii_hexdigit()) {
                                devices.push((name.clone(), id.to_uppercase()));
                            }
                        }
                    }
                }
            }
        }

        // Stop the scan so repeated listings do not leave scans running.
        let _ = tokio::time::timeout(timeout, central.stop_scan()).await;

        Ok(devices)
    }

    /// Returns the active connection, or [`Error::NotConnected`].
    fn connection(&self) -> PolarResult<&connection::Connection> {
        self.connection.as_ref().ok_or(Error::NotConnected)
    }

    /// Starts a scan on the first available adapter.
    async fn scan(&self, timeout: Duration) -> PolarResult<Adapter> {
        let adapters = tokio::time::timeout(timeout, self.ble_manager.adapters())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(Error::BleError)?;
        if adapters.is_empty() {
            return Err(Error::NoBleAdaptor);
        }

        let central = adapters.into_iter().next().unwrap();
        tokio::time::timeout(timeout, central.start_scan(ScanFilter::default()))
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(Error::BleError)?;

        Ok(central)
    }

    /// Scans for a device whose advertised local name matches `predicate`,
    /// reacting to discovery events until a match is found or the scan times
    /// out.
    async fn find_device_by<F>(
        &self,
        central: &Adapter,
        predicate: F,
        timeout: Duration,
    ) -> Option<Peripheral>
    where
        F: Fn(&str) -> bool,
    {
        let mut events = tokio::time::timeout(timeout, central.events())
            .await
            .ok()?
            .ok()?;
        let deadline = time::Instant::now() + timeout;

        // Check peripherals already discovered before subscribing to events.
        if let Some(device) = matching_peripheral(central, &predicate, timeout).await {
            return Some(device);
        }

        loop {
            let remaining = deadline.saturating_duration_since(time::Instant::now());
            if remaining.is_zero() {
                return None;
            }

            let event = match time::timeout(remaining, events.next()).await {
                Ok(Some(event)) => event,
                _ => return None,
            };

            // Only re-check peripherals when a new device appears or updates.
            match event {
                CentralEvent::DeviceDiscovered(_) | CentralEvent::DeviceUpdated(_) => {}
                _ => continue,
            }

            if let Some(device) = matching_peripheral(central, &predicate, timeout).await {
                return Some(device);
            }
        }
    }
}

/// Returns the first peripheral whose advertised local name matches
/// `predicate`.
async fn matching_peripheral<F>(
    central: &Adapter,
    predicate: &F,
    timeout: Duration,
) -> Option<Peripheral>
where
    F: Fn(&str) -> bool,
{
    let peripherals = tokio::time::timeout(timeout, central.peripherals())
        .await
        .ok()?
        .ok()?;
    for p in peripherals {
        let props = tokio::time::timeout(timeout, p.properties()).await.ok()?.ok()?;
        if let Some(props) = props {
            if props.local_name.iter().any(|name| predicate(name)) {
                return Some(p);
            }
        }
    }

    None
}

/// Private helper to find characteristics from a [`Uuid`]
pub(crate) async fn find_characteristic(
    device: &Peripheral,
    uuid: Uuid,
) -> PolarResult<btleplug::api::Characteristic> {
    device
        .characteristics()
        .iter()
        .find(|c| c.uuid == uuid)
        .ok_or(Error::CharacteristicNotFound)
        .cloned()
}

#[cfg(test)]
mod test {
    use super::*;

    macro_rules! aw {
        ($e:expr) => {
            tokio_test::block_on($e)
        };
    }

    #[test]
    fn new_rejects_bad_id() {
        let res = aw!(PolarSensor::new("short".to_string()));
        assert!(matches!(res, Err(Error::InvalidLength)));
    }
}
