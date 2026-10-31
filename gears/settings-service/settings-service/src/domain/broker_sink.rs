//! The publishing end of the broker client, as the domain sees it: the sink a
//! publish goes through and what one publish came to.
//!
//! A leaf module of the domain: both the replica-coherence half and the
//! consumer-activation half publish through it, and neither imports the other
//! for it.

use async_trait::async_trait;
// The broker-native event is the unit a sink publishes, so the broker SDK's
// `Event` is the one type of it this module names.
use event_broker_sdk::Event;
use toolkit_macros::domain_model;

/// Why a publish did not complete.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishError {
    /// The broker refused it, a missing `produce` grant included.
    Refused(String),
    /// The broker did not accept it within `publish_timeout_seconds`.
    TimedOut,
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => write!(f, "refused: {reason}"),
            Self::TimedOut => f.write_str("not accepted within the publish timeout"),
        }
    }
}

/// The publishing end of the broker client: hands the broker one event and
/// waits at most `publish_timeout_seconds` for its acceptance.
#[async_trait]
pub trait BroadcastSink: Send + Sync {
    /// Publish `event` under the publishing identity.
    async fn publish(&self, event: Event) -> Result<(), PublishError>;
}

/// The result of one `change_notification` publish of a request.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationResult {
    /// The broker accepted it.
    Accepted,
    /// It was never attempted: nothing can be published, or an earlier publish
    /// of the request failed.
    Skipped,
    /// It was refused or not accepted in time; the request's remaining
    /// publishes are skipped.
    Failed(PublishError),
}
