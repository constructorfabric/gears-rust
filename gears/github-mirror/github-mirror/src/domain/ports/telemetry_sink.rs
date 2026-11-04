use std::path::Path;

use async_trait::async_trait;

use crate::domain::error::DomainError;
use crate::domain::sync::TelemetryLine;

#[async_trait]
pub trait TelemetrySink: Send + Sync + std::fmt::Debug {
    /// # Errors
    /// `Internal` when the line cannot be queued for writing.
    fn record(&self, file: &Path, line: &TelemetryLine<'_>) -> Result<(), DomainError>;

    async fn flush(&self) {}
}
