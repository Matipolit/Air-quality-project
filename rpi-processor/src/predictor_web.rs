use crate::types::InfluxMeasurementRow;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct AppState {
    pub influx_host: String,
    pub influx_token: String,
    pub influx_database: String,
    pub reqwest_client: reqwest::Client,
}

#[derive(Serialize, Deserialize)]
pub struct AvailableTimestamp {
    pub time: String,
    pub co2: f64,
    pub temperature: f64,
    pub humidity: f64,
    pub device: String,
}

#[derive(Deserialize)]
pub struct PredictionRequest {
    pub timestamp: String,
}

#[derive(Serialize)]
pub struct PredictionResponse {
    pub success: bool,
    pub input_time: String,
    pub prediction_time: String,
    pub input: InputConditions,
    pub predicted: PredictedValues,
    pub actual: Option<ActualValues>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct InputConditions {
    pub co2: f64,
    pub temperature: f64,
    pub humidity: f64,
}

#[derive(Serialize)]
pub struct PredictedValues {
    pub co2: f64,
    pub temperature: f64,
    pub humidity: f64,
}

#[derive(Serialize)]
pub struct ActualValues {
    pub co2: f64,
    pub temperature: f64,
    pub humidity: f64,
    pub co2_diff: f64,
    pub temperature_diff: f64,
    pub humidity_diff: f64,
}

pub async fn run_web_server(
    influx_host: String,
    influx_token: String,
    influx_database: String,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = Arc::new(AppState {
        influx_host,
        influx_token,
        influx_database,
        reqwest_client: reqwest::Client::new(),
    });

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/available-timestamps", get(get_available_timestamps))
        .route("/api/predict", post(perform_prediction))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    log::info!("Starting predictor web server on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn serve_index() -> impl IntoResponse {
    Html(include_str!("predictor_web.html"))
}

async fn get_available_timestamps(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<AvailableTimestamp>>, AppError> {
    let query_url = format!(
        "{}/api/v3/query_sql?db={}",
        state.influx_host, state.influx_database
    );

    // Get measurements from the last 4 hours (so we can check for 3h history)
    let sql_query = r#"
        SELECT
            time,
            co2_ppm,
            temperature_c,
            humidity_percent,
            device
        FROM scd40_data
        WHERE time >= now() - INTERVAL '4 hours'
        ORDER BY time DESC
        LIMIT 500
    "#;

    let response = state
        .reqwest_client
        .post(&query_url)
        .bearer_auth(&state.influx_token)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&serde_json::json!({
            "db": state.influx_database,
            "q": sql_query
        }))?)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(AppError::influx_error(format!(
            "Query failed: {}",
            response.status()
        )));
    }

    let response_text = response.text().await?;
    if response_text.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let influx_rows: Vec<InfluxMeasurementRow> = serde_json::from_str(&response_text)?;

    let timestamps: Vec<AvailableTimestamp> = influx_rows
        .into_iter()
        .map(|row| AvailableTimestamp {
            time: row.time,
            co2: row.co2_ppm,
            temperature: row.temperature_c,
            humidity: row.humidity_percent,
            device: row.device,
        })
        .collect();

    Ok(Json(timestamps))
}

async fn perform_prediction(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PredictionRequest>,
) -> Result<Json<PredictionResponse>, AppError> {
    log::info!("Performing prediction for timestamp: {}", request.timestamp);

    // Parse the timestamp
    let prediction_timestamp = if let Ok(dt) = DateTime::parse_from_rfc3339(&request.timestamp) {
        dt.with_timezone(&Utc)
    } else {
        let time_with_timezone = format!("{}Z", request.timestamp);
        DateTime::parse_from_rfc3339(&time_with_timezone)?.with_timezone(&Utc)
    };

    // Capture prediction results by running the predictor
    let result = predict_weather_with_result(
        &state.influx_host,
        &state.influx_token,
        &state.influx_database,
        &state.reqwest_client,
        Some(request.timestamp.clone()),
    )
    .await;

    match result {
        Ok(pred_result) => Ok(Json(pred_result)),
        Err(e) => Ok(Json(PredictionResponse {
            success: false,
            input_time: request.timestamp.clone(),
            prediction_time: (prediction_timestamp + chrono::Duration::hours(1)).to_rfc3339(),
            input: InputConditions {
                co2: 0.0,
                temperature: 0.0,
                humidity: 0.0,
            },
            predicted: PredictedValues {
                co2: 0.0,
                temperature: 0.0,
                humidity: 0.0,
            },
            actual: None,
            error: Some(e.to_string()),
        })),
    }
}

