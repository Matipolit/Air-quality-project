# Sunlight Spike Detection Algorithm

## Problem Statement

The sensor is occasionally exposed to direct sunlight, which causes anomalous data:
- **Temperature**: Unnaturally high spikes (3-10°C above normal)
- **Humidity**: Significant drops (5-20% below normal)
- **Duration**: Multi-hour events (2-6 hours typically)
- **Challenge**: Distinguishing from normal daily temperature variations
- **Data flow**: Irregular (typically ~5 minutes, but gaps possible due to battery replacement)

## Characteristics of Sunlight Spikes

Based on Grafana analysis:

1. **Simultaneous multi-metric impact**
   - Temperature rises sharply
   - Humidity drops sharply
   - Strong negative correlation between the two

2. **Temporal patterns**
   - Only occurs during daylight hours (7 AM - 8 PM)
   - Duration: 2-6 hours typically
   - Sharp onset, variable ending (gradual or sharp)

3. **Magnitude**
   - Temperature: 3-10°C above baseline
   - Humidity: 5-20% below baseline
   - Much higher than normal indoor fluctuations

## Algorithm Approaches

### Approach 1: Multi-Feature Correlation Detector ⭐ (Recommended)

**Principle**: Sunlight creates strong negative correlation between temp and humidity.

**Algorithm**:
```
1. Use sliding time-based window (30-60 minutes) - not fixed count!
2. Filter out measurements with large time gaps (>15 min from previous)
3. Calculate temperature slope (°C/hour) using timestamp differences
4. Calculate humidity slope (%RH/hour) using timestamp differences
5. Calculate Pearson correlation coefficient
6. If slopes have opposite signs AND correlation < -0.6 → flag as suspicious
7. Check magnitude to confirm
```

**Pros**:
- Robust against gradual weather changes
- Uses multiple signals
- Low false positive rate

**Cons**:
- Requires enough data points in window (minimum 6-8 measurements)
- May miss very slow-onset events
- Sensitive to data gaps - need gap handling

---

### Approach 2: Deviation from Expected Daily Pattern

**Principle**: Build baseline from historical data, detect anomalous deviations.

**Algorithm**:
```
1. Collect baseline data from cloudy days (manual or auto-labeled)
2. Calculate hourly expected values for temp/humidity
3. For each measurement:
   - z_temp = (actual_temp - expected_temp) / std_dev_temp
   - z_humidity = (actual_humidity - expected_humidity) / std_dev_humidity
4. If z_temp > 2.0 AND z_humidity < -2.0 → potential sunlight
5. Check duration (must persist >1.5 hours)
```

**Pros**:
- Adapts to seasonal changes
- Statistical rigor
- Accounts for day-of-week patterns

**Cons**:
- Requires historical data
- Needs periodic baseline updates
- Complex implementation

---

### Approach 3: Rate of Change + Magnitude Detector

**Principle**: Sunlight causes rapid changes that are sustained over hours.

**Algorithm**:
```
Phase 1: Detect rapid onset
- Find measurement ~30min ago (use actual timestamp, not index!)
- Check if time gap < 45 min (otherwise skip - data too sparse)
- actual_time_diff = current_time - old_time (in hours)
- rate_temp = (current_temp - old_temp) / actual_time_diff
- rate_humidity = (current_humidity - old_humidity) / actual_time_diff
- IF rate_temp > 2°C/hour AND rate_humidity < -3%/hour → flag onset

Phase 2: Check magnitude
- baseline_temp = 24-hour moving average
- deviation_temp = current_temp - baseline_temp
- baseline_humidity = 24-hour moving average
- deviation_humidity = current_humidity - baseline_humidity
- IF deviation_temp > 3°C AND deviation_humidity < -5% → flag elevated

Phase 3: Check duration
- Count consecutive flagged measurements
- IF duration > 1.5 hours → confirm sunlight spike

Phase 4: Verify anti-correlation
- correlation = pearson(temp_window, humidity_window)
- IF correlation < -0.6 → additional confirmation
```

**Pros**:
- Simple to implement
- No historical data needed
- Fast detection

**Cons**:
- May have false positives during HVAC events
- Threshold tuning required
- Requires careful handling of time gaps to avoid incorrect rate calculations

---

### Approach 4: Shape-Based Template Matching

**Principle**: Match current data against known sunlight spike patterns.

**Algorithm**:
```
1. Create reference templates from known sunny days:
   - Short spike (2-3 hours)
   - Medium spike (3-5 hours)
   - Long spike (5-8 hours)

2. Use Dynamic Time Warping (DTW) to compare:
   - Current 4-hour window vs templates
   - Calculate DTW distance

3. If distance < threshold → sunlight detected

4. Alternative: Simple shape features
   - Peak temperature value
   - Time to peak
   - Peak-to-baseline ratio
   - Symmetry of rise/fall
```

