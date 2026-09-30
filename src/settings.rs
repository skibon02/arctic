//! # Settings
//!
//! PMD measurement settings exchanged with the device over the control point.
//!
//! Settings are encoded as a sequence of `[type(1)][count(1)][values...]`
//! records. Each setting type has a fixed value width. The device reports the
//! available values for a measurement type; the host selects one value per
//! setting when starting a stream.

use crate::Error;
use crate::PolarResult;

/// Setting type: sampling rate in Hz
const SETTING_SAMPLE_RATE: u8 = 0;
/// Setting type: resolution in bits
const SETTING_RESOLUTION: u8 = 1;
/// Setting type: measurement range
const SETTING_RANGE: u8 = 2;
/// Setting type: number of channels
const SETTING_CHANNELS: u8 = 4;
/// Setting type: scaling factor (float, response only)
const SETTING_FACTOR: u8 = 5;

/// A single setting selection: a type and its chosen value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Setting {
    pub(crate) ty: u8,
    pub(crate) value: u32,
}

impl Setting {
    pub(crate) fn new(ty: u8, value: u32) -> Setting {
        Setting { ty, value }
    }

    /// The width of this setting's value in bytes.
    fn value_width(ty: u8) -> usize {
        match ty {
            SETTING_CHANNELS => 1,
            SETTING_SAMPLE_RATE | SETTING_RESOLUTION | SETTING_RANGE => 2,
            SETTING_FACTOR => 4,
            _ => 1,
        }
    }
}

/// The settings available for a measurement type, and the values selected for
/// a stream.
///
/// `available` holds every value the device offers per setting type; `selected`
/// holds the value chosen for each setting type. When starting a stream, only
/// the selected settings are sent. Settings the host does not select use the
/// device defaults.
#[derive(Debug, Clone, Default)]
pub struct StreamSettings {
    pub(crate) available: Vec<(u8, Vec<u32>)>,
    pub(crate) selected: Vec<Setting>,
}

impl StreamSettings {
    /// Returns the available values for a setting type.
    ///
    /// `setting` is the raw PMD setting type: 0 = sampling rate, 1 = resolution,
    /// 2 = range, 4 = channels.
    pub fn available(&self, setting: u8) -> &[u32] {
        self.available
            .iter()
            .find(|(ty, _)| *ty == setting)
            .map(|(_, values)| values.as_slice())
            .unwrap_or(&[])
    }

    /// Returns the available sampling rates in Hz.
    pub fn sample_rates(&self) -> &[u32] {
        self.available(SETTING_SAMPLE_RATE)
    }

    /// Returns the available resolutions in bits.
    pub fn resolutions(&self) -> &[u32] {
        self.available(SETTING_RESOLUTION)
    }

    /// Returns the available ranges.
    pub fn ranges(&self) -> &[u32] {
        self.available(SETTING_RANGE)
    }

    /// Returns the available channel counts.
    pub fn channels(&self) -> &[u32] {
        self.available(SETTING_CHANNELS)
    }

    /// Selects a sampling rate in Hz.
    pub fn set_sample_rate(&mut self, hz: u32) -> &mut Self {
        self.select(SETTING_SAMPLE_RATE, hz)
    }

    /// Selects a resolution in bits.
    pub fn set_resolution(&mut self, bits: u32) -> &mut Self {
        self.select(SETTING_RESOLUTION, bits)
    }

    /// Selects a measurement range.
    pub fn set_range(&mut self, range: u32) -> &mut Self {
        self.select(SETTING_RANGE, range)
    }

    /// Selects the number of channels.
    pub fn set_channels(&mut self, channels: u32) -> &mut Self {
        self.select(SETTING_CHANNELS, channels)
    }

    /// Selects the maximum available value for every setting.
    ///
    /// The device requires a resolution, range, and channel count when starting
    /// most streams, so selecting the maximum of each is a convenient default.
    /// PPI offers no settings and is left unchanged.
    pub fn select_max(&mut self) -> &mut Self {
        let maxima: Vec<(u8, u32)> = self
            .available
            .iter()
            .filter_map(|(ty, values)| values.iter().max().map(|value| (*ty, *value)))
            .collect();
        for (ty, value) in maxima {
            self.select(ty, value);
        }
        self
    }

