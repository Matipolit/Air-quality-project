mod anomalies;
mod fetcher;
mod predictor;
mod predictor_web;
mod types;

use chrono::{DateTime, Utc};
use circular_queue::CircularQueue;
use rumqttc::{Client, Event, MqttOptions, Packet};
use shared_types::{DeviceMessage, DevicePayload};
use std::{env, time::Duration};

use log::{self, debug, error, info};

use clap::Parser;
use types::{InfluxMeasurementRow, MeasurementWithTime};

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

    /// Predict weather (CO2, Temp, Humidity) based on historical data
    #[arg(short, long, default_value_t = false)]
    predict_weather: bool,

    /// Timestamp to use as "now" for prediction (RFC3339 format).
    /// If provided, the model will be trained on data before this time,
    /// and predict the weather 1 hour after this time, comparing it with actual data.
    #[arg(long)]
    prediction_timestamp: Option<String>,

    /// Run a matrix of anomaly detection tests with different parameters
    #[arg(long, default_value_t = false)]
    mark_anomalies_test: bool,

    /// Run web server for predictor UI
    #[arg(short = 'w', long, default_value_t = false)]
    web_server: bool,

    /// Port for web server
    #[arg(long, default_value_t = 8080)]
    web_port: u16,

    /// Base path for web server (e.g. "/air-predictor")
    #[arg(long, default_value = "/")]
    web_base_path: String,
}

pub async fn fetch_historical_measurements(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
) -> Result<Vec<MeasurementWithTime>, Box<dyn std::error::Error>> {
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
            "InfluxDB query failed with status {}: {}",
            status, error_text
        )
        .into());
    }

    let response_text = response.text().await?;
    if response_text.is_empty() {
        return Ok(Vec::new());
    }

    let influx_rows: Vec<InfluxMeasurementRow> = serde_json::from_str(&response_text)?;
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
                return Err(format!("Row {} conversion failed: {}", idx, e).into());
            }
        }
    }
    Ok(measurements)
}

pub async fn run_anomaly_test_matrix(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("Starting anomaly test matrix with new multi-stage detector...");
    let measurements =
        fetch_historical_measurements(influx_host, influx_token, influx_database, reqwest_client)
            .await?;
    log::info!("Fetched {} measurements for testing", measurements.len());

    // Test different configuration combinations
    // Simple rule-based thresholds
    let humidity_definite = [50.0, 55.0, 60.0];
    let humidity_suspicious = [60.0, 65.0, 70.0];
    let temp_above_daily_min = [6.0, 8.0, 10.0];
    let temp_absolute_min = [10.0, 12.0, 14.0];

    let total_tests =
        humidity_definite.len() * humidity_suspicious.len() * temp_above_daily_min.len();
    let mut current_test = 0;

    for &hum_def in &humidity_definite {
        for &hum_sus in &humidity_suspicious {
            for &temp_rise in &temp_above_daily_min {
                for &temp_abs in &temp_absolute_min {
                    current_test += 1;

                    let config = anomalies::AnomalyConfig {
                        humidity_definite_anomaly: hum_def,
                        humidity_suspicious: hum_sus,
                        temp_above_daily_min: temp_rise,
                        temp_absolute_min_for_spike: temp_abs,
                        ..Default::default()
                    };

                    let measurement_name = format!(
                        "anomalies_v3_hd{}_hs{}_tr{}_ta{}",
                        hum_def as u32, hum_sus as u32, temp_rise as u32, temp_abs as u32
                    );

                    log::info!(
                        "Running test {}/{}: {}",
                        current_test,
                        total_tests,
                        measurement_name
                    );

                    // Run analysis with this config
                    let result = anomalies::analyze_historical_data(&measurements, Some(config));

                    log::info!(
                        "  -> {} anomalies ({} sunlight)",
                        result.anomalies_detected,
                        result.sunlight_events
                    );

                    // Write results
                    let anomaly_batch: Vec<_> = result
                        .anomaly_timestamps
                        .iter()
                        .map(|(time, flags, device)| (*time, flags.clone(), device.clone()))
                        .collect();

                    for chunk in anomaly_batch.chunks(500) {
                        save_anomalies_batch(
                            influx_host,
                            influx_token,
                            influx_database,
                            reqwest_client,
                            chunk,
                            &measurement_name,
                        )
                        .await?;
                    }
                }
            }
        }
    }

    log::info!("Anomaly test matrix complete!");
    Ok(())
}

