//! Log-only [`UsageSink`]: the placeholder behind the Quota Guard until the
//! Usage Reporter (slice 5b) replaces it.

use async_trait::async_trait;

use crate::domain::ports::{UsageReport, UsageSink};

/// Logs the usage of each terminal request and drops it. Carries no content.
#[derive(Debug, Default)]
pub struct LogUsageSink;

#[async_trait]
impl UsageSink for LogUsageSink {
    async fn record(&self, report: UsageReport) {
        tracing::info!(
            response_id = %report.response_id,
            model = %report.model,
            provider_type = %report.provider_type,
            quantity_micro_credits = report.quantity_micro_credits,
            complete = report.usage.complete,
            estimated = report.usage.estimated,
            "usage recorded (log-only sink)"
        );
    }
}