**Pros**:
- Captures complex patterns
- Learns from real data
- Can handle variable durations

**Cons**:
- Requires labeled examples
- Computationally intensive (DTW)
- May overfit to specific patterns

---

### Approach 5: Machine Learning (Future Enhancement)

**Principle**: Train classifier on labeled data.

**Features per time window (2-hour)**:
- Temperature: mean, max, min, std_dev, slope, curvature
- Humidity: mean, max, min, std_dev, slope, curvature
- Cross-correlation between temp and humidity
- Time of day (hour)
- Day of week
- Rate of change at window boundaries
- Deviation from 24h moving average

**Model Options**:
- Random Forest (good for tabular data)
- Gradient Boosting (XGBoost, LightGBM)
- Simple Neural Network

**Training Process**:
1. Manually label 30-50 days as sunny/cloudy
2. Extract features for each 30-minute window
3. Train binary classifier
4. Validate on hold-out set
5. Deploy in processor

**Pros**:
- Can discover hidden patterns
- Adapts to specific sensor characteristics
- High accuracy potential

**Cons**:
- Requires labeled training data
- Model complexity
- Needs retraining periodically

---

## Recommended Hybrid Implementation

Combine Approaches 1 and 3 with time-of-day filtering.

### Pseudocode

```rust
fn detect_sunlight_spike(
    window_data: &[Measurement],
    window_duration_hours: f32,
    current_hour: u8,
) -> SunlightDetectionResult {
    
    // Time of day filter
    if current_hour < 7 || current_hour > 20 {
        return SunlightDetectionResult::no_sunlight("Outside daylight hours");
    }
    
    // Calculate temperature metrics
    let temp_values: Vec<f32> = window_data.iter().map(|m| m.temperature).collect();
    let temp_mean = mean(&temp_values);
    let temp_baseline = moving_average_24h(&temperature_history);
    let temp_deviation = temp_mean - temp_baseline;
    let temp_slope = linear_regression_slope(&temp_values, window_duration_hours);
    
    // Calculate humidity metrics
    let humidity_values: Vec<f32> = window_data.iter().map(|m| m.humidity).collect();
    let humidity_mean = mean(&humidity_values);
    let humidity_baseline = moving_average_24h(&humidity_history);
    let humidity_deviation = humidity_mean - humidity_baseline;
    let humidity_slope = linear_regression_slope(&humidity_values, window_duration_hours);
    
    // Calculate correlation
    let correlation = pearson_correlation(&temp_values, &humidity_values);
    
    // Detection logic - all conditions must be true
    let conditions = ConditionSet {
        temp_elevated: temp_deviation > 3.0,
        humidity_depressed: humidity_deviation < -5.0,
        temp_rising_or_high: temp_slope > 0.3,
        humidity_falling_or_low: humidity_slope < -0.3,
        strong_negative_correlation: correlation < -0.6,
    };
    
    let is_spike = conditions.all_true();
    
    // Duration check
    if is_spike && window_duration_hours >= 1.5 {
        return SunlightDetectionResult::sunlight_detected(
            confidence: calculate_confidence(&conditions),
            temp_deviation,
            humidity_deviation,
            correlation,
        );
    }
    
    SunlightDetectionResult::no_sunlight("Conditions not met")
}

// Helper: Get measurements within time window, handling gaps
fn get_time_window_data(
    history: &CircularQueue<MeasurementWithTime>,
    window_duration: Duration,
    max_gap_duration: Duration,  // e.g., 15 minutes
) -> Option<Vec<MeasurementWithTime>> {
    
    let now = SystemTime::now();
    let window_start = now - window_duration;
    
    // Collect measurements within window
    let window_data: Vec<_> = history
        .iter()
        .filter(|m| m.time >= window_start)
        .collect();
    
    // Check for large gaps that would invalidate the window
    for i in 1..window_data.len() {
        let gap = window_data[i].time.duration_since(window_data[i-1].time);
        if gap > max_gap_duration {
            return None;  // Gap too large, window invalid
        }
    }
    
    // Need minimum number of measurements
    if window_data.len() < 8 {
        return None;
    }
    
    Some(window_data)
}
```

### Key Tunable Parameters

