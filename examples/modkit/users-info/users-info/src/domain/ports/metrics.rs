/// Output port for recording domain-level metrics.
///
/// Implementations live in `infra/` (e.g. OpenTelemetry counters).
/// Domain services and API handlers depend only on this trait.
pub trait UsersMetricsPort: Send + Sync {
    fn record_get_user(&self, result: &str);
}