    fn select(&mut self, ty: u8, value: u32) -> &mut Self {
        self.selected.retain(|s| s.ty != ty);
        self.selected.push(Setting::new(ty, value));
        self
    }

    /// Returns the selected sampling rate, if any.
    pub(crate) fn selected_sample_rate(&self) -> u32 {
        self.selected_value(SETTING_SAMPLE_RATE).unwrap_or(0)
    }

    /// Returns the selected factor, if any. The factor is a response-only
    /// setting used to scale delta-encoded samples.
    pub(crate) fn factor(&self) -> f32 {
        self.selected_value(SETTING_FACTOR)
            .map(f32::from_bits)
            .unwrap_or(1.0)
    }

    fn selected_value(&self, ty: u8) -> Option<u32> {
        self.selected.iter().find(|s| s.ty == ty).map(|s| s.value)
    }

    /// Encodes the selected settings for a start request.
    pub(crate) fn encode_selected(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for setting in &self.selected {
            // The factor is response-only and must not be sent.
            if setting.ty == SETTING_FACTOR {
                continue;
            }
            let width = Setting::value_width(setting.ty);
            out.push(setting.ty);
            out.push(1);
            for i in 0..width {
                out.push(((setting.value >> (i * 8)) & 0xff) as u8);
            }
        }
        out
    }

    /// Parses a settings response into available values.
    pub(crate) fn parse_available(data: &[u8]) -> PolarResult<StreamSettings> {
        let mut available: Vec<(u8, Vec<u32>)> = Vec::new();
        let mut offset = 0;

        while offset + 2 <= data.len() {
            let ty = data[offset];
            let count = data[offset + 1] as usize;
            offset += 2;

            let width = Setting::value_width(ty);
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                if offset + width > data.len() {
                    return Err(Error::InvalidData);
                }
                let mut value = 0u32;
                for i in 0..width {
                    value |= (data[offset + i] as u32) << (i * 8);
                }
                offset += width;
                values.push(value);
            }

            match available.iter_mut().find(|(t, _)| *t == ty) {
                Some((_, existing)) => existing.extend(values),
                None => available.push((ty, values)),
            }
        }

        Ok(StreamSettings {
            available,
            selected: Vec::new(),
        })
    }

    /// Updates the selected settings from a start response, capturing the
    /// factor the device reports for the active stream.
    pub(crate) fn update_from_start_response(&mut self, data: &[u8]) -> PolarResult<()> {
        let response = StreamSettings::parse_available(data)?;
        for (ty, values) in &response.available {
            if let Some(value) = values.first() {
                self.select(*ty, *value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parses_settings_response() {
        // sample_rate: [52], resolution: [16], range: [8], channels: [3].
        let data = [
            0x00, 0x01, 0x34, 0x00, // sample rate 52
            0x01, 0x01, 0x10, 0x00, // resolution 16
            0x02, 0x01, 0x08, 0x00, // range 8
            0x04, 0x01, 0x03, // channels 3
        ];
        let settings = StreamSettings::parse_available(&data).unwrap();
        assert_eq!(settings.sample_rates(), &[52]);
        assert_eq!(settings.resolutions(), &[16]);
        assert_eq!(settings.ranges(), &[8]);
        assert_eq!(settings.channels(), &[3]);
    }

    #[test]
    fn encodes_selected_settings() {
        let mut settings = StreamSettings::default();
        settings.set_sample_rate(52).set_resolution(16).set_range(8);
        let encoded = settings.encode_selected();
        assert_eq!(
            encoded,
            vec![0x00, 0x01, 0x34, 0x00, 0x01, 0x01, 0x10, 0x00, 0x02, 0x01, 0x08, 0x00]
        );
    }

    #[test]
    fn captures_factor_from_start_response() {
        // factor = 0x397FDA40 = 0.000244 as f32.
        let factor_bytes = 0.000244f32.to_bits().to_le_bytes();
        let mut data = vec![0x05, 0x01];
        data.extend_from_slice(&factor_bytes);
        let mut settings = StreamSettings::default();
        settings.update_from_start_response(&data).unwrap();
        assert!((settings.factor() - 0.000244).abs() < 1e-9);
    }
}