| Parameter | Initial Value | Range | Notes |
|-----------|---------------|-------|-------|
| `window_size_hours` | 2.0 | 1.0-3.0 | Smaller = faster detection, larger = more robust |
| `max_gap_minutes` | 15 | 10-20 | Max time gap between measurements in window |
| `min_measurements_in_window` | 8 | 6-12 | Minimum data points needed (accounts for ~5min intervals) |
| `temp_deviation_threshold` | 3.0°C | 2.0-5.0°C | Depends on typical indoor variance |
| `humidity_deviation_threshold` | -5.0% | -3.0 to -10.0% | Adjust based on sensor sensitivity |
| `correlation_threshold` | -0.6 | -0.5 to -0.8 | Stricter = fewer false positives |
| `min_spike_duration` | 1.5 hours | 1.0-3.0 hours | Filters out brief anomalies |
| `daylight_start_hour` | 7 | 6-8 | Adjust for season/location |
| `daylight_end_hour` | 20 | 18-21 | Adjust for season/location |

## Implementation Strategy

### Phase 1: Basic Detection (Week 1)
1. Implement time-based windowing with gap detection
2. Implement Approach 3 (Rate of Change + Magnitude)
3. Add time-of-day filtering
4. Flag measurements with `possible_sunlight: bool`
5. Log detections AND data gaps for manual review

### Phase 2: Correlation Enhancement (Week 2)
1. Add correlation calculation
2. Implement sliding window
3. Refine thresholds based on Phase 1 results
4. Add confidence scoring

### Phase 3: Validation (Week 3)
1. Collect ground truth labels (manually mark sunny days)
2. Calculate precision/recall metrics
3. Visualize detections on Grafana
4. Tune parameters to minimize false positives/negatives

### Phase 4: Advanced Features (Future)
1. Implement baseline learning from historical data
2. Add shape-based detection
3. Consider ML approach if needed
4. Auto-adjust thresholds seasonally

## Data Structure Recommendations

### Add to measurement queue
```rust
pub struct MeasurementWithTime {
    co2: u16,
    temperature: f32,  // Consider using f32 instead of u32 for precision
    humidity: f32,
    time: SystemTime,
    time_since_last: Option<Duration>,  // Track gaps
    sunlight_score: Option<f32>,  // 0.0-1.0 confidence
}
```

### Detection result
```rust
pub struct SunlightDetectionResult {
    detected: bool,
    confidence: f32,  // 0.0-1.0
    temp_deviation: f32,
    humidity_deviation: f32,
    correlation: f32,
    reason: String,
    measurements_in_window: usize,  // For debugging
    largest_gap_seconds: u64,  // For debugging
}
```

## Validation Metrics

Track these metrics to evaluate algorithm performance:

- **Precision**: Of flagged spikes, how many are real sunlight? (target: >90%)
- **Recall**: Of actual sunny periods, how many are detected? (target: >85%)
- **False Positive Rate**: Normal conditions flagged as sunlight (target: <5%)
- **Detection Latency**: Time from spike start to detection (target: <30 min)

## Testing Approach

1. **Unit tests**: Test individual components (correlation, slope, etc.)
2. **Gap handling tests**: Test with synthetic data containing gaps (10min, 30min, 2hr)
3. **Integration tests**: Test full pipeline with synthetic data
4. **Historical replay**: Run algorithm on past data with known labels
5. **Live monitoring**: Deploy with logging, manually verify for 1-2 weeks

## Handling Data Gaps

### Gap Detection Strategy

**Small gaps (<15 minutes)**: 
- Include in window analysis
- Use actual timestamps for rate calculations
- Should not affect detection significantly

**Medium gaps (15-60 minutes)**:
- Invalidate current detection window
- Reset spike tracking
- Wait for new continuous data stream

**Large gaps (>60 minutes)**:
- Clear all baseline calculations
- Treat as system restart
- Require new warmup period

### Implementation Notes

1. **Always use timestamps, never array indices** for rate calculations
2. **Calculate actual time differences** between measurements
3. **Track largest gap in each window** for debugging
4. **Log gap events** to understand battery replacement patterns
5. **Consider exponential moving averages** instead of simple moving averages (more robust to gaps)

## Future Enhancements

- **Adaptive thresholds**: Adjust based on recent weather patterns
- **Seasonal models**: Different parameters for summer vs winter
- **Multi-sensor fusion**: If you add multiple sensors, combine signals
- **Spike prediction**: Use weather API to predict sunny periods
- **Auto-correction**: Automatically filter or adjust spike data before storage
- **Gap interpolation**: Intelligently fill small gaps using interpolation
- **Battery monitoring**: Predict when battery replacement is needed to minimize gaps

## References

- Pearson correlation coefficient: https://en.wikipedia.org/wiki/Pearson_correlation_coefficient
- Dynamic Time Warping: https://en.wikipedia.org/wiki/Dynamic_time_warping
- Z-score anomaly detection: Standard statistical method
- Moving averages: Simple/exponential for baseline calculation

---

**Last Updated**: 2024
**Status**: Planning phase
**Next Steps**: Implement Phase 1 (Basic Detection)
