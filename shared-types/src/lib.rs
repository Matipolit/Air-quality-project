use serde::{Deserialize, Serialize};

// ============================================================================
// Device to Server Messages (ESP32 → Raspberry Pi)
// ============================================================================

/// Main message envelope sent from ESP32 to server
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceMessage {
    /// Device identifier (e.g., "esp32-scd40")
    pub device: String,
    #[serde(flatten)]
    pub payload: DevicePayload,
}

impl DeviceMessage {
    pub fn new(device: impl Into<String>, payload: DevicePayload) -> Self {
        Self {
            device: device.into(),
            payload,
        }
    }

    #[cfg(feature = "std")]
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    #[cfg(feature = "std")]
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// Payload variants for messages from device
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status")]
pub enum DevicePayload {
    #[serde(rename = "success")]
    MeasurementSuccess {
        co2: u16,
        temperature: u32,
        humidity: f32,
    },

    #[serde(rename = "error")]
    Error { detail: String },

    #[serde(rename = "frc_start")]
    FrcStart { target_ppm: u16 },

    #[serde(rename = "frc_warmup_complete")]
    FrcWarmupComplete { detail: String },

    #[serde(rename = "frc_calibrating")]
    FrcCalibrating { target_ppm: u16 },

    #[serde(rename = "frc_success")]
    FrcSuccess { correction: u16 },

    #[serde(rename = "frc_error")]
    FrcError { detail: String },

    #[serde(rename = "set_offset_success")]
    SetOffsetSuccess { offset: f32 },

    #[serde(rename = "set_offset_error")]
    SetOffsetError { detail: String },

    #[serde(rename = "get_offset_success")]
    GetOffsetSuccess { offset: f32 },

    #[serde(rename = "get_offset_error")]
    GetOffsetError { detail: String },

    #[serde(rename = "alive")]
    Alive { uptime_seconds: u64 },
}

// ============================================================================
// Server to Device Commands (Raspberry Pi → ESP32)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd")]
pub enum DeviceCommand {
    #[serde(rename = "noop")]
    NoOp,

    /// Start forced recalibration
    #[serde(rename = "start_frc")]
    StartFrc {
        #[serde(default = "default_frc_ppm")]
        target_ppm: u16,
    },

    #[serde(rename = "set_temp_offset")]
    SetTempOffset { offset: f32 },

    #[serde(rename = "get_temp_offset")]
    GetTempOffset,
}

impl Default for DeviceCommand {
    fn default() -> Self {
        DeviceCommand::NoOp
    }
}

fn default_frc_ppm() -> u16 {
    422
}

impl DeviceCommand {
    #[cfg(feature = "std")]
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    #[cfg(feature = "std")]
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

// ============================================================================
// Helper Constructors
// ============================================================================

impl DevicePayload {
    pub fn measurement(co2: u16, temperature: u32, humidity: f32) -> Self {
        Self::MeasurementSuccess {
            co2,
            temperature,
            humidity,
        }
    }

    pub fn error(detail: impl Into<String>) -> Self {
        Self::Error {
            detail: detail.into(),
        }
    }

    pub fn frc_start(target_ppm: u16) -> Self {
        Self::FrcStart { target_ppm }
    }

    pub fn frc_success(correction: u16) -> Self {
        Self::FrcSuccess { correction }
    }

    pub fn alive(uptime_seconds: u64) -> Self {
        Self::Alive { uptime_seconds }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_measurement_serialization() {
        let msg = DeviceMessage::new("esp32-test", DevicePayload::measurement(450, 22, 45.3));

        let json = msg.to_json().unwrap();
        assert!(json.contains("\"status\":\"success\""));
        assert!(json.contains("\"co2\":450"));

        let deserialized = DeviceMessage::from_json(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_command_deserialization() {
        let json = r#"{"cmd":"start_frc","target_ppm":420}"#;
        let cmd = DeviceCommand::from_json(json).unwrap();

        assert_eq!(cmd, DeviceCommand::StartFrc { target_ppm: 420 });
    }

    #[test]
    fn test_error_message() {
        let msg = DeviceMessage::new("esp32-test", DevicePayload::error("Sensor timeout"));

        let json = msg.to_json().unwrap();
        println!("{}", json);
        assert!(json.contains("\"status\":\"error\""));
        assert!(json.contains("Sensor timeout"));
    }
}
