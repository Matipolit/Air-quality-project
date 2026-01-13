# Air Quality Predictor Web UI

A web-based user interface for the air quality prediction system. This allows you to visually select historical data points and predict air quality conditions 1 hour into the future using machine learning.

## Overview

The predictor web UI provides an intuitive interface to:
- Browse available measurement timestamps from the last 4 hours
- Select a specific point in time with at least 3 hours of historical data
- Predict CO2, temperature, and humidity values 1 hour into the future
- Compare predictions with actual values (when available)
- Visualize prediction accuracy

## Starting the Web Server

### Prerequisites

1. Ensure you have the following environment variables set (or in a `.env` file):
   ```bash
   INFLUX_HOST=http://your-influxdb-host:8086
   INFLUX_TOKEN=your_influx_token
   INFLUX_DATABASE=your_database_name
   ```

2. Make sure you have sufficient historical data in your InfluxDB database (at least 3 hours of continuous measurements).

### Running the Server

Start the web server with:

```bash
cargo run --bin rpi-processor -- --web-server
```

Or specify a custom port:

```bash
cargo run --bin rpi-processor -- --web-server --web-port 3000
```

The default port is **8080**.

### Command Line Options

- `--web-server` / `-w`: Start the web server
- `--web-port <PORT>`: Specify the port (default: 8080)

## Using the Web Interface

1. **Open your browser** and navigate to:
   ```
   http://localhost:8080
   ```

2. **Wait for data to load**: The dropdown will populate with available timestamps from the last 4 hours.

3. **Select a timestamp**: Choose a point in time from the dropdown. Each entry shows:
   - Date and time
   - Current CO2 level (ppm)
   - Temperature (°C)
   - Humidity (%)

4. **Click "Predict Air Quality +1 Hour"**: The system will:
   - Train a gradient boosting model on historical data
   - Use measurements from 3 hours, 1 hour, and 15 minutes before the selected time
   - Predict values 1 hour into the future

5. **View Results**: The interface displays:
   - **Current values** at the selected timestamp
   - **Predicted values** for 1 hour later
   - **Actual values** (if available) with prediction errors
   - Color-coded accuracy indicators

## Features

### Machine Learning Model

The predictor uses a **chained Gradient Boosting Regressor** with the following features:

**Input Features:**
- Time of day (hour, minute, weekday)
- Current measurements (CO2, temperature, humidity)
- Delta values from 15 minutes ago
- Delta values from 1 hour ago
- Delta values from 3 hours ago

**Prediction Chain:**
1. Predict CO2 (+1 hour)
2. Predict Temperature using predicted CO2
3. Predict Humidity using predicted CO2 and temperature

**Model Parameters:**
- 150 estimators
- Learning rate: 0.1
- Max depth: 3

### Anomaly Filtering

The system automatically filters out anomalous measurements detected by the anomaly detection system before training the model.

### Validation

When actual data is available for the prediction time, the UI shows:
- Actual measured values
- Prediction error (difference)
- Visual indicators (green for accurate, red for less accurate)

## API Endpoints

The web server exposes the following REST API endpoints:

### GET `/`
Returns the HTML web interface.

### GET `/api/available-timestamps`
Returns a list of available timestamps with measurements from the last 4 hours.

**Response:**
```json
[
  {
    "time": "2025-01-15T14:30:00Z",
    "co2": 856.5,
    "temperature": 22.3,
    "humidity": 45.2,
    "device": "bedroom"
  },
  ...
]
```

### POST `/api/predict`
Performs a prediction for a given timestamp.

**Request:**
```json
{
  "timestamp": "2025-01-15T14:30:00Z"
}
```

**Response:**
```json
{
  "success": true,
  "input_time": "2025-01-15T14:30:00Z",
  "prediction_time": "2025-01-15T15:30:00Z",
  "input": {
    "co2": 856.5,
    "temperature": 22.3,
    "humidity": 45.2
  },
  "predicted": {
    "co2": 892.3,
    "temperature": 22.8,
    "humidity": 43.1
  },
  "actual": {
    "co2": 888.0,
    "temperature": 22.7,
    "humidity": 43.5,
    "co2_diff": 4.3,
    "temperature_diff": 0.1,
    "humidity_diff": -0.4
  },
  "error": null
}
```

## Architecture

### Backend (Rust)

- **Framework**: Axum web framework
- **ML Library**: SmartCore (XGBoost implementation)
- **Database**: InfluxDB v3 (SQL API)
- **Features**:
  - Async/await with Tokio runtime
  - Type-safe API with Serde JSON serialization
  - CORS support for development

### Frontend (HTML/JavaScript)

- **Pure JavaScript** (no framework dependencies)
- **Responsive design** with CSS Grid
- **Real-time updates** via fetch API
- **Modern UI** with gradient backgrounds and animations

## Troubleshooting

### "No data available" in dropdown

**Problem**: The dropdown shows no available timestamps.

**Solutions**:
1. Check if your InfluxDB is running and accessible
2. Verify environment variables are set correctly
3. Ensure there is data in the `scd40_data` table
4. Check that data is recent (within the last 4 hours)

### "Not enough data after filtering for training"

**Problem**: Too many measurements were filtered as anomalies.

**Solutions**:
1. Review your anomaly detection settings
2. Check if you have at least 100 clean measurements
3. Consider adjusting anomaly detection parameters

### "Could not find full historical context"

**Problem**: Missing measurements at required time intervals (15m, 1h, 3h ago).

**Solutions**:
1. Check for gaps in your data collection
2. Ensure your sensors are sending data regularly
3. Select a different timestamp with more complete history

### Slow predictions

**Problem**: Model training takes a long time.

**Notes**:
- Training on ~10,000 measurements typically takes 5-15 seconds
- This is normal for gradient boosting models
- The model is trained fresh for each prediction to ensure accuracy

## Development

### Adding New Features

The web UI code is located in:
- **Backend**: `src/predictor_web.rs`
- **Frontend**: `src/predictor_web.html`

### Testing Locally

```bash
# Run in development mode with logging
RUST_LOG=debug cargo run --bin rpi-processor -- --web-server

# Test with a specific timestamp via CLI (for comparison)
cargo run --bin rpi-processor -- --predict-weather --prediction-timestamp "2025-01-15T14:30:00Z"
```

### CORS Configuration

The server uses permissive CORS settings for development. For production, consider restricting CORS to your specific domains in `predictor_web.rs`.

## License

Part of the Air Quality Project. See main project LICENSE for details.