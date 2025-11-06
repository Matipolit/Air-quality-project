use std::{env, time::Duration};

use rumqttc::{Client, Event, MqttOptions, Packet};
use shared_types::{DeviceMessage, DevicePayload};

use log::{self, debug, error, info};

// MQTT Configuration
const MQTT_BROKER_HOST: &str = "localhost";
const MQTT_BROKER_PORT: u16 = 1883;
const MQTT_CLIENT_ID: &str = "raspberry-pi-receiver";
const MQTT_TOPIC: &str = "sensors/esp32/sensor";

pub async fn save_measurement_to_influx(device: &str, co2: u16, temperature: u32, humidity: f32) {
    let host = env::var("INFLUXDB_URL").expect("INFLUXDB_URL must be set");
    let token = env::var("INFLUXDB_TOKEN").expect("INFLUXDB_TOKEN must be set");
    let database = env::var("INFLUXDB_DATABASE").expect("INFLUXDB_DATABASE must be set");

    let line_protocol = format!(
        "scd40_data,device={} co2_ppm={},temperature_c={},humidity_percent={}",
        device, co2, temperature, humidity
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&format!("{}/api/v3/write_lp?db={}", host, database))
        .body(line_protocol)
        .bearer_auth(token)
        .send()
        .await
        .expect("Failed to send measurement to InfluxDB");

    if !response.status().is_success() {
        eprintln!(
            "Failed to save measurement to InfluxDB: {} - {}",
            response.status(),
            response.text().await.expect("Failed to get response text")
        );
    } else {
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let mut mqttoptions = MqttOptions::new(MQTT_CLIENT_ID, MQTT_BROKER_HOST, MQTT_BROKER_PORT);
    mqttoptions.set_keep_alive(Duration::from_secs(30));
    mqttoptions.set_clean_session(true);

    info!(
        "Connecting to MQTT broker at {}:{}",
        MQTT_BROKER_HOST, MQTT_BROKER_PORT
    );
    let (client, mut connection) = Client::new(mqttoptions, 10);
    info!("Subscribing to mqtt topic");
    client
        .subscribe(MQTT_TOPIC, rumqttc::QoS::AtLeastOnce)
        .expect("Could not connect to the MQTT topic.");
    info!("✓ Connected and subscribed! Waiting for messages...\n");

    loop {
        match connection.eventloop.poll().await {
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                let topic = &publish.topic;
                let payload = &publish.payload;

                match std::str::from_utf8(payload) {
                    Ok(str_message) => {
                        info!("Received message on topic '{}'", topic);
                        debug!("Raw message content: {}", str_message);

                        match serde_json::from_str::<DeviceMessage>(str_message) {
                            Ok(device_message) => {
                                let device = &device_message.device;
                                debug!("Decoded message: {:?}", &device_message);
                                match device_message.payload {
                                    DevicePayload::MeasurementSuccess {
                                        co2,
                                        temperature,
                                        humidity,
                                    } => {
                                        info!("Received measurement success");
                                        info!("CO2: {}", co2);
                                        info!("Temperature: {}", temperature);
                                        info!("Humidity: {}", humidity);
                                        save_measurement_to_influx(
                                            device,
                                            co2,
                                            temperature,
                                            humidity,
                                        )
                                        .await;
                                        info!("Measurement saved to InfluxDB");
                                    }
                                    DevicePayload::Error { detail } => {
                                        error!("Error: {}", detail);
                                    }
                                    DevicePayload::FrcStart { detail } => {
                                        info!("Force recalibration started: {}", detail);
                                    }
                                    DevicePayload::FrcWarmupComplete { detail } => {
                                        info!("Force recalibration warmup complete: {}", detail);
                                    }
                                    DevicePayload::FrcCalibrating { target_ppm } => {
                                        info!(
                                            "Force recalibration calibrating to target ppm: {}",
                                            target_ppm
                                        );
                                    }
                                    DevicePayload::FrcSuccess { correction } => {
                                        info!(
                                            "Force recalibration successful with correction: {}",
                                            correction
                                        );
                                    }
                                    DevicePayload::FrcError { detail } => {
                                        error!("Force recalibration error: {}", detail);
                                    }
                                    DevicePayload::SetOffsetSuccess { offset } => {
                                        info!(
                                            "Set temperature offset successful with offset: {}",
                                            offset
                                        );
                                    }
                                    DevicePayload::SetOffsetError { detail } => {
                                        error!("Set temperature offset error: {}", detail);
                                    }
                                    DevicePayload::GetOffsetSuccess { offset } => {
                                        info!(
                                            "Get temperature offset successful with offset: {}",
                                            offset
                                        );
                                    }
                                    DevicePayload::GetOffsetError { detail } => {
                                        error!("Get temperature offset error: {}", detail);
                                    }
                                    DevicePayload::Alive { uptime_seconds } => {
                                        info!(
                                            "Device is alive with uptime: {} seconds",
                                            uptime_seconds
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                error!("❌ Failed to decode message payload: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("❌ Failed to decode message payload: {:?}", e);
                    }
                }
            }

            Ok(Event::Incoming(Packet::ConnAck(_))) => info!("✓ Connected to MQTT broker"),
            Ok(Event::Incoming(Packet::SubAck(_))) => info!("✓ Subscription confirmed"),
            Err(e) => {
                error!("❌ Connection error: {:?}", e);
                error!("Retrying in 5 seconds...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            _ => {} // Ignoruj inne zdarzenia
        }
    }
}
