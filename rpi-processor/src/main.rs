mod anomalies;

use circular_queue::CircularQueue;
use rumqttc::{Client, Event, MqttOptions, Packet};
use shared_types::{DeviceMessage, DevicePayload};
use std::time::SystemTime;
use std::{env, time::Duration};

use log::{self, debug, error, info};

pub struct MeasurementWithTime {
    co2: u16,
    temperature: u32,
    humidity: f32,
    time: SystemTime,
}

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
    dotenvy::dotenv().ok();
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let mut measurement_queue: CircularQueue<MeasurementWithTime> =
        CircularQueue::with_capacity(10);

    let mqtt_host = env::var("MQTT_BROKER_HOST").unwrap_or_else(|_| "localhost".to_string());
    let mqtt_port: u16 = env::var("MQTT_BROKER_PORT")
        .unwrap_or_else(|_| "1883".to_string())
        .parse()
        .expect("MQTT_BROKER_PORT must be a valid u16");
    let mqtt_client_id =
        env::var("MQTT_CLIENT_ID").unwrap_or_else(|_| "raspberry-pi-receiver".to_string());
    let mqtt_topic = env::var("MQTT_TOPIC").unwrap_or_else(|_| "sensors/esp32/sensor".to_string());

    let mut mqttoptions = MqttOptions::new(mqtt_client_id, &mqtt_host, mqtt_port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));
    mqttoptions.set_clean_session(true);

    info!("Connecting to MQTT broker at {}:{}", &mqtt_host, mqtt_port);
    let (client, mut connection) = Client::new(mqttoptions, 10);
    info!("Waiting for connection...\n");

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
                                        let now = SystemTime::now();
                                        info!("Received measurement success");
                                        info!("CO2: {}", co2);
                                        info!("Temperature: {}", temperature);
                                        info!("Humidity: {}", humidity);
                                        measurement_queue.push(MeasurementWithTime {
                                            co2,
                                            temperature,
                                            humidity,
                                            time: now,
                                        });
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
                                    DevicePayload::FrcStart { target_ppm } => {
                                        info!(
                                            "Force recalibration started with target ppm: {}",
                                            target_ppm
                                        );
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
                                    DevicePayload::SetDeepSleepTimeSuccess { seconds } => {
                                        info!(
                                            "Set deep sleep time successful with seconds: {}",
                                            seconds
                                        );
                                    }
                                    DevicePayload::GetDeepSleepTimeSuccess { seconds } => {
                                        info!(
                                            "Get deep sleep time successful with seconds: {}",
                                            seconds
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

            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!("✓ Connected to MQTT broker");
                info!("Subscribing to mqtt topic {}", mqtt_topic);
                client
                    .subscribe(&mqtt_topic, rumqttc::QoS::AtLeastOnce)
                    .expect("Could not subscribe to the MQTT topic.");
            }
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
