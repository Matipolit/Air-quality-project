mod anomalies;

use chrono::{DateTime, Utc};
use circular_queue::CircularQueue;
use rumqttc::{Client, Event, MqttOptions, Packet};
use shared_types::{DeviceMessage, DevicePayload};
use std::collections::VecDeque;
use std::time::SystemTime;
use std::{env, time::Duration};

use log::{self, debug, error, info};

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Mark historical measurements from influxDB for anomalies
    #[arg(short, long, default_value_t = false)]
    mark_historical_data: bool,

    /// Delete old markings from influxDB for anomalies
    #[arg(short, long, default_value_t = false)]
    delete_old_markings: bool,

    /// Receive live data from MQTT broker and save it to influxDB
    #[arg(short, long, default_value_t = false)]
    receive_live_data: bool,
}

#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
struct InfluxMeasurementRow {
    time: String, // InfluxDB returns RFC3339 string
    co2_ppm: f64,
    temperature_c: f64,
    humidity_percent: f64,
    device: String,
}

impl InfluxMeasurementRow {
    fn to_measurement_with_time(&self) -> Result<MeasurementWithTime, Box<dyn std::error::Error>> {
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

pub async fn mark_historical_data(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let query_url = format!("{}/api/v3/query_sql?db={}", influx_host, influx_database);
    log::debug!("Query URL: {}", query_url);
    // SQL query to get all measurements ordered by time
    let sql_query = r#"
        SELECT
            time,
            co2_ppm,
            temperature_c,
            humidity_percent,
            device
        FROM scd40_data
        ORDER BY time ASC
    "#;

    // Make the request
    // Make the request
    let response = reqwest_client
        .post(&query_url)
        .bearer_auth(influx_token)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&serde_json::json!({
            "db": influx_database,
            "q": sql_query
        }))?)
        .send()
        .await?;

    // Check response status first
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await?;
        return Err(format!(
            "InfluxDB query failed with status {}: {}",
            status, error_text
        )
        .into());
    }

    // Parse the response
    let response_text = response.text().await?;
    log::info!("Received response of {} bytes", response_text.len());
    log::debug!(
        "First 500 chars: {}",
        &response_text.chars().take(500).collect::<String>()
    );
    log::debug!(
        "Last 200 chars: {}",
        &response_text.chars().rev().take(200).collect::<String>()
    );

    // Check if response is empty
    if response_text.is_empty() {
        log::warn!("Received empty response from InfluxDB");
        return Ok(());
    }

    // Check if response is valid JSON
    if !response_text.starts_with('[') || !response_text.ends_with(']') {
        log::error!("Received invalid JSON response from InfluxDB");
        return Err("Invalid JSON response".into());
    }

    log::info!("Parsing JSON response");
    let influx_rows: Vec<InfluxMeasurementRow> = serde_json::from_str(&response_text)?;
    log::info!("Parsed {} rows", influx_rows.len());

    // With this:
    log::info!(
        "Converting {} rows to MeasurementWithTime...",
        influx_rows.len()
    );
    let mut measurements = Vec::with_capacity(influx_rows.len());

    for (idx, row) in influx_rows.iter().enumerate() {
        match row.to_measurement_with_time() {
            Ok(measurement) => measurements.push(measurement),
            Err(e) => {
                log::error!(
                    "Failed to convert row {} to MeasurementWithTime: {}",
                    idx,
                    e
                );
                log::error!("Problematic row: {:?}", row);
                return Err(format!("Row {} conversion failed: {}", idx, e).into());
            }
        }

        // Progress indicator for large datasets
        if idx > 0 && idx % 1000 == 0 {
            log::debug!("Converted {} / {} rows...", idx, influx_rows.len());
        }
    }

    log::info!("Received {} measurements", measurements.len());

    // Sliding window anomaly detection
    let window_size = 300;
    let batch_size = 100; // Write anomalies in batches
    let mut window: VecDeque<MeasurementWithTime> = VecDeque::with_capacity(window_size);
    let mut anomaly_batch = Vec::new();
    let mut total_anomalies = 0;

    for (idx, m) in measurements.iter().enumerate() {
        window.push_back(m.clone());
        if window.len() > window_size {
            window.pop_front();
        }

        let anomalies = if idx > 0 && idx % 1000 == 0 {
            log::debug!("Analysed {} / {} rows...", idx, measurements.len());
            anomalies::analyse_measurements_window(window.clone(), true)
        } else {
            anomalies::analyse_measurements_window(window.clone(), false)
        };

        if anomalies.is_any_true() {
            log::warn!("Anomalies detected in measurement from time: {:?}", m.time);
            log::warn!("{}", anomalies);

            // Add to batch
            anomaly_batch.push((m.time, anomalies, m.device.clone()));
            total_anomalies += 1;

            // Write batch if it reaches batch_size
            if anomaly_batch.len() >= batch_size {
                save_anomalies_batch(
                    influx_host,
                    influx_token,
                    influx_database,
                    reqwest_client,
                    &anomaly_batch,
                )
                .await?;
                log::info!(
                    "Wrote batch of {} anomalies to InfluxDB",
                    anomaly_batch.len()
                );
                anomaly_batch.clear();
            }
        }
    }

    // Write remaining anomalies
    if !anomaly_batch.is_empty() {
        save_anomalies_batch(
            influx_host,
            influx_token,
            influx_database,
            reqwest_client,
            &anomaly_batch,
        )
        .await?;
        log::info!(
            "Wrote final batch of {} anomalies to InfluxDB",
            anomaly_batch.len()
        );
    }

    log::info!(
        "✓ Anomaly detection complete: {} total anomalies found and saved",
        total_anomalies
    );
    Ok(())
}

