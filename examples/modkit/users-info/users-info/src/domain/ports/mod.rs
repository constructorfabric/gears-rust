pub mod audit;
pub mod metrics;

pub use audit::AuditPort;
pub use metrics::UsersMetricsPort;

/// Output port: publish domain events (no knowledge of transport).
pub trait EventPublisher<E>: Send + Sync + 'static {
    fn publish(&self, event: &E);
}
