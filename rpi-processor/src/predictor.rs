use crate::fetcher::fetch_measurement_at;
use crate::types::{InfluxMeasurementRow, MeasurementWithTime};
use chrono::{DateTime, Datelike, Timelike, Utc};
use smartcore::linalg::basic::matrix::DenseMatrix;
use smartcore::xgboost::{
    XGRegressor as GradientBoostingRegressor,
    XGRegressorParameters as GradientBoostingRegressorParameters,
};
use std::collections::HashSet;
use std::error::Error;

pub async fn predict_weather(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
    prediction_timestamp_str: Option<String>,
) -> Result<(), Box<dyn Error>> {
    log::info!("Starting weather prediction...");

    let prediction_timestamp = if let Some(ts_str) = prediction_timestamp_str {
        // Try parsing as provided first (e.g. "2025-11-17T09:15:00+01:00")
        if let Ok(dt) = DateTime::parse_from_rfc3339(&ts_str) {
            Some(dt.with_timezone(&Utc))
        } else {
            // If that fails, try appending 'Z' (assuming UTC if no timezone provided)
            let time_with_timezone = format!("{}Z", ts_str);
            Some(DateTime::parse_from_rfc3339(&time_with_timezone)?.with_timezone(&Utc))
        }
    } else {
        None
    };

    // 1. Fetch historical data
    let mut measurements = fetch_training_data(
        influx_host,
        influx_token,
        influx_database,
        reqwest_client,
        prediction_timestamp,
    )
    .await?;

    if measurements.is_empty() {
        log::warn!("No data found for training.");
        return Ok(());
    }

    // Fetch anomalies to filter
    let anomalies =
        fetch_anomalies(influx_host, influx_token, influx_database, reqwest_client).await?;
    log::info!("Fetched {} anomalies for filtering", anomalies.len());

    // Filter out anomalies
    let initial_len = measurements.len();
    measurements.retain(|m| !anomalies.contains(&m.time));
    log::info!(
        "Filtered {} anomalous measurements. Remaining: {}",
        initial_len - measurements.len(),
        measurements.len()
    );

    if measurements.len() < 100 {
        log::warn!("Not enough data after filtering for training.");
        return Ok(());
    }

    // Sort by time ascending for time series processing
    measurements.sort_by_key(|m| m.time);

    // Parameters for the Gradient Boosting Regressor itself
    let gbm_params = GradientBoostingRegressorParameters::default()
        .with_n_estimators(150)
        .with_learning_rate(0.1)
        .with_max_depth(3);

    // 2. Prepare data
    // Features: [Hour, Minute, Weekday, Current_CO2, Delta_15m_CO2, Delta_1h_CO2, Delta_3h_CO2, Current_Temp, Delta_15m_Temp, Delta_1h_Temp, Delta_3h_Temp, Current_Humidity, Delta_15m_Humidity, Delta_1h_Humidity, Delta_3h_Humidity]
    // Targets: [Future_CO2, Future_Temp, Future_Humidity] (1 hour later)

    let mut x_base_data = Vec::new();
    let mut y_co2 = Vec::new();
    let mut y_temp = Vec::new();
    let mut y_humidity = Vec::new();

    // Helper to find past measurement
    let find_past =
        |target_time: DateTime<Utc>, current_idx: usize| -> Option<&MeasurementWithTime> {
            let start_search = if current_idx > 400 {
                current_idx - 400
            } else {
                0
            };
            for j in (start_search..current_idx).rev() {
                let m = &measurements[j];
                let diff = target_time
                    .signed_duration_since(m.time)
                    .num_minutes()
                    .abs();
                if diff <= 10 {
                    return Some(m);
                }
                if m.time < target_time - chrono::Duration::minutes(20) {
                    return None;
                }
            }
            None
        };

    // Find triplets (t-3h, t-1h, t-15m, t, t+1h)
    for (i, m_current) in measurements.iter().enumerate() {
        // 1. Find Future Target (t + 1h)
        let target_time = m_current.time + chrono::Duration::hours(1);
        let mut m_future_opt = None;

        // Look forward
        for m_next in measurements.iter().skip(i + 1) {
            let diff = m_next.time.signed_duration_since(target_time);
            if diff.num_minutes().abs() <= 5 {
                m_future_opt = Some(m_next);
                break;
            } else if diff.num_minutes() > 5 {
                break;
            }
        }

        if let Some(m_future) = m_future_opt {
            // Find historical context
            let m_15m = find_past(m_current.time - chrono::Duration::minutes(15), i);
            let m_1h = find_past(m_current.time - chrono::Duration::hours(1), i);
            let m_3h = find_past(m_current.time - chrono::Duration::hours(3), i);

            if let (Some(m_15m), Some(m_1h), Some(m_3h)) = (m_15m, m_1h, m_3h) {
                let hour = m_current.time.hour() as f64;
                let minute = m_current.time.minute() as f64;
                let weekday = m_current.time.weekday().num_days_from_monday() as f64;

                x_base_data.push(vec![
                    hour,
                    minute,
                    weekday,
                    m_current.co2 as f64,
                    m_current.co2 as f64 - m_15m.co2 as f64,
                    m_current.co2 as f64 - m_1h.co2 as f64,
                    m_current.co2 as f64 - m_3h.co2 as f64,
                    m_current.temperature as f64,
                    m_current.temperature as f64 - m_15m.temperature as f64,
                    m_current.temperature as f64 - m_1h.temperature as f64,
                    m_current.temperature as f64 - m_3h.temperature as f64,
                    m_current.humidity as f64,
                    m_current.humidity as f64 - m_15m.humidity as f64,
                    m_current.humidity as f64 - m_1h.humidity as f64,
                    m_current.humidity as f64 - m_3h.humidity as f64,
                ]);

                y_co2.push(m_future.co2 as f64);
                y_temp.push(m_future.temperature as f64);
                y_humidity.push(m_future.humidity as f64);
            }
        }
    }

    log::info!(
        "Created {} training samples with full 3h context",
        x_base_data.len()
    );
    if x_base_data.is_empty() {
        log::warn!("No training samples found (maybe gaps in data).");
        return Ok(());
    }

    // 3. Train models (Chained Gradient Boosting)

    // Train CO2 Model
    log::info!("Training CO2 Gradient Boosting model...");
    let x_co2_mat =
        DenseMatrix::from_2d_vec(&x_base_data).map_err(|e| Box::new(e) as Box<dyn Error>)?;
    let model_co2 = GradientBoostingRegressor::fit(&x_co2_mat, &y_co2, gbm_params.clone())?;

    // Train Temperature Model (using actual future CO2 as feature)
    log::info!("Training Temperature Gradient Boosting model (chained)...");
    let mut x_temp_data = x_base_data.clone();
    for (i, row) in x_temp_data.iter_mut().enumerate() {
        row.push(y_co2[i]);
    }
    let x_temp_mat =
        DenseMatrix::from_2d_vec(&x_temp_data).map_err(|e| Box::new(e) as Box<dyn Error>)?;
    let model_temp = GradientBoostingRegressor::fit(&x_temp_mat, &y_temp, gbm_params.clone())?;

    // Train Humidity Model (using actual future CO2 and Temp as features)
    log::info!("Training Humidity Gradient Boosting model (chained)...");
    let mut x_hum_data = x_temp_data.clone();
    for (i, row) in x_hum_data.iter_mut().enumerate() {
        row.push(y_temp[i]);
    }
    let x_hum_mat =
        DenseMatrix::from_2d_vec(&x_hum_data).map_err(|e| Box::new(e) as Box<dyn Error>)?;
    let model_humidity =
        GradientBoostingRegressor::fit(&x_hum_mat, &y_humidity, gbm_params.clone())?;

    // 4. Predict for next hour using LATEST measurement
    // We need the latest measurement AND measurements from 15m, 1h, 3h ago.

    let latest_measurement = measurements.last().ok_or("No measurements available")?;
    let latest_idx = measurements.len() - 1;

    // Find historical context for prediction
    let p15 = find_past(
        latest_measurement.time - chrono::Duration::minutes(15),
        latest_idx,
    );
    let p1h = find_past(
        latest_measurement.time - chrono::Duration::hours(1),
        latest_idx,
    );
    let p3h = find_past(
        latest_measurement.time - chrono::Duration::hours(3),
        latest_idx,
    );

    if p15.is_none() || p1h.is_none() || p3h.is_none() {
        log::warn!(
            "Could not find full historical context (15m, 1h, 3h) for latest measurement. Cannot predict."
        );
        return Ok(());
    }
    let (p15, p1h, p3h) = (p15.unwrap(), p1h.unwrap(), p3h.unwrap());

    // If we are in "live" mode (no prediction_timestamp), check if data is recent
    if prediction_timestamp.is_none() {
        if Utc::now()
            .signed_duration_since(latest_measurement.time)
            .num_minutes()
            > 30
        {
            log::warn!(
                "Latest measurement is too old ({}), skipping prediction.",
                latest_measurement.time
            );
            return Ok(());
        }
    }

    let target_time = latest_measurement.time + chrono::Duration::hours(1);
    let pred_hour = target_time.hour() as f64;
    let pred_minute = target_time.minute() as f64;
    let pred_weekday = target_time.weekday().num_days_from_monday() as f64;

    // Construct base input vector
    let mut input_vec = vec![
        pred_hour,
        pred_minute,
        pred_weekday,
        latest_measurement.co2 as f64,
        latest_measurement.co2 as f64 - p15.co2 as f64,
        latest_measurement.co2 as f64 - p1h.co2 as f64,
        latest_measurement.co2 as f64 - p3h.co2 as f64,
        latest_measurement.temperature as f64,
        latest_measurement.temperature as f64 - p15.temperature as f64,
        latest_measurement.temperature as f64 - p1h.temperature as f64,
        latest_measurement.temperature as f64 - p3h.temperature as f64,
        latest_measurement.humidity as f64,
        latest_measurement.humidity as f64 - p15.humidity as f64,
        latest_measurement.humidity as f64 - p1h.humidity as f64,
        latest_measurement.humidity as f64 - p3h.humidity as f64,
    ];

    // Predict CO2
    let x_pred_co2 = DenseMatrix::from_2d_vec(&vec![input_vec.clone()])
        .map_err(|e| Box::new(e) as Box<dyn Error>)?;
    let pred_co2_val = model_co2.predict(&x_pred_co2)?[0];

    // Predict Temperature (chaining CO2)
    input_vec.push(pred_co2_val);
    let x_pred_temp = DenseMatrix::from_2d_vec(&vec![input_vec.clone()])
        .map_err(|e| Box::new(e) as Box<dyn Error>)?;
    let pred_temp_val = model_temp.predict(&x_pred_temp)?[0];

    // Predict Humidity (chaining CO2 and Temp)
    input_vec.push(pred_temp_val);
    let x_pred_hum = DenseMatrix::from_2d_vec(&vec![input_vec.clone()])
        .map_err(|e| Box::new(e) as Box<dyn Error>)?;
    let pred_humidity_val = model_humidity.predict(&x_pred_hum)?[0];

    log::info!(
        "Input conditions at {}: CO2: {} ppm, Temp: {:.2} °C, Humidity: {:.2} %",
        latest_measurement.time,
        latest_measurement.co2,
        latest_measurement.temperature,
        latest_measurement.humidity
    );
    log::info!("Prediction for +1 hour ({}): ", target_time);
    log::info!("  CO2: {:.2} ppm", pred_co2_val);
    log::info!("  Temperature: {:.2} °C", pred_temp_val);
    log::info!("  Humidity: {:.2} %", pred_humidity_val);

    // Validation: If we have a prediction timestamp, fetch the actual value
    if prediction_timestamp.is_some() {
        log::info!("Validating prediction against actual data...");
        if let Some(actual) = fetch_measurement_at(
            influx_host,
            influx_token,
            influx_database,
            reqwest_client,
            target_time,
        )
        .await?
        {
            log::info!("Actual values at {}: ", actual.time);
            log::info!(
                "  CO2: {} ppm (Diff: {:.2})",
                actual.co2,
                pred_co2_val - actual.co2 as f64
            );
            log::info!(
                "  Temperature: {:.2} °C (Diff: {:.2})",
                actual.temperature,
                pred_temp_val - actual.temperature as f64
            );
            log::info!(
                "  Humidity: {:.2} % (Diff: {:.2})",
                actual.humidity,
                pred_humidity_val - actual.humidity as f64
            );
        } else {
            log::warn!(
                "Could not find actual data for validation at {}",
                target_time
            );
        }
    }

    Ok(())
}

