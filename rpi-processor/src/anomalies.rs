use std::fmt::Display;

use chrono::{DateTime, Datelike, Timelike, Utc};

use crate::types::MeasurementWithTime;

#[derive(Clone, Debug)]
pub struct AnomalyConfig {
    // Humidity thresholds
    /// Humidity below this is definitely anomalous (sunlight dip)
    pub humidity_definite_anomaly: f32, // 55%
    /// Humidity below this is suspicious, needs temp confirmation
    pub humidity_suspicious: f32, // 65%

    // Temperature thresholds
    /// If current temp is this many degrees above daily minimum, it's a spike
    pub temp_above_daily_min: f32, // 8°C
    /// Absolute minimum temp to consider as a spike (avoid flagging cold days)
    pub temp_absolute_min_for_spike: f32, // 12°C

    // Time constraints
    /// Earliest hour for sunlight detection (24h format)
    pub daylight_start_hour: u32, // 6
    /// Latest hour for sunlight detection (24h format)
    pub daylight_end_hour: u32, // 18

    // CO2 thresholds
    /// CO2 above this is anomalous
    pub co2_spike_threshold: f32, // 700 ppm
}

impl Default for AnomalyConfig {
    fn default() -> Self {
        Self {
            humidity_definite_anomaly: 55.0,
            humidity_suspicious: 65.0,
            temp_above_daily_min: 8.0,
            temp_absolute_min_for_spike: 12.0,
            daylight_start_hour: 6,
            daylight_end_hour: 18,
            co2_spike_threshold: 700.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnomalyFlags {
    pub temperature_spike: bool,
    pub humidity_spike: bool,
    pub co2_spike: bool,
    pub possible_sunlight: bool,
    // Legacy fields for compatibility
    pub physical_constraint_temp_violation: bool,
    pub physical_constraint_humidity_violation: bool,
    pub physical_constraint_co2_violation: bool,
}

impl AnomalyFlags {
    pub fn is_any_true(&self) -> bool {
        self.temperature_spike || self.humidity_spike || self.co2_spike || self.possible_sunlight
    }
}

impl Default for AnomalyFlags {
    fn default() -> Self {
        Self {
            temperature_spike: false,
            humidity_spike: false,
            co2_spike: false,
            possible_sunlight: false,
            physical_constraint_temp_violation: false,
            physical_constraint_humidity_violation: false,
            physical_constraint_co2_violation: false,
        }
    }
}

impl Display for AnomalyFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.possible_sunlight {
            parts.push("Sunlight");
        }
        if self.temperature_spike {
            parts.push("TempSpike");
        }
        if self.humidity_spike {
            parts.push("HumidityDip");
        }
        if self.co2_spike {
            parts.push("CO2Spike");
        }
        if parts.is_empty() {
            write!(f, "None")
        } else {
            write!(f, "{}", parts.join(", "))
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DailyStats {
    pub date: Option<(i32, u32)>, // (year, day_of_year)
    pub temp_min: f32,
    pub temp_max: f32,
    pub humidity_min: f32,
    pub humidity_max: f32,
    pub measurement_count: usize,
}

impl DailyStats {
    pub fn new() -> Self {
        Self {
            date: None,
            temp_min: f32::MAX,
            temp_max: f32::MIN,
            humidity_min: f32::MAX,
            humidity_max: f32::MIN,
            measurement_count: 0,
        }
    }

    pub fn update(&mut self, measurement: &MeasurementWithTime) {
        let current_date = (measurement.time.year(), measurement.time.ordinal());

        // Reset if new day
        if self.date != Some(current_date) {
            self.date = Some(current_date);
            self.temp_min = measurement.temperature;
            self.temp_max = measurement.temperature;
            self.humidity_min = measurement.humidity;
            self.humidity_max = measurement.humidity;
            self.measurement_count = 1;
        } else {
            self.temp_min = self.temp_min.min(measurement.temperature);
            self.temp_max = self.temp_max.max(measurement.temperature);
            self.humidity_min = self.humidity_min.min(measurement.humidity);
            self.humidity_max = self.humidity_max.max(measurement.humidity);
            self.measurement_count += 1;
        }
    }
}

pub struct AnomalyDetector {
    pub config: AnomalyConfig,
    /// Stats for current day
    current_day_stats: DailyStats,
    /// Stats from previous days (for baseline comparison)
    /// Key: (year, day_of_year), Value: DailyStats
    historical_daily_stats: Vec<DailyStats>,
    /// Recent measurements for context (last 2 hours = ~30 measurements at 4-min interval)
    recent_measurements: Vec<MeasurementWithTime>,
}

impl AnomalyDetector {
    pub fn new() -> Self {
        Self {
            config: AnomalyConfig::default(),
            current_day_stats: DailyStats::new(),
            historical_daily_stats: Vec::new(),
            recent_measurements: Vec::with_capacity(50),
        }
    }

    pub fn with_config(config: AnomalyConfig) -> Self {
        Self {
            config,
            current_day_stats: DailyStats::new(),
            historical_daily_stats: Vec::new(),
            recent_measurements: Vec::with_capacity(50),
        }
    }

    /// Pre-compute daily stats from historical data
    pub fn build_profile(&mut self, measurements: &[MeasurementWithTime]) {
        self.historical_daily_stats.clear();

        let mut current_stats = DailyStats::new();

        for m in measurements {
            let m_date = (m.time.year(), m.time.ordinal());

            if current_stats.date != Some(m_date) {
                // Save previous day's stats if we have data
                if current_stats.measurement_count > 0 {
                    self.historical_daily_stats.push(current_stats.clone());
                }
                current_stats = DailyStats::new();
            }

            current_stats.update(m);
        }

        // Don't forget the last day
        if current_stats.measurement_count > 0 {
            self.historical_daily_stats.push(current_stats);
        }

        log::info!(
            "Built profile from {} days of historical data",
            self.historical_daily_stats.len()
        );
    }

    /// Get the baseline temperature for comparison
    /// Uses the minimum temp from before the current hour (pre-sunlight baseline)
    fn get_pre_sunlight_baseline(&self, current_time: DateTime<Utc>) -> Option<f32> {
        // Find measurements from earlier today (before potential sunlight)
        // Use measurements from midnight to 7 AM as baseline
        let baseline_temps: Vec<f32> = self
            .recent_measurements
            .iter()
            .filter(|m| {
                m.time.ordinal() == current_time.ordinal()
                    && m.time.year() == current_time.year()
                    && m.time.hour() < 7
            })
            .map(|m| m.temperature)
            .collect();

        if baseline_temps.len() >= 3 {
            // Use median of early morning temps
            let mut temps = baseline_temps;
            temps.sort_by(|a, b| a.partial_cmp(b).unwrap());
            return Some(temps[temps.len() / 2]);
        }

        // Fall back: use minimum from recent measurements
        if self.recent_measurements.len() >= 10 {
            return self
                .recent_measurements
                .iter()
                .map(|m| m.temperature)
                .min_by(|a, b| a.partial_cmp(b).unwrap());
        }

        None
    }

    /// Analyze a single measurement
    pub fn analyze(&mut self, measurement: &MeasurementWithTime, debug: bool) -> AnomalyFlags {
        let mut flags = AnomalyFlags::default();

        // Update tracking
        self.current_day_stats.update(measurement);
        self.recent_measurements.push(measurement.clone());

        // Keep only last 3 hours of measurements (~45 at 4-min interval)
        let cutoff = measurement.time - chrono::Duration::hours(3);
        self.recent_measurements.retain(|m| m.time > cutoff);

        let hour = measurement.time.hour();
        let is_daylight_hours =
            hour >= self.config.daylight_start_hour && hour <= self.config.daylight_end_hour;

        let temp = measurement.temperature;
        let humidity = measurement.humidity;
        let co2 = measurement.co2 as f32;

        if humidity <= self.config.humidity_definite_anomaly {
            flags.humidity_spike = true;
            if is_daylight_hours {
                flags.possible_sunlight = true;
            }
            if debug {
                log::debug!(
                    "Definite humidity anomaly: {:.1}% <= {:.1}%",
                    humidity,
                    self.config.humidity_definite_anomaly
                );
            }
        }

        if humidity <= self.config.humidity_suspicious && !flags.humidity_spike {
            // Get baseline temp for comparison
            let baseline_temp = self.get_pre_sunlight_baseline(measurement.time);

            if let Some(baseline) = baseline_temp {
                let temp_rise = temp - baseline;

                // If temp is elevated above baseline, confirm as sunlight
                if temp_rise >= self.config.temp_above_daily_min
                    && temp >= self.config.temp_absolute_min_for_spike
                {
                    flags.humidity_spike = true;
                    flags.temperature_spike = true;
                    if is_daylight_hours {
                        flags.possible_sunlight = true;
                    }
                    if debug {
                        log::debug!(
                            "Humidity + Temp anomaly: humidity {:.1}%, temp {:.1}°C (+{:.1}°C from baseline {:.1}°C)",
                            humidity,
                            temp,
                            temp_rise,
                            baseline
                        );
                    }
                }
            } else if is_daylight_hours {
                // No baseline available, but suspicious humidity during daylight
                // Check if temp is absolutely high
                if temp >= self.config.temp_absolute_min_for_spike {
                    flags.humidity_spike = true;
                    if debug {
                        log::debug!(
                            "Suspicious humidity during daylight: {:.1}% with temp {:.1}°C",
                            humidity,
                            temp
                        );
                    }
                }
            }
        }

        if !flags.temperature_spike {
            if let Some(baseline) = self.get_pre_sunlight_baseline(measurement.time) {
                let temp_rise = temp - baseline;

                if temp_rise >= self.config.temp_above_daily_min
                    && temp >= self.config.temp_absolute_min_for_spike
                    && is_daylight_hours
                {
                    flags.temperature_spike = true;
                    if debug {
                        log::debug!(
                            "Temperature spike: {:.1}°C (+{:.1}°C from baseline {:.1}°C)",
                            temp,
                            temp_rise,
                            baseline
                        );
                    }
                }
            }
        }

        if co2 >= self.config.co2_spike_threshold {
            flags.co2_spike = true;
            if debug {
                log::debug!(
                    "CO2 spike: {:.0} ppm >= {:.0} ppm threshold",
                    co2,
                    self.config.co2_spike_threshold
                );
            }
        }

        if flags.temperature_spike && flags.humidity_spike && is_daylight_hours {
            flags.possible_sunlight = true;
        }

        flags
    }
}

impl Default for AnomalyDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct BatchAnalysisResult {
    pub total_measurements: usize,
    pub anomalies_detected: usize,
    pub sunlight_events: usize,
    pub anomaly_timestamps: Vec<(DateTime<Utc>, AnomalyFlags, String)>,
}

/// Analyze a batch of historical measurements
pub fn analyze_historical_data(
    measurements: &[MeasurementWithTime],
    config: Option<AnomalyConfig>,
) -> BatchAnalysisResult {
    let config = config.unwrap_or_default();

    let mut detector = AnomalyDetector::with_config(config);

    // Build profile from all data first (to get daily min/max stats)
    detector.build_profile(measurements);

    let mut result = BatchAnalysisResult {
        total_measurements: measurements.len(),
        anomalies_detected: 0,
        sunlight_events: 0,
        anomaly_timestamps: Vec::new(),
    };

    for (idx, m) in measurements.iter().enumerate() {
        let debug = idx > 0 && idx % 5000 == 0;
        if debug {
            log::info!("Analyzed {} / {} measurements...", idx, measurements.len());
        }

        let flags = detector.analyze(m, false);

        if flags.is_any_true() {
            result.anomalies_detected += 1;
            if flags.possible_sunlight {
                result.sunlight_events += 1;
            }
            result
                .anomaly_timestamps
                .push((m.time, flags, m.device.clone()));
        }
    }

    log::info!(
        "Batch analysis complete: {} anomalies ({} sunlight) out of {} measurements",
        result.anomalies_detected,
        result.sunlight_events,
        result.total_measurements
    );

    result
}
