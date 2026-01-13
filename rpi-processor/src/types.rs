use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct InfluxMeasurementRow {
    pub time: String, // InfluxDB returns RFC3339 string
    pub co2_ppm: f64,
    pub temperature_c: f64,
    pub humidity_percent: f64,
    pub device: String,
}

impl InfluxMeasurementRow {
    pub fn to_measurement_with_time(
        &self,
    ) -> Result<MeasurementWithTime, Box<dyn std::error::Error>> {
        let time_with_timezone = if self.time.ends_with('Z') {
            self.time.clone()
        } else {
            format!("{}Z", self.time)
        };
        Ok(MeasurementWithTime {
            co2: self.co2_ppm as u16,
            temperature: self.temperature_c as f32,
            humidity: self.humidity_percent as f32,
            time: DateTime::parse_from_rfc3339(&time_with_timezone)?.with_timezone(&Utc),
            device: self.device.clone(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct MeasurementWithTime {
    pub co2: u16,
    pub temperature: f32,
    pub humidity: f32,
    pub time: DateTime<Utc>,
    pub device: String,
}
