//! Domain entities (`DESIGN.md` §3.1 Domain Model): their shapes, their wire
//! tokens, and the checks that read only an entity and the caller
//! (`Subscription::require_joiner`, `ConsumerGroup::require_owner`). No
//! persistence lives here.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gts::{GtsIdPattern, GtsTypeId};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;
use toolkit_security::SecurityContext;
use toolkit_utils::iso8601_duration::Iso8601Duration;

use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::resolution::EffectiveSettings;
use uuid::Uuid;

/// A position in the broker's sequence space.
///
/// Sequences are scoped to one `(topic, partition)` and assigned by the
/// storage backend at persist time, monotonically, starting from 1. Sequence 0
/// is therefore never an event sequence, which is exactly what makes it usable
/// as a cursor: the cursor model is last-processed-offset, the broker emits
/// from `cursor + 1`, and cursor 0 means "nothing processed yet". Consequently
/// no value in this space is ever negative - the valid SEEK range
/// `[RF - 1, HWM]` has a floor of 0 because the retention floor is at least 1.
///
/// See `docs/ADR/0001-offset-semantics.md` and `docs/DESIGN.md`'s "Offset
/// Semantics" section.
///
/// The producer chain (`Meta::previous`/`Meta::sequence`) is a *different*
/// numbering space - producer-assigned per `(producer_id, topic)` for ingest
/// dedup - and deliberately does not use this type.
pub use event_broker_sdk::Sequence;

/// `gts.cf.core.events.topic.v1~` - a named, partitioned event log, as the
/// broker holds it: what the registered instance says, and what this deployment
/// resolved for it.
///
/// `id`, `description` and `retention` are the instance's own data, projected
/// by [`crate::domain::projection::topic`]; `uuid` is the instance's Registry
/// Reference, which no API shape carries. `settings` is what that projection
/// and configuration resolved to together
/// ([`crate::domain::resolution::resolve`]), so a caller needing a partition
/// count, a retention bound or a backend has one place to look and no second
/// source to reconcile it against.
///
/// An event type has no counterpart here: its flat shape is the projection
/// `event_broker_sdk::models::EventType` already owns, and a second copy in the
/// domain would be a shape to keep in step for no gain.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub id: GtsInstanceId,
    /// The topic's Registry Reference (`Instance.uuid`), stored at `bulk_load`;
    /// never derived from `id`. It keys the topic's binding record.
    pub uuid: Uuid,
    /// Required on the instance, so always present on a projected topic.
    pub description: String,
    /// What the topic itself declares, which is advisory: it is one tier of the
    /// retention ladder, and `settings` carries the value that won.
    #[serde(default)]
    pub retention: Option<Iso8601Duration>,
    pub settings: EffectiveSettings,
}

/// `gts.cf.core.events.event.v1~` - an immutable record in a
/// `(topic, partition)` log.
///
/// `Serialize`/`Deserialize` (unlike `event_broker_sdk::models::Event`,
/// which deliberately has none) - this is the exact type
/// `IngestServiceImpl::publish_event` serializes as the ingest outbox
/// payload (design.md D5): reusing this domain type directly means the
/// leased handler that later decodes it can call the existing
/// `domain::backend::to_sdk_event` conversion unchanged, instead of a
/// second, hand-mirrored envelope type duplicating every field.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    /// The event type this event is an instance of: a GTS **type** identifier,
    /// ending in `~`, because a concrete event type is a derived type schema
    /// rather than an instance of one.
    pub r#type: GtsTypeId,
    /// Broker-resolved, never producer-supplied: a publish body names no
    /// topic, and `IngestService::publish_event` stamps the one its `type`
    /// resolves to. Present here so the outbox payload and every read-side
    /// consumer of an `Event` can name the log without re-resolving it.
    pub topic: GtsInstanceId,
    pub subject_tenant: Uuid,
    pub source: String,
    pub subject: String,
    pub subject_type: GtsTypeId,
    /// The principal that owns the event; `None` for a tenant-wide event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_owner: Option<Uuid>,
    pub occurred_at: DateTime<Utc>,
    pub trace_parent: Option<String>,
    pub data: JsonValue,
    /// Publish-input only; stripped on the read projection.
    pub meta: Option<Meta>,
    /// Who published the event, stamped at ingest; `None` for an anonymous
    /// publisher. Stored, never delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest: Option<IngestStamp>,
    /// Read-projection only; broker-derived.
    pub partition: Option<i32>,
    /// Read-projection only.
    pub sequence: Option<Sequence>,
    pub sequence_time: Option<DateTime<Utc>>,
}