async fn save_anomalies_batch(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
    anomalies: &[(DateTime<Utc>, anomalies::AnomalyFlags, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    if anomalies.is_empty() {
        return Ok(());
    }

    // Build line protocol for all anomalies
    let mut line_protocol_lines = Vec::new();

    for (timestamp, flags, device) in anomalies {
        // Convert timestamp to Unix nanoseconds
        let timestamp_nanos = timestamp.timestamp_nanos_opt().unwrap_or(0);

        // Build line protocol: measurement,tags fields timestamp
        let line = format!(
            "anomalies,device={} temperature_spike={},humidity_spike={},co2_spike={},physical_constraint_temp_violation={},physical_constraint_humidity_violation={},physical_constraint_co2_violation={},possible_sunlight={} {}",
            device,
            flags.temperature_spike,
            flags.humidity_spike,
            flags.co2_spike,
            flags.physical_constraint_temp_violation,
            flags.physical_constraint_humidity_violation,
            flags.physical_constraint_co2_violation,
            flags.possible_sunlight,
            timestamp_nanos
        );
        line_protocol_lines.push(line);
    }

    // Join all lines with newlines
    let batch_body = line_protocol_lines.join("\n");

    // Write to InfluxDB
    let response = reqwest_client
        .post(&format!(
            "{}/api/v3/write_lp?db={}",
            influx_host, influx_database
        ))
        .body(batch_body)
        .bearer_auth(influx_token)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await?;
        return Err(format!(
            "Failed to write anomalies to InfluxDB: {} - {}",
            status, error_text
        )
        .into());
    }

    Ok(())
}

pub async fn delete_old_markings(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("Deleting old anomaly markings from database...");

    let query_url = format!("{}/api/v3/query_sql?db={}", influx_host, influx_database);

    // SQL query to delete all records from the anomalies measurement
    let sql_query = "DELETE FROM anomalies";

    let response = reqwest_client
        .post(&query_url)
        .bearer_auth(influx_token)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&serde_json::json!({
            "db": influx_database,
            "q": sql_query
        }))?)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await?;
        return Err(format!(
            "InfluxDB delete failed with status {}: {}",
            status, error_text
        )
        .into());
    }

    log::info!("Successfully deleted all anomaly markings");
    Ok(())
}

#[derive(Debug, Clone)]
pub struct MeasurementWithTime {
    co2: u16,
    temperature: f32,
    humidity: f32,
    time: DateTime<Utc>,
    device: String,
}

pub async fn save_measurement_to_influx(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    device: &str,
    co2: u16,
    temperature: f32,
    humidity: f32,
    reqwest_client: &reqwest::Client,
) {
    let line_protocol = format!(
        "scd40_data,device={} co2_ppm={},temperature_c={},humidity_percent={}",
        device, co2, temperature, humidity
    );

    let response = reqwest_client
        .post(&format!(
            "{}/api/v3/write_lp?db={}",
            influx_host, influx_database
        ))
        .body(line_protocol)
        .bearer_auth(influx_token)
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

pub async fn receive_live_data(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
) {
    let mut measurement_queue: CircularQueue<MeasurementWithTime> =
        CircularQueue::with_capacity(300);

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
                                        let now = chrono::Utc::now();
                                        info!("Received measurement success");
                                        info!("CO2: {}", co2);
                                        info!("Temperature: {}", temperature);
                                        info!("Humidity: {}", humidity);
                                        measurement_queue.push(MeasurementWithTime {
                                            co2,
                                            temperature,
                                            humidity,
                                            time: now,
                                            device: device.clone(),
                                        });
                                        save_measurement_to_influx(
                                            &influx_host,
                                            &influx_token,
                                            &influx_database,
                                            device,
                                            co2,
                                            temperature,
                                            humidity,
                                            &reqwest_client,
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
            _ => {} // Ignore other events
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_default_env().init();

    let args = Args::parse();

    let influx_host = env::var("INFLUXDB_URL").expect("INFLUXDB_URL must be set");
    let influx_token = env::var("INFLUXDB_TOKEN").expect("INFLUXDB_TOKEN must be set");
    let influx_database = env::var("INFLUXDB_DATABASE").expect("INFLUXDB_DATABASE must be set");

    let reqwest_client = reqwest::Client::new();

    if args.mark_historical_data {
        log::info!("Marking historical data");
        match mark_historical_data(
            &influx_host,
            &influx_token,
            &influx_database,
            &reqwest_client,
        )
        .await
        {
            Ok(()) => log::info!("Historical data marked successfully"),
            Err(e) => log::error!("Failed to mark historical data: {}", e),
        }
    }

    if args.delete_old_markings {
        log::info!("Deleting old anomaly markings");
        match delete_old_markings(
            &influx_host,
            &influx_token,
            &influx_database,
            &reqwest_client,
        )
        .await
        {
            Ok(()) => log::info!("Old anomaly markings deleted successfully"),
            Err(e) => log::error!("Failed to delete old markings: {}", e),
        }
    }

    if args.receive_live_data {
        log::info!("Receiving live data");
        receive_live_data(
            &influx_host,
            &influx_token,
            &influx_database,
            &reqwest_client,
        )
        .await;
    }
}