// Modified version of predict_weather that returns results instead of just logging
async fn predict_weather_with_result(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
    prediction_timestamp_str: Option<String>,
) -> Result<PredictionResponse, Box<dyn std::error::Error>> {
    use crate::fetcher::fetch_measurement_at;
    use crate::types::MeasurementWithTime;
    use chrono::{Datelike, Timelike};
    use smartcore::linalg::basic::matrix::DenseMatrix;
    use smartcore::xgboost::{
        XGRegressor as GradientBoostingRegressor,
        XGRegressorParameters as GradientBoostingRegressorParameters,
    };

    let prediction_timestamp = if let Some(ts_str) = &prediction_timestamp_str {
        if let Ok(dt) = DateTime::parse_from_rfc3339(ts_str) {
            Some(dt.with_timezone(&Utc))
        } else {
            let time_with_timezone = format!("{}Z", ts_str);
            Some(DateTime::parse_from_rfc3339(&time_with_timezone)?.with_timezone(&Utc))
        }
    } else {
        None
    };

    // Fetch and prepare training data
    let mut measurements = fetch_training_data_internal(
        influx_host,
        influx_token,
        influx_database,
        reqwest_client,
        prediction_timestamp,
    )
    .await?;

    if measurements.is_empty() {
        return Err("No data found for training".into());
    }

    let anomalies =
        fetch_anomalies_internal(influx_host, influx_token, influx_database, reqwest_client)
            .await?;

    measurements.retain(|m| !anomalies.contains(&m.time));

    if measurements.len() < 100 {
        return Err("Not enough data after filtering for training".into());
    }

    measurements.sort_by_key(|m| m.time);

    let gbm_params = GradientBoostingRegressorParameters::default()
        .with_n_estimators(150)
        .with_learning_rate(0.1)
        .with_max_depth(3);

    // Prepare training data
    let mut x_base_data = Vec::new();
    let mut y_co2 = Vec::new();
    let mut y_temp = Vec::new();
    let mut y_humidity = Vec::new();

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

    for (i, m_current) in measurements.iter().enumerate() {
        let target_time = m_current.time + chrono::Duration::hours(1);
        let mut m_future_opt = None;

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

    if x_base_data.is_empty() {
        return Err("No training samples found".into());
    }

    // Train models
    let x_co2_mat = DenseMatrix::from_2d_vec(&x_base_data)?;
    let model_co2 = GradientBoostingRegressor::fit(&x_co2_mat, &y_co2, gbm_params.clone())?;

    let mut x_temp_data = x_base_data.clone();
    for (i, row) in x_temp_data.iter_mut().enumerate() {
        row.push(y_co2[i]);
    }
    let x_temp_mat = DenseMatrix::from_2d_vec(&x_temp_data)?;
    let model_temp = GradientBoostingRegressor::fit(&x_temp_mat, &y_temp, gbm_params.clone())?;

    let mut x_hum_data = x_temp_data.clone();
    for (i, row) in x_hum_data.iter_mut().enumerate() {
        row.push(y_temp[i]);
    }
    let x_hum_mat = DenseMatrix::from_2d_vec(&x_hum_data)?;
    let model_humidity =
        GradientBoostingRegressor::fit(&x_hum_mat, &y_humidity, gbm_params.clone())?;

    // Predict
    let latest_measurement = measurements.last().ok_or("No measurements available")?;
    let latest_idx = measurements.len() - 1;

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
        return Err(
            "Could not find full historical context (15m, 1h, 3h) for latest measurement".into(),
        );
    }
    let (p15, p1h, p3h) = (p15.unwrap(), p1h.unwrap(), p3h.unwrap());

    let target_time = latest_measurement.time + chrono::Duration::hours(1);
    let pred_hour = target_time.hour() as f64;
    let pred_minute = target_time.minute() as f64;
    let pred_weekday = target_time.weekday().num_days_from_monday() as f64;

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

    let x_pred_co2 = DenseMatrix::from_2d_vec(&vec![input_vec.clone()])?;
    let pred_co2_val = model_co2.predict(&x_pred_co2)?[0];

    input_vec.push(pred_co2_val);
    let x_pred_temp = DenseMatrix::from_2d_vec(&vec![input_vec.clone()])?;
    let pred_temp_val = model_temp.predict(&x_pred_temp)?[0];

    input_vec.push(pred_temp_val);
    let x_pred_hum = DenseMatrix::from_2d_vec(&vec![input_vec.clone()])?;
    let pred_humidity_val = model_humidity.predict(&x_pred_hum)?[0];

    // Try to fetch actual values if available
    let actual = if prediction_timestamp.is_some() {
        fetch_measurement_at(
            influx_host,
            influx_token,
            influx_database,
            reqwest_client,
            target_time,
        )
        .await?
        .map(|actual| ActualValues {
            co2: actual.co2 as f64,
            temperature: actual.temperature as f64,
            humidity: actual.humidity as f64,
            co2_diff: pred_co2_val - actual.co2 as f64,
            temperature_diff: pred_temp_val - actual.temperature as f64,
            humidity_diff: pred_humidity_val - actual.humidity as f64,
        })
    } else {
        None
    };

    Ok(PredictionResponse {
        success: true,
        input_time: latest_measurement.time.to_rfc3339(),
        prediction_time: target_time.to_rfc3339(),
        input: InputConditions {
            co2: latest_measurement.co2 as f64,
            temperature: latest_measurement.temperature as f64,
            humidity: latest_measurement.humidity as f64,
        },
        predicted: PredictedValues {
            co2: pred_co2_val,
            temperature: pred_temp_val,
            humidity: pred_humidity_val,
        },
        actual,
        error: None,
    })
}

async fn fetch_training_data_internal(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
    end_time: Option<DateTime<Utc>>,
) -> Result<Vec<crate::types::MeasurementWithTime>, Box<dyn std::error::Error>> {
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

async fn fetch_anomalies_internal(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
) -> Result<HashSet<DateTime<Utc>>, Box<dyn std::error::Error>> {
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

// Error handling
struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Error: {}", self.0),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

impl AppError {
    fn influx_error(msg: String) -> Self {
        Self(anyhow::anyhow!(msg))
    }
}