/// The publisher of an event as ingest saw it: the caller's tenant and principal.
/// Internal to the broker, for audit and limits.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestStamp {
    pub tenant: Uuid,
    pub actor: Uuid,
}

impl IngestStamp {
    /// The stamp of a publish made as `ctx`; `None` for an anonymous context.
    #[must_use]
    pub fn of_caller(ctx: &SecurityContext) -> Option<Self> {
        (!ctx.is_anonymous()).then(|| Self {
            tenant: ctx.subject_tenant_id(),
            actor: ctx.subject_id(),
        })
    }
}

/// Producer chain metadata. Publish-input only; stripped on read.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub version: i32,
    pub producer_id: Uuid,
    /// `None` when the producer omitted it on the wire. A chained producer
    /// supplies it (the prior sequence its event links to); a monotonic
    /// producer must omit it, and its presence is a mode-shape violation the
    /// ingest path rejects before any sequence check. Kept as an `Option` -
    /// not collapsed to `0` - so that distinction survives into the domain.
    pub previous: Option<i64>,
    pub sequence: i64,
}

/// Engine-typed filter expression on one interest (`ADR-0005`).
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterSpec {
    pub engine: GtsInstanceId,
    pub expression: String,
}

/// One topic-anchored interest within a JOIN request/response
/// (`gts.cf.core.events.subscription.v1~.schema.json`'s `Interest` definition): event-type
/// patterns (GTS wildcard rules) scoped to `topic`, with an optional
/// compiled filter. Lives here (not `domain/delivery.rs`) because it's part
/// of `Subscription`'s own persisted/echoed shape, not just a
/// `DeliveryService::join` operation parameter.
/// Whether tenant-hierarchy traversal stops at `self_managed = true` tenant
/// boundaries. `Respect` is the default and the only mode ordinary callers may
/// ask for; `Ignore` traverses through barriers and is platform-services only.
/// Mirrors `tenant_resolver_sdk::BarrierMode`.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarrierMode {
    #[default]
    Respect,
    Ignore,
}

impl BarrierMode {
    /// The wire token, matching this type's `serde` renaming - kept here so
    /// the serialized form and the hand-built response body cannot drift.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Respect => "respect",
            Self::Ignore => "ignore",
        }
    }

    /// Inverse of [`Self::as_wire`]; `None` for any token the schema's enum
    /// does not list, so an unrecognised mode is rejected rather than
    /// defaulted.
    #[must_use]
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "respect" => Some(Self::Respect),
            "ignore" => Some(Self::Ignore),
            _ => None,
        }
    }
}

impl From<BarrierMode> for authz_resolver_sdk::BarrierMode {
    fn from(mode: BarrierMode) -> Self {
        match mode {
            BarrierMode::Respect => Self::Respect,
            BarrierMode::Ignore => Self::Ignore,
        }
    }
}

/// Which of the events a consume scope admits an interest reads, on top of
/// that scope.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipFilter {
    /// The caller's mailbox: events of the caller's own tenant whose
    /// `subject_owner` is the caller. An unowned event never matches.
    #[default]
    OnlyMine,
    /// Every event of the interest's tenant, owned or not.
    EntireTenant,
    /// Every event of the interest's tenant and of the tenants below it, as
    /// far as the barrier mode and the consume scope reach.
    TenantSubtreeIncluded,
}

impl OwnershipFilter {
    /// The value as the wire spells it.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::OnlyMine => "only_mine",
            Self::EntireTenant => "entire_tenant",
            Self::TenantSubtreeIncluded => "tenant_subtree_included",
        }
    }
}

