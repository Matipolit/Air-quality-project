use circular_queue::CircularQueue;

use crate::MeasurementWithTime;

pub struct AnomalyFlags {
    pub is_startup: bool,
    pub rate_of_change_spike: bool,
    pub physical_constraint_violation: bool,
    pub possible_sunlight: bool,
    pub reason: String,
}
