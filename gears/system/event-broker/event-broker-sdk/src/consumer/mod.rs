mod ack;
pub mod backend;
mod builder;
mod consumer;
mod dispatcher;
mod offset_manager;

pub use ack::AckHandle;
#[cfg(feature = "outbox")]
pub use ack::TxAckHandle;

#[cfg(feature = "outbox")]
pub use builder::WithTx;
pub use builder::{BrokerOnly, ConsumerBuilder, ConsumerReady, NoDlq, WithDlq};

pub use consumer::Consumer;
pub use offset_manager::{
    BrokerOffsetManager, Fallback, InMemoryOffsetManager, OffsetManager, ResolvedPosition,
};
#[cfg(feature = "outbox")]
pub use offset_manager::{LocalDbOffsetManager, TxOffsetManager};

pub use crate::error::OffsetManagerError;
pub use backend::{
    ConsumerBackend, FrameStream, PartitionSlot, SeekPosition, SubscriptionAssignment,
    SubscriptionInterest, WireEvent, WireFrame,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ConsumerError;
use crate::ids::ConsumerGroupId;

/// Raw event delivered to v1 handlers. `data` is untyped JSON;
/// typed dispatch is deferred to v2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvent {
    pub id: Uuid,
    pub type_id: String,
    pub topic: String,
    pub tenant_id: Uuid,
    pub subject: String,
    pub subject_type: String,
    pub partition: u32,
    pub sequence: i64,
    pub offset: i64,
    pub occurred_at: DateTime<Utc>,
    pub sequence_time: DateTime<Utc>,
    pub trace_parent: Option<String>,
    pub data: serde_json::Value,
}

/// Handler outcome without DLQ — `Reject` is structurally absent.
#[derive(Debug, Clone)]
pub enum HandlerOutcome {
    Success,
    Retry { reason: String },
}

/// Handler outcome when DLQ is configured — adds `Reject`.
#[derive(Debug, Clone)]
pub enum RejectableOutcome {
    Success,
    Retry { reason: String },
    Reject { reason: String },
}

/// Consumer-group reference for the builder.
#[derive(Debug, Clone)]
pub enum ConsumerGroupRef {
    Existing(ConsumerGroupId),
    AutoAnonymous {
        client_agent: String,
        description: Option<String>,
    },
}

impl ConsumerGroupRef {
    pub fn existing(id: ConsumerGroupId) -> Self {
        Self::Existing(id)
    }

    pub fn auto_anonymous(client_agent: impl Into<String>) -> Self {
        Self::AutoAnonymous {
            client_agent: client_agent.into(),
            description: None,
        }
    }
}

/// Payload passed to the `on_dead_letter` callback.
#[derive(Debug, Clone)]
pub struct DeadLetterEvent {
    pub event: RawEvent,
    pub reason: String,
    pub attempts: u16,
}

/// Generic handler trait. The `Ack` and `Outcome` type parameters are bound by the
/// builder's typestate, ensuring compile-time enforcement of DLQ and in-tx ack.
#[async_trait::async_trait]
pub trait EventHandler<Ack, Outcome>: Send + Sync {
    async fn handle(
        &self,
        ctx: &toolkit_security::SecurityContext,
        event: RawEvent,
        attempts: u16,
        ack: Ack,
    ) -> Result<Outcome, ConsumerError>;
}
