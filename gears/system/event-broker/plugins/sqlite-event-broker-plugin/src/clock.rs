//! Instants as the tables store them: INTEGER microseconds since the Unix
//! epoch, UTC.

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::BackendError;

/// `instant` in whole microseconds, rounded down.
pub fn micros(instant: DateTime<Utc>) -> i64 {
    instant.timestamp_micros()
}

/// `instant` in whole microseconds, rounded up: the first stored instant at
/// or after it.
pub fn micros_ceil(instant: DateTime<Utc>) -> i64 {
    let floor = instant.timestamp_micros();
    if instant.timestamp_subsec_nanos().is_multiple_of(1000) {
        floor
    } else {
        floor.saturating_add(1)
    }
}

/// A stored instant.
///
/// # Errors
/// `Internal` for a value outside what `chrono` represents, which this plugin
/// never writes.
pub fn instant(micros: i64) -> Result<DateTime<Utc>, BackendError> {
    DateTime::from_timestamp_micros(micros)
        .ok_or_else(|| BackendError::Internal("a stored instant is out of range".to_owned()))
}
