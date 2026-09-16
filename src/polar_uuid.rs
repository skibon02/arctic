//! # Polar UUID
//!
//! This module contains the UUID characteristics and measurement types used to
//! communicate with the Polar Verity Sense.

use uuid::Uuid;

/// PMD control point (read | write | indicate)
pub(crate) const PMD_CP_UUID: Uuid = Uuid::from_u128(0xfb005c81_02e7_f387_1cad_8acd2d8df0c8);

/// PS-FTP MTU characteristic (data transfer)
pub(crate) const PSFTP_MTU_UUID: Uuid = Uuid::from_u128(0xfb005c51_02e7_f387_1cad_8acd2d8df0c8);
/// PS-FTP device-to-host notification characteristic
pub(crate) const PSFTP_D2H_UUID: Uuid = Uuid::from_u128(0xfb005c52_02e7_f387_1cad_8acd2d8df0c8);

/// The measurement types supported by the Verity Sense for offline recording.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MeasurementType {
    /// Photoplethysmography (optical heart rate signal)
    Ppg,
    /// Pulse-to-pulse interval (cardiac pulse-to-pulse interval in ms)
    Ppi,
    /// Accelerometer
    Acc,
    /// Gyroscope
    Gyro,
    /// Magnetometer
    Mag,
}

impl MeasurementType {
    /// The raw PMD measurement type byte
    pub(crate) fn as_u8(&self) -> u8 {
        match self {
            MeasurementType::Ppg => 0x01,
            MeasurementType::Ppi => 0x03,
            MeasurementType::Acc => 0x02,
            MeasurementType::Gyro => 0x05,
            MeasurementType::Mag => 0x06,
        }
    }

    /// Maps an offline recording file name prefix to a measurement type.
    pub(crate) fn from_file_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "PPG" => Some(MeasurementType::Ppg),
            "PPI" => Some(MeasurementType::Ppi),
            "ACC" => Some(MeasurementType::Acc),
            "GYRO" => Some(MeasurementType::Gyro),
            "MAG" => Some(MeasurementType::Mag),
            _ => None,
        }
    }
}
