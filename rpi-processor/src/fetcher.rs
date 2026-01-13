use crate::types::{InfluxMeasurementRow, MeasurementWithTime};
use chrono::{DateTime, Utc};
use std::error::Error;

pub async fn fetch_measurement_at(
    influx_host: &str,
    influx_token: &str,
    influx_database: &str,
    reqwest_client: &reqwest::Client,
    target_time: DateTime<Utc>,
) -> Result<Option<MeasurementWithTime>, Box<dyn Error>> {
    let query_url = format!("{}/api/v3/query_sql?db={}", influx_host, influx_database);

    // Look for a measurement within +/- 5 minutes of the target time
    let start_window = target_time - chrono::Duration::minutes(5);
    let end_window = target_time + chrono::Duration::minutes(5);

    let sql_query = format!(
        r#"
        SELECT
            time,
            co2_ppm,
            temperature_c,
            humidity_percent,
            device
        FROM scd40_data
        WHERE time >= '{}' AND time <= '{}'
        ORDER BY time ASC
        LIMIT 1
    "#,
        start_window.to_rfc3339(),
        end_window.to_rfc3339()
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
        return Ok(None);
    }

    let influx_rows: Vec<InfluxMeasurementRow> = serde_json::from_str(&response_text)?;
    if let Some(row) = influx_rows.first() {
        Ok(Some(row.to_measurement_with_time()?))
    } else {
        Ok(None)
    }
}