#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interest {
    pub topic: GtsInstanceId,
    /// The tenant the interest reads, resolved at JOIN: the caller's tenant
    /// when the request names none. Under `TenantSubtreeIncluded` it is the
    /// root of the subtree read.
    pub subject_tenant: Uuid,
    /// Which events of the tenant (or subtree) the interest reads.
    #[serde(default, rename = "ownership_filter")]
    pub ownership: OwnershipFilter,
    /// Whether the interest asks to read across a self-managed tenant
    /// boundary. JOIN sends it as the tenant context's barrier mode, and for
    /// `Ignore` first asks the PDP for `consume_ignore_barrier` on the topic.
    /// No effect under `OnlyMine`, which reads only the caller's own tenant.
    #[serde(default)]
    pub barrier_mode: BarrierMode,
    /// Event-type selectors as canonical GTS patterns. A bare type id already
    /// covers itself and its derived subtree; `…event.v1~*` selects every type.
    /// Typed as [`GtsIdPattern`] - never a bare `String` - and serialized to/from
    /// the cluster cache as strings via [`crate::domain::gts_pattern_serde`].
    #[serde(with = "crate::domain::gts_pattern_serde")]
    pub types: Vec<GtsIdPattern>,
    /// Subject-type selectors as GTS patterns; empty for none. A delivery filter, not a grant.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        with = "crate::domain::gts_pattern_serde"
    )]
    pub subject_types: Vec<GtsIdPattern>,
    pub filter: Option<FilterSpec>,
}

/// The principal that joined a subscription: the subject, its tenant and its
/// subject type, as the JOIN's `SecurityContext` carried them. The token scopes
/// are left out, so the same principal holding another token is still the
/// joiner. Kept in memory with the subscription and never sent on the wire.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joiner {
    pub subject: Uuid,
    pub tenant: Uuid,
    pub subject_type: Option<String>,
}

impl From<&SecurityContext> for Joiner {
    fn from(ctx: &SecurityContext) -> Self {
        Self {
            subject: ctx.subject_id(),
            tenant: ctx.subject_tenant_id(),
            subject_type: ctx.subject_type().map(str::to_owned),
        }
    }
}

/// `gts.cf.core.events.subscription.v1~` - ephemeral consumer instance, held in
/// memory by the `ConsumerGroupCoordinator` of the instance that owns its group.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Only this principal may stream, seek or LEAVE the subscription.
    pub joiner: Joiner,
    pub consumer_group: GtsInstanceId,
    pub client_agent: String,
    pub interests: Vec<Interest>,
    pub topics: Vec<GtsInstanceId>,
    pub assigned: Vec<Assignment>,
    /// Monotonically increasing per group; set by `ConsumerGroupCoordinator`
    /// on every rebalance of the group.
    pub topology_version: i64,
    /// How long the subscription survives a dropped stream before it is
    /// reaped.
    pub session_timeout: Duration,
    /// When this subscription was created (at JOIN). The stable primary sort
    /// key for `GET /v1/subscriptions`, so a new subscription has a
    /// deterministic page position rather than one that depends on its random
    /// id.
    pub created_at: DateTime<Utc>,
}

impl Subscription {
    /// Refuses a caller that is not the subscription's joiner. It reads only the
    /// subscription and the caller, so it asks the PDP nothing.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] with [`ErrorCode::SubscriptionNotOwned`] when
    /// the caller's subject, tenant or subject type differs from the joiner's.
    pub fn require_joiner(&self, ctx: &SecurityContext) -> Result<(), DomainError> {
        if self.joiner != Joiner::from(ctx) {
            return Err(DomainError::Forbidden {
                code: ErrorCode::SubscriptionNotOwned,
                message: format!("subscription '{}' was joined by another principal", self.id),
                resource: self.id.to_string(),
            });
        }
        Ok(())
    }
}

#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Assignment {
    pub topic: GtsInstanceId,
    pub partition: i32,
    pub offset: Sequence,
    pub last_examined: Sequence,
}

/// Ephemeral, in-cache runtime state of a consumer group.
#[domain_model]
#[derive(Debug, Clone)]
pub struct GroupState {
    pub consumer_group: GtsInstanceId,
    pub topic: GtsInstanceId,
    pub per_member_filters: HashMap<Uuid, JsonValue>,
    pub active_members: HashMap<Uuid, Subscription>,
    pub topology_version: i64,
    pub owning_delivery_shard_id: String,
}

