//! # Arctic
//!
//! A Rust library for offline PPI (pulse-to-pulse interval) recording with the
//! Polar Verity Sense optical heart rate sensor.
//!
//! The Verity Sense can record PPI data to its internal memory while
//! disconnected from Bluetooth. This library starts and stops that recording,
//! lists the recorded files, and downloads and parses them.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use arctic::{PolarSensor, MeasurementType};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a new PolarSensor with the device ID written on the device.
//!     let mut sensor = PolarSensor::new("7B45F72B".to_string()).await?;
//!     sensor.connect().await?;
//!
//!     // Start recording PPI to the device's internal memory.
//!     sensor.start_offline_recording(MeasurementType::Ppi, None).await?;
//!
//!     // The device records while disconnected. Reconnect later to stop and
//!     // download the recorded data.
//!     sensor.stop_offline_recording(MeasurementType::Ppi).await?;
//!
//!     let recordings = sensor.list_offline_recordings().await?;
//!     for entry in recordings {
//!         let ppi = sensor.get_offline_record(&entry, None).await?;
//!         for sample in ppi.samples {
//!             println!("pp={} ms, hr={}", sample.pp_in_ms, sample.hr);
//!         }
//!     }
//!     Ok(())
//! }
//! ```

#![deny(missing_docs)]

mod control;
mod offline;
mod pftp;
mod polar_uuid;