pub async fn mark_historical_data(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let measurements =
        fetch_historical_measurements(influx_host, influx_token, influx_database, reqwest_client)
            .await?;

    log::info!("Received {} measurements", measurements.len());

    // Use new multi-stage anomaly detection
    let result = anomalies::analyze_historical_data(&measurements, None);

    log::info!(
        "Analysis complete: {} anomalies detected ({} sunlight events)",
        result.anomalies_detected,
        result.sunlight_events
    );

    // Write anomalies in batches
    let batch_size = 100;
    let anomaly_batch: Vec<_> = result
        .anomaly_timestamps
        .iter()
        .map(|(time, flags, device)| (*time, flags.clone(), device.clone()))
        .collect();

    for chunk in anomaly_batch.chunks(batch_size) {
        save_anomalies_batch(
            influx_host,
            influx_token,
            influx_database,
            reqwest_client,
            chunk,
            "anomalies",
        )
        .await?;
        log::info!("Wrote batch of {} anomalies to InfluxDB", chunk.len());
    }

    log::info!(
        "Anomaly detection complete: {} total anomalies found and saved",
        result.anomalies_detected
    );
    Ok(())
}

async fn save_anomalies_batch(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
    anomalies: &[(DateTime<Utc>, anomalies::AnomalyFlags, String)],
    measurement_name: &str,
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
            "{},device={} temperature_spike={},humidity_spike={},co2_spike={},physical_constraint_temp_violation={},physical_constraint_humidity_violation={},physical_constraint_co2_violation={},possible_sunlight={} {}",
            measurement_name,
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

    // 1. List all tables to find ones starting with "anomalies"
    let query_url = format!("{}/api/v3/query_sql?db={}", influx_host, influx_database);
    let sql_query = "SHOW TABLES";

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
            "InfluxDB show tables failed with status {}: {}",
            status, error_text
        )
        .into());
    }

    let response_text = response.text().await?;
    let tables: Vec<serde_json::Value> = serde_json::from_str(&response_text)?;

    let mut tables_to_delete = Vec::new();
    for table in tables {
        if let Some(name) = table.get("table_name").and_then(|v| v.as_str()) {
            if name.starts_with("anomalies") {
                tables_to_delete.push(name.to_string());
            }
        }
    }

    if tables_to_delete.is_empty() {
        log::info!("No anomaly tables found to delete.");
        return Ok(());
    }

    log::info!(
        "Found {} tables to delete: {:?}",
        tables_to_delete.len(),
        tables_to_delete
    );

    for table_name in tables_to_delete {
        let delete_url = format!(
            "{}/api/v3/configure/table?db={}&table={}",
            influx_host, influx_database, table_name
        );

        log::info!("Deleting table: {}", table_name);
        let response = reqwest_client
            .delete(&delete_url)
            .bearer_auth(influx_token)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await?;
            log::error!(
                "Failed to delete table {}: {} - {}",
                table_name,
                status,
                error_text
            );
        } else {
            log::info!("Successfully deleted table: {}", table_name);
        }
    }

    log::info!("Finished deleting old anomaly markings");
    Ok(())
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
                                error!("Failed to decode message payload: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to decode message payload: {:?}", e);
                    }
                }
            }

            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!("Connected to MQTT broker");
                info!("Subscribing to mqtt topic {}", mqtt_topic);
                client
                    .subscribe(&mqtt_topic, rumqttc::QoS::AtLeastOnce)
                    .expect("Could not subscribe to the MQTT topic.");
            }
            Ok(Event::Incoming(Packet::SubAck(_))) => info!("Subscription confirmed"),
            Err(e) => {
                error!("Connection error: {:?}", e);
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

    if args.mark_anomalies_test {
        log::info!("Running anomaly test matrix");
        match run_anomaly_test_matrix(
            &influx_host,
            &influx_token,
            &influx_database,
            &reqwest_client,
        )
        .await
        {
            Ok(()) => log::info!("Anomaly test matrix completed successfully"),
            Err(e) => log::error!("Failed to run anomaly test matrix: {}", e),
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

    if args.predict_weather {
        log::info!("Predicting weather");
        match predictor::predict_weather(
            &influx_host,
            &influx_token,
            &influx_database,
            &reqwest_client,
            args.prediction_timestamp,
        )
        .await
        {
            Ok(()) => log::info!("Weather prediction complete"),
            Err(e) => log::error!("Failed to predict weather: {}", e),
        }
    }

    if args.web_server {
        log::info!("Starting predictor web server on port {}", args.web_port);
        match predictor_web::run_web_server(
            influx_host.clone(),
            influx_token.clone(),
            influx_database.clone(),
            args.web_port,
            args.web_base_path,
        )
        .await
        {
            Ok(()) => log::info!("Web server stopped"),
            Err(e) => log::error!("Web server failed: {}", e),
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
