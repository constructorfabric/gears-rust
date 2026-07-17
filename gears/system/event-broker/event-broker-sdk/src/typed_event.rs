use std::borrow::Cow;

use chrono::{DateTime, Utc};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

/// Implement this on your own event struct to publish or consume it through the SDK.
///
/// ```ignore
/// #[derive(Debug, Clone, Serialize, Deserialize)]
/// pub struct OrderCreated {
///     pub order_id: Uuid,
///     pub customer_id: Uuid,
///     pub total_cents: i64,
/// }
///
/// impl TypedEvent for OrderCreated {
///     const TYPE_ID: &'static str = "gts.cf.core.events.event.v1~example.orders.created.v1";
///     const TOPIC:   &'static str = "gts.cf.core.events.topic.v1~example.orders.v1";
///     const SUBJECT_TYPE: &'static str = "gts.cf.core.events.subject.v1~example.order.v1";
///     const SOURCE:  &'static str = "order-service";
///
///     fn subject(&self) -> Cow<'_, str> {
///         Cow::Owned(self.order_id.to_string())
///     }
/// }
/// ```
pub trait TypedEvent: Serialize + DeserializeOwned + Send + Sync + 'static {
    const TYPE_ID: &'static str;
    const TOPIC: &'static str;
    const SUBJECT_TYPE: &'static str;
    const SOURCE: &'static str;

    fn subject(&self) -> Cow<'_, str>;

    /// Returns the stable grouping identifier used for partition selection.
    ///
    /// Prefer an authenticated, normalized identifier whose representation is
    /// controlled by the producer. Do not pass raw attacker-controlled
    /// free-form values: MurmurHash3 is non-cryptographic, so adversarial keys
    /// can deliberately hot-spot a partition.
    fn partition_key(&self) -> Option<Cow<'_, str>> {
        None
    }

    /// Overrides the authenticated security-context tenant for this event.
    ///
    /// Explicit tenant IDs must pass broker authorization. Returning `None`
    /// uses the authenticated tenant as the default partition input.
    fn tenant_id(&self) -> Option<Uuid> {
        None
    }

    fn trace_parent(&self) -> Option<Cow<'_, str>> {
        None
    }
}

/// Typed event envelope handed to v2 consumers. `Deref<Target = E>` lets callers
/// access payload fields directly while broker-stamped metadata remains accessible.
#[derive(Debug, Clone)]
pub struct EnvelopedEvent<E: TypedEvent> {
    pub payload: E,
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub subject: String,
    pub partition: u32,
    pub sequence: i64,
    pub offset: i64,
    pub occurred_at: DateTime<Utc>,
    pub sequence_time: DateTime<Utc>,
    pub trace_parent: Option<String>,
}

impl<E: TypedEvent> std::ops::Deref for EnvelopedEvent<E> {
    type Target = E;

    fn deref(&self) -> &E {
        &self.payload
    }
}