use btleplug::api::{Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::{BoxStream, StreamExt};
use std::fmt;
use tokio::time::{self, Duration};
use uuid::Uuid;

pub use offline::{OfflineRecord, PpiRecord, PpiSample};
pub use polar_uuid::MeasurementType;

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
    /// Device is not connected, but function was called that requires it
    NotConnected,
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
            Error::NotConnected => "Not connected".to_string(),
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
    /// The connection to the device
    ble_device: Option<Peripheral>,
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
            ble_device: None,
        })
    }

    /// Scans for the first Polar Verity Sense and connects to it, regardless of
    /// the device id this instance was created with.
    ///
    /// # Errors
    ///
    /// Returns a [`Error::BleError`] if the bluetooth adapter, scan, or service
    /// discovery fails. Returns [`Error::NoBleAdaptor`] if no adapters are
    /// available, and [`Error::NoDevice`] if no Verity Sense was found.
    pub async fn discover(&mut self) -> PolarResult<()> {
        let central = self.scan().await?;

        self.ble_device = self
            .find_device_by(&central, |name| name.starts_with("Polar Sense"))
            .await;

        if let Some(device) = &self.ble_device {
            log::info!("arctic: connecting to device...");
            device.connect().await.map_err(Error::BleError)?;
            log::info!("arctic: connected, discovering services...");
            device.discover_services().await.map_err(Error::BleError)?;
            log::info!("arctic: services discovered");
            return Ok(());
        }

        Err(Error::NoDevice)
    }

    /// Finds and connects to the device id associated with this instance.
    ///
    /// # Errors
    ///
    /// Returns a [`Error::BleError`] if the bluetooth adapter, scan, or service
    /// discovery fails. Returns [`Error::NoBleAdaptor`] if no adapters are
    /// available, and [`Error::NoDevice`] if the device was not found.
    pub async fn connect(&mut self) -> PolarResult<()> {
        let central = self.scan().await?;
        let device_id = self.device_id.clone();

        self.ble_device = self
            .find_device_by(&central, move |name| {
                name.starts_with("Polar") && name.ends_with(&device_id)
            })
            .await;

        if let Some(device) = &self.ble_device {
            device.connect().await.map_err(Error::BleError)?;
            device.discover_services().await.map_err(Error::BleError)?;
            return Ok(());
        }

        Err(Error::NoDevice)
    }

    /// Returns whether the device is currently connected or not
    pub async fn is_connected(&self) -> bool {
        if let Some(device) = &self.ble_device {
            if let Ok(value) = device.is_connected().await {
                return value;
            }
        }

        false
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
        if ty == MeasurementType::Ppi {
            pftp::disable_ppi_led(self.device().await?).await?;
        }
        control::start_offline_recording(self.device().await?, ty, secret).await
    }

    /// Stops the offline recording of the given measurement type.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the request.
    pub async fn stop_offline_recording(&self, ty: MeasurementType) -> PolarResult<()> {
        control::stop_measurement(self.device().await?, ty).await
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
        control::get_measurement_status(self.device().await?).await
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
        control::is_offline_recording_active(self.device().await?, ty).await
    }

    /// Lists the offline recordings stored on the device.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::PftpError`] if the file listing fails.
    pub async fn list_offline_recordings(&self) -> PolarResult<Vec<OfflineRecord>> {
        pftp::list_offline_recordings(self.device().await?).await
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
    ) -> PolarResult<offline::PpiRecord> {
        let data = pftp::get_file(self.device().await?, &entry.path).await?;
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
        pftp::remove_file(self.device().await?, &entry.path).await
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
        pftp::set_led(self.device().await?, sdk_mode_led, ppi_mode_led).await
    }

    /// Starts online PPI streaming and returns a stream of parsed PPI frames.
    ///
    /// Unlike offline recording, online streaming delivers PPI samples live over
    /// the PMD data characteristic while the connection is open. Each yielded
    /// item is a `(timestamp_us, samples)` pair: the frame timestamp (time of
    /// the last sample, in microseconds) and the parsed samples.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the start request.
    pub async fn start_ppi_streaming(
        &self,
    ) -> PolarResult<BoxStream<'static, (u64, Vec<PpiSample>)>> {
        let device = self.device().await?;

        // Subscribe to the PMD data characteristic so frames are delivered.
        let data_char = find_characteristic(device, polar_uuid::PMD_DATA_UUID).await?;
        device.subscribe(&data_char).await.map_err(Error::BleError)?;

        // Start online PPI streaming.
        control::start_online_streaming(device, MeasurementType::Ppi).await?;

        let notifications = device.notifications().await.map_err(Error::BleError)?;
        let stream = notifications.filter_map(move |n| {
            if n.uuid != polar_uuid::PMD_DATA_UUID {
                return futures::future::ready(None);
            }
            let (ts, samples) = match offline::parse_ppi_stream_frame(&n.value) {
                Ok((ts, s)) if !s.is_empty() => (ts, s),
                _ => return futures::future::ready(None),
            };
            futures::future::ready(Some((ts, samples)))
        });

        Ok(stream.boxed())
    }

    /// Stops online PPI streaming.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::ControlPointError`] if the device rejects the stop request.
    pub async fn stop_ppi_streaming(&self) -> PolarResult<()> {
        control::stop_measurement(self.device().await?, MeasurementType::Ppi).await
    }

    /// Starts standard BLE Heart Rate streaming and returns a stream of BPM
    /// values.
    ///
    /// Unlike PPI streaming, this uses the standard Heart Rate Service
    /// (`0x180D`) measurement characteristic (`0x2A37`), which delivers only the
    /// current heart rate in beats per minute with no pulse-to-pulse intervals.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::BleError`] if the subscription fails.
    pub async fn start_hr_streaming(&self) -> PolarResult<BoxStream<'static, u8>> {
        let device = self.device().await?;

        let hr_char = find_characteristic(device, polar_uuid::HEART_RATE_MEASUREMENT_UUID).await?;
        device.subscribe(&hr_char).await.map_err(Error::BleError)?;

        let notifications = device.notifications().await.map_err(Error::BleError)?;
        let stream = notifications.filter_map(move |n| {
            if n.uuid != polar_uuid::HEART_RATE_MEASUREMENT_UUID {
                return futures::future::ready(None);
            }
            // Heart Rate Measurement: [flags, bpm, ...]. BPM is the second byte.
            let bpm = n.value.get(1).copied();
            futures::future::ready(bpm)
        });

        Ok(stream.boxed())
    }

    /// Stops standard BLE Heart Rate streaming by unsubscribing from the
    /// measurement characteristic.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if not connected, or
    /// [`Error::BleError`] if the unsubscribe fails.
    pub async fn stop_hr_streaming(&self) -> PolarResult<()> {
        let device = self.device().await?;
        let hr_char = find_characteristic(device, polar_uuid::HEART_RATE_MEASUREMENT_UUID).await?;
        device
            .unsubscribe(&hr_char)
            .await
            .map_err(Error::BleError)
    }

    /// Scans for and returns all matching Polar devices (name and device id).
    ///
    /// The device id is the trailing 8-character suffix of the advertised name.
    /// The scan runs for a short period so that nearby devices can be discovered.
    ///
    /// # Errors
    ///
    /// Returns [`Error::BleError`] if the adapter or scan fails, or
    /// [`Error::NoBleAdaptor`] if no adapters are available.
    pub async fn list_devices(&self) -> PolarResult<Vec<(String, String)>> {
        let central = self.scan().await?;

        // Give the adapter a moment to discover nearby devices.
        tokio::time::sleep(Duration::from_secs(3)).await;

        let mut devices = Vec::new();
        for p in central.peripherals().await.map_err(Error::BleError)? {
            if let Some(props) = p.properties().await.map_err(Error::BleError)? {
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

        Ok(devices)
    }

    async fn device(&self) -> PolarResult<&Peripheral> {
        if let Some(device) = &self.ble_device {
            return Ok(device);
        }

        Err(Error::NoDevice)
    }

    /// Starts a scan on the first available adapter.
    async fn scan(&self) -> PolarResult<Adapter> {
        let adapters = self.ble_manager.adapters().await.map_err(Error::BleError)?;
        if adapters.is_empty() {
            return Err(Error::NoBleAdaptor);
        }

        let central = adapters.into_iter().next().unwrap();
        central
            .start_scan(ScanFilter::default())
            .await
            .map_err(Error::BleError)?;

        Ok(central)
    }

    /// Scans for a device whose advertised local name matches `predicate`,
    /// reacting to discovery events until a match is found or the scan times
    /// out.
    async fn find_device_by<F>(&self, central: &Adapter, predicate: F) -> Option<Peripheral>
    where
        F: Fn(&str) -> bool,
    {
        let mut events = central.events().await.ok()?;
        let deadline = time::Instant::now() + Duration::from_secs(10);

        // Check peripherals already discovered before subscribing to events.
        if let Some(device) = matching_peripheral(central, &predicate).await {
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

            if let Some(device) = matching_peripheral(central, &predicate).await {
                return Some(device);
            }
        }
    }
}

/// Returns the first peripheral whose advertised local name matches
/// `predicate`.
async fn matching_peripheral<F>(central: &Adapter, predicate: &F) -> Option<Peripheral>
where
    F: Fn(&str) -> bool,
{
    for p in central.peripherals().await.ok()? {
        if let Some(props) = p.properties().await.ok().flatten() {
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
