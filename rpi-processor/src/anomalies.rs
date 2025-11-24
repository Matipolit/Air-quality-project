use std::{collections::VecDeque, fmt::Display};

use chrono::Timelike;
use circular_queue::CircularQueue;

use crate::MeasurementWithTime;

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

const SUNLIGHT_DETECTION_SCOPE: u32 = 2;
const TEMP_1H_ANOMALY_THRESHOLD: f32 = 6.0;
const HUMIDITY_1H_ANOMALY_THRESHOLD: f32 = 20.0;
const CO2_1H_ANOMALY_THRESHOLD: f32 = 40.0;

fn get_values_from_time_window(
    data: impl Iterator<Item = MeasurementWithTime>,
    hours_to_include: u32,
) -> Vec<MeasurementWithTime> {
    let data_vec: Vec<MeasurementWithTime> = data.collect();
    let cutoff_time = data_vec
        .last()
        .unwrap()
        .time
        .checked_sub_signed(chrono::Duration::hours(hours_to_include as i64))
        .unwrap_or_default();
    data_vec
        .into_iter()
        .filter(|m: &MeasurementWithTime| m.time >= cutoff_time)
        .collect()
}

pub fn analyse_measurements_window(
    measurements: VecDeque<MeasurementWithTime>,
    debug_info: bool,
) -> AnomalyFlags {
    let mut anomaly_flags = AnomalyFlags::default();

    if debug_info {
        log::debug!(
            "Window size: {} | First measurement date: {} | Last measurement date: {}",
            measurements.len(),
            measurements.front().unwrap().time,
            measurements.back().unwrap().time
        );
    }

    let measurements_1h_scope =
        get_values_from_time_window(measurements.iter().cloned(), SUNLIGHT_DETECTION_SCOPE);

    if debug_info {
        log::debug!("{:?}", measurements_1h_scope);
    }

    if measurements_1h_scope.len() > 1 {
        let mut measurements_1h_iter = measurements_1h_scope.iter();

        let first_measurement_opt = measurements_1h_iter.next().cloned();
        let last_measurement_opt = measurements_1h_iter.last().cloned();

        if let Some(first_measurement) = first_measurement_opt {
            if let Some(last_measurement) = last_measurement_opt {
                if (first_measurement.temperature as f32 - last_measurement.temperature as f32)
                    .abs()
                    > TEMP_1H_ANOMALY_THRESHOLD
                {
                    anomaly_flags.temperature_spike = true;
                }

                if (first_measurement.humidity as f32 - last_measurement.humidity as f32).abs()
                    > HUMIDITY_1H_ANOMALY_THRESHOLD
                {
                    anomaly_flags.humidity_spike = true;
                }

                if (first_measurement.co2 as f32 - last_measurement.co2 as f32).abs()
                    > CO2_1H_ANOMALY_THRESHOLD
                {
                    anomaly_flags.co2_spike = true;
                }
            }
        } else {
            log::warn!("No measurements found for the last hour");
        }
    }
    return anomaly_flags;
}
