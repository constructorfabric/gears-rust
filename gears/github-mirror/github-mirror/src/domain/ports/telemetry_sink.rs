use std::path::Path;

use crate::domain::error::DomainError;
use crate::domain::sync::TelemetryLine;

pub trait TelemetrySink: Send + Sync + std::fmt::Debug {
    /// # Errors
    /// `Internal` when the line cannot be written.
    fn record(&self, file: &Path, line: &TelemetryLine<'_>) -> Result<(), DomainError>;
}
