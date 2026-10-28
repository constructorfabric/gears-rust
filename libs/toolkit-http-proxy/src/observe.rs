//! Hooks for metrics and logging.

use std::time::Duration;

use crate::target::Target;

/// Facts about the connection a request was sent on.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ConnectionInfo<'a> {
    /// The request's target.
    pub target: &'a Target,
    /// Whether a pooled connection was reused.
    pub reused: bool,
    /// HTTP version used on the connection.
    pub version: http::Version,
    /// TCP connect plus TLS handshake time, for new connections.
    pub connect_time: Option<Duration>,
}

/// Receives engine events. Implementations must be cheap and non-blocking;
/// they run on the request path.
pub trait Observer: Send + Sync {
    /// A request is about to be written to a connection.
    fn on_connection(&self, info: &ConnectionInfo<'_>) {
        let _ = info;
    }
}

/// Observer that ignores every event.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopObserver;

impl Observer for NoopObserver {}