/// Ephemeral, in-cache group progress for one `(topic, partition)`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Cursor {
    pub topic: GtsInstanceId,
    pub consumer_group: GtsInstanceId,
    pub partition: i32,
    pub offset: Sequence,
}

/// Whether a `ConsumerGroup` was minted anonymously by `POST
/// /v1/consumer-groups` or pre-registered by name via the types registry
/// (`docs/openapi.yaml`'s `POST /v1/consumer-groups` description).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerGroupKind {
    Anonymous,
    Named,
}

/// `gts.cf.core.events.consumer_group.v1~` - a registered consumer group.
/// Membership (`GroupState::active_members`) is tracked separately, in
/// cache, not here. `tenant_id`/`owner_principal_id` are non-overridable -
/// captured from `SecurityContext` at create time, never from the request
/// body (`docs/schemas/gts.cf.core.events.consumer_group.v1~.schema.json`).
#[domain_model]
#[derive(Debug, Clone)]
pub struct ConsumerGroup {
    pub id: GtsInstanceId,
    pub kind: ConsumerGroupKind,
    pub tenant_id: Uuid,
    pub owner_principal_id: Uuid,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ConsumerGroup {
    /// Refuses a caller that is not the principal that created this anonymous
    /// group: its subject in its tenant. The same subject in another tenant is
    /// another principal. A named group is shared by permission, so it passes.
    /// It reads only the group and the caller, so it asks the PDP nothing.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] with [`ErrorCode::ConsumerGroupNotOwned`] when
    /// the group is anonymous and the caller's subject is not its
    /// `owner_principal_id` or the caller's tenant is not its `tenant_id`.
    pub fn require_owner(&self, ctx: &SecurityContext) -> Result<(), DomainError> {
        if self.kind == ConsumerGroupKind::Anonymous
            && (ctx.subject_id() != self.owner_principal_id
                || ctx.subject_tenant_id() != self.tenant_id)
        {
            return Err(DomainError::Forbidden {
                code: ErrorCode::ConsumerGroupNotOwned,
                message: format!(
                    "consumer group '{}' was created by another principal",
                    self.id
                ),
                resource: self.id.to_string(),
            });
        }
        Ok(())
    }
}

/// Which consumer groups a listing keeps: the documented filterable fields,
/// `id` (any of `ids`) and `kind`. An empty `ids` and no `kind` keep every
/// group.
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct ConsumerGroupFilter {
    pub ids: Vec<GtsInstanceId>,
    pub kind: Option<ConsumerGroupKind>,
}

impl ConsumerGroupFilter {
    /// Whether `group` passes every field this filter sets.
    #[must_use]
    pub fn matches(&self, group: &ConsumerGroup) -> bool {
        let id_matches = self.ids.is_empty() || self.ids.contains(&group.id);
        let kind_matches = self.kind.is_none_or(|kind| kind == group.kind);
        id_matches && kind_matches
    }
}

/// Caller-suppliable fields for `DeliveryService::create_consumer_group`
/// (`docs/openapi.yaml`'s `POST /v1/consumer-groups`'s `requestBody` is
/// itself optional; `client_agent` is required only once a body is sent at
/// all - both fields are `None` when no body is sent).
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct ConsumerGroupCreateInput {
    /// Diagnostic hint surfaced in operational logs only - no broker-side
    /// semantic, not persisted on `ConsumerGroup`
    /// (`gts.cf.core.events.consumer_group.v1~.schema.json`'s `CreateRequest.client_agent` doc
    /// comment).
    pub client_agent: Option<String>,
    pub description: Option<String>,
}

/// Backend segment manifest for one `(topic, partition)`
/// (`GET /v1/topics/segments`): the stored range and the segments the topic's
/// backend reports for the whole partition. `segments` entries are opaque
/// (`docs/openapi.yaml`). A partition that holds nothing, or is not yet
/// provisioned, has both sequences 0, no times and no segments.
#[domain_model]
#[derive(Debug, Clone)]
pub struct TopicSegmentManifest {
    pub topic: GtsInstanceId,
    pub partition: i32,
    pub start_sequence: Sequence,
    pub end_sequence: Sequence,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub segments: Vec<JsonValue>,
}