async fn fetch_training_data(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
    end_time: Option<DateTime<Utc>>,
) -> Result<Vec<MeasurementWithTime>, Box<dyn Error>> {
    let query_url = format!("{}/api/v3/query_sql?db={}", influx_host, influx_database);

    let time_filter = if let Some(et) = end_time {
        format!("WHERE time <= '{}'", et.to_rfc3339())
    } else {
        "".to_string()
    };

    let sql_query = format!(
        r#"
        SELECT
            time,
            co2_ppm,
            temperature_c,
            humidity_percent,
            device
        FROM scd40_data
        {}
        ORDER BY time DESC
        LIMIT 10000
    "#,
        time_filter
    );
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
        return Err(format!("InfluxDB query failed: {}", response.status()).into());
    }

    let response_text = response.text().await?;
    if response_text.is_empty() {
        return Ok(Vec::new());
    }

    let influx_rows: Vec<InfluxMeasurementRow> = serde_json::from_str(&response_text)?;

    let mut measurements = Vec::with_capacity(influx_rows.len());
    for row in influx_rows {
        if let Ok(m) = row.to_measurement_with_time() {
            measurements.push(m);
        }
    }

    Ok(measurements)
}

async fn fetch_anomalies(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
) -> Result<HashSet<DateTime<Utc>>, Box<dyn Error>> {
    let query_url = format!("{}/api/v3/query_sql?db={}", influx_host, influx_database);
    let sql_query = "SELECT time FROM anomalies";

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
        // If anomalies table doesn't exist or other error, just return empty set?
        // Better to log and return empty if it's just "table not found" but hard to distinguish.
        // For now, let's assume if it fails, we have no anomalies to filter.
        log::warn!(
            "Failed to fetch anomalies or no anomalies found: {}",
            response.status()
        );
        return Ok(HashSet::new());
    }

    let response_text = response.text().await?;
    if response_text.is_empty() {
        return Ok(HashSet::new());
    }

    #[derive(serde::Deserialize)]
    struct AnomalyRow {
        time: String,
    }

    let rows: Vec<AnomalyRow> = serde_json::from_str(&response_text).unwrap_or_default();
    let mut anomalies = HashSet::new();
    for row in rows {
        let time_with_timezone = if row.time.ends_with('Z') {
            row.time
        } else {
            format!("{}Z", row.time)
        };
        if let Ok(dt) = DateTime::parse_from_rfc3339(&time_with_timezone) {
            anomalies.insert(dt.with_timezone(&Utc));
        }
    }
    Ok(anomalies)
}
