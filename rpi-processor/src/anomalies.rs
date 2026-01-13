use std::{collections::VecDeque, fmt::Display};

use crate::types::MeasurementWithTime;

#[derive(Clone, Debug)]
pub struct AnomalyConfig {
    pub z_score_temp: f32,
    pub z_score_humidity: f32,
    pub z_score_co2: f32,
    // Minimum absolute difference required to trigger an anomaly
    pub min_humidity_diff: f32,
    pub min_temp_diff: f32,
    pub min_co2_diff: f32,
    // how many past measurements to consider in the analysis window
    pub window_size: usize,
}

impl Default for AnomalyConfig {
    fn default() -> Self {
        Self {
            z_score_temp: 3.0,
            z_score_humidity: 1.0,
            z_score_co2: 6.0,
            window_size: 20,
            min_humidity_diff: 5.0,
            min_temp_diff: 2.0,
            min_co2_diff: 50.0,
        }
    }
}

pub struct AnomalyFlags {
    pub temperature_spike: bool,
    pub humidity_spike: bool,
    pub co2_spike: bool,
    pub physical_constraint_temp_violation: bool,
    pub physical_constraint_humidity_violation: bool,
    pub physical_constraint_co2_violation: bool,
    pub possible_sunlight: bool,
}

impl AnomalyFlags {
    pub fn is_any_true(&self) -> bool {
        self.temperature_spike
            || self.humidity_spike
            || self.co2_spike
            || self.physical_constraint_temp_violation
            || self.physical_constraint_humidity_violation
            || self.physical_constraint_co2_violation
            || self.possible_sunlight
    }
}

impl Default for AnomalyFlags {
    fn default() -> Self {
        Self {
            temperature_spike: false,
            humidity_spike: false,
            co2_spike: false,
            physical_constraint_temp_violation: false,
            physical_constraint_humidity_violation: false,
            physical_constraint_co2_violation: false,
            possible_sunlight: false,
        }
    }
}

impl Display for AnomalyFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut result_string = "".to_owned();
        if self.temperature_spike {
            result_string.push_str("Temperature Spike");
        } else if self.humidity_spike {
            result_string.push_str("Humidity Spike");
        } else if self.co2_spike {
            result_string.push_str("CO2 Spike");
        } else if self.physical_constraint_temp_violation {
            result_string.push_str("Physical Constraint Temp Violation");
        } else if self.physical_constraint_humidity_violation {
            result_string.push_str("Physical Constraint Humidity Violation");
        } else if self.physical_constraint_co2_violation {
            result_string.push_str("Physical Constraint CO2 Violation");
        } else if self.possible_sunlight {
            result_string.push_str("Possible Sunlight");
        }
        write!(f, "{}", result_string)
    }
}

pub fn analyse_measurements_window(
    measurements: &VecDeque<MeasurementWithTime>,
    config: &AnomalyConfig,
    debug_info: bool,
) -> AnomalyFlags {
    let mut anomaly_flags = AnomalyFlags::default();

    // We need at least a few data points to calculate a meaningful average
    if measurements.len() < 10 {
        return anomaly_flags;
    }

    // Get the most recent measurement (the one we are testing)
    let current = measurements.back().unwrap();

    // Use only the most recent history for the baseline to adapt quickly to changes.
    // We skip the last item (current) and take the preceding ANALYSIS_WINDOW_SIZE items.
    // We use rev() to start from the end (newest) and go backwards.
    let history_iter = measurements.iter().rev().skip(1).take(config.window_size);

    // Count actual items available (in case total measurements < ANALYSIS_WINDOW_SIZE + 1)
    let history_count = history_iter.clone().count();

    if history_count < 5 {
        return anomaly_flags;
    }

    let history_len_f32 = history_count as f32;

    // Calculate Means
    let sum_temp: f32 = history_iter.clone().map(|m| m.temperature as f32).sum();
    let sum_hum: f32 = history_iter.clone().map(|m| m.humidity as f32).sum();
    let sum_co2: f32 = history_iter.clone().map(|m| m.co2 as f32).sum();

    let mean_temp = sum_temp / history_len_f32;
    let mean_hum = sum_hum / history_len_f32;
    let mean_co2 = sum_co2 / history_len_f32;

    // Calculate Standard Deviations
    let variance_temp: f32 = history_iter
        .clone()
        .map(|m| (m.temperature as f32 - mean_temp).powi(2))
        .sum::<f32>()
        / history_len_f32;
    let std_dev_temp = variance_temp.sqrt();

    let variance_hum: f32 = history_iter
        .clone()
        .map(|m| (m.humidity as f32 - mean_hum).powi(2))
        .sum::<f32>()
        / history_len_f32;
    let std_dev_hum = variance_hum.sqrt();

    let variance_co2: f32 = history_iter
        .clone()
        .map(|m| (m.co2 as f32 - mean_co2).powi(2))
        .sum::<f32>()
        / history_len_f32;
    let std_dev_co2 = variance_co2.sqrt();

    // Helper closure to check for anomaly
    let check_anomaly = |current_val: f32,
                         mean: f32,
                         std_dev: f32,
                         min_diff: f32,
                         z_score_threshold: f32|
     -> bool {
        let diff = (current_val - mean).abs();
        // It is an anomaly if:
        // 1. The absolute difference is significant (greater than min_diff)
        // 2. AND the value is statistically rare (Z-score > threshold)
        diff > min_diff && diff > (std_dev * z_score_threshold)
    };

    if check_anomaly(
        current.temperature as f32,
        mean_temp,
        std_dev_temp,
        config.min_temp_diff,
        config.z_score_temp,
    ) {
        anomaly_flags.temperature_spike = true;
        if debug_info {
            log::debug!(
                "Temp Anomaly: Val={}, Mean={:.2}, StdDev={:.2}",
                current.temperature,
                mean_temp,
                std_dev_temp
            );
        }
    }

    if check_anomaly(
        current.humidity as f32,
        mean_hum,
        std_dev_hum,
        config.min_humidity_diff,
        config.z_score_humidity,
    ) {
        anomaly_flags.humidity_spike = true;
        if debug_info {
            log::debug!(
                "Hum Anomaly: Val={}, Mean={:.2}, StdDev={:.2}",
                current.humidity,
                mean_hum,
                std_dev_hum
            );
        }
    }

    if check_anomaly(
        current.co2 as f32,
        mean_co2,
        std_dev_co2,
        config.min_co2_diff,
        config.z_score_co2,
    ) {
        anomaly_flags.co2_spike = true;
        if debug_info {
            log::debug!(
                "CO2 Anomaly: Val={}, Mean={:.2}, StdDev={:.2}",
                current.co2,
                mean_co2,
                std_dev_co2
            );
        }
    }

    return anomaly_flags;
}
