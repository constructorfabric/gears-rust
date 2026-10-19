//! The inbox for every non-usage upstream fact (DESIGN 11 §4.11, T-D-56; feature 01 §2.1, §2.2,
//! §4.1).
//!
//! An entry is keyed by `(tenant_id, source, business_id, version)` and carries the digest of its
//! payload. A new key is `accepted` and applied by the owning feature in the same transaction; the
//! same key with the same digest is a no-op; the same key with another digest is stored as its own
//! `quarantined` entry and nothing is applied, so the applied entry is never touched. An operator
//! releases a quarantined entry by applying or discarding it.
//!
//! The entry id is `UUIDv5(NS_RATING_INBOX, tenant | source | business_id | version | digest)`, so
//! one arrival always names one entry. Usage entries never pass this inbox: their capture and
//! deduplication are `bss_rating__usage_record` (T-D-61).

mod service;
pub(crate) mod source;

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

pub use service::{Inbox, InboxParts, ReleaseCommand};
pub use source::InboxSource;

use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;

/// Namespace of inbox entry ids (`UUIDv5`).
pub const NS_RATING_INBOX: Uuid = Uuid::from_u128(0x6b1f_3c2e_8a4d_5e7f_9a1b_2c3d_4e5f_6071);

/// The longest business id an upstream producer may use.
pub const MAX_BUSINESS_ID_LEN: usize = 255;

/// How an entry reached the inbox.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReceivedVia {
    /// A pull sweep of the owner's read.
    Pull,
    /// An event consumer (TARGET; disabled by default, T-D-71).
    Event,
}

impl ReceivedVia {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Event => "event",
        }
    }

    /// The value a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "pull" => Some(Self::Pull),
            "event" => Some(Self::Event),
            _ => None,
        }
    }
}

/// The inbox entry state machine (feature 01 §4.1).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InboxState {
    /// A new key, about to be applied in the same transaction.
    Accepted,
    /// Applied by the owning feature.
    Applied,
    /// Held for an operator: a digest conflict or an unsupported schema version.
    Quarantined,
    /// Released with resolution `discard`: kept for its retention, never applied.
    Discarded,
}

impl InboxState {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Applied => "applied",
            Self::Quarantined => "quarantined",
            Self::Discarded => "discarded",
        }
    }

    /// The state a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "accepted" => Some(Self::Accepted),
            "applied" => Some(Self::Applied),
            "quarantined" => Some(Self::Quarantined),
            "discarded" => Some(Self::Discarded),
            _ => None,
        }
    }

    /// The state an operator release moves this one to.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotQuarantined`] unless the entry is quarantined.
    pub fn release(
        self,
        entry_id: Uuid,
        resolution: ReleaseResolution,
    ) -> Result<Self, DomainError> {
        match self {
            // @cpt-begin:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-release
            Self::Quarantined => Ok(resolution.target_state()),
            // @cpt-end:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-release
            Self::Accepted | Self::Applied | Self::Discarded => {
                Err(DomainError::NotQuarantined { entry_id })
            }
        }
    }
}

impl fmt::Display for InboxState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an entry is quarantined, with what that reason refers to.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quarantine {
    /// The key already has a primary entry — its first arrival, applied or not — with another
    /// payload digest.
    DigestConflict {
        /// The key's primary entry.
        conflict_of: Uuid,
    },
    /// The payload's schema version is not supported (quarantined, never dropped).
    UnsupportedSchemaVersion,
}

impl Quarantine {
    /// The reason code.
    #[must_use]
    pub const fn reason(self) -> QuarantineReason {
        match self {
            Self::DigestConflict { .. } => QuarantineReason::DigestConflict,
            Self::UnsupportedSchemaVersion => QuarantineReason::UnsupportedSchemaVersion,
        }
    }

    /// The primary entry a digest conflict refers to.
    #[must_use]
    pub const fn conflict_of(self) -> Option<Uuid> {
        match self {
            Self::DigestConflict { conflict_of } => Some(conflict_of),
            Self::UnsupportedSchemaVersion => None,
        }
    }

    /// The quarantine a stored reason and `conflict_of` denote; `None` for a pair no quarantine
    /// produces.
    #[must_use]
    pub fn from_parts(reason: QuarantineReason, conflict_of: Option<Uuid>) -> Option<Self> {
        match (reason, conflict_of) {
            (QuarantineReason::DigestConflict, Some(conflict_of)) => {
                Some(Self::DigestConflict { conflict_of })
            }
            (QuarantineReason::UnsupportedSchemaVersion, None) => {
                Some(Self::UnsupportedSchemaVersion)
            }
            _ => None,
        }
    }
}

/// The reason code of a quarantine: the stored `quarantine_reason` and the exception's
/// `reason_code`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuarantineReason {
    /// The key exists with another payload digest.
    DigestConflict,
    /// The payload's schema version is not supported (quarantined, never dropped).
    UnsupportedSchemaVersion,
}

impl QuarantineReason {
    /// The stored name, also the exception's `reason_code`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DigestConflict => "digest_conflict",
            Self::UnsupportedSchemaVersion => "unsupported_schema_version",
        }
    }

    /// The reason a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "digest_conflict" => Some(Self::DigestConflict),
            "unsupported_schema_version" => Some(Self::UnsupportedSchemaVersion),
            _ => None,
        }
    }
}

/// What an operator release does with a quarantined entry.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReleaseResolution {
    /// Apply the entry's payload through its source's owning feature.
    Apply,
    /// Keep the entry for its retention and never apply it.
    Discard,
}

impl ReleaseResolution {
    /// The state a release with this resolution ends in.
    #[must_use]
    pub const fn target_state(self) -> InboxState {
        match self {
            // @cpt-begin:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-discard
            Self::Discard => InboxState::Discarded,
            // @cpt-end:cpt-cf-bss-rating-state-foundation-inbox:p1:inst-is-discard
            Self::Apply => InboxState::Applied,
        }
    }

    /// The name recorded as the resolution of the entry's exception.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Discard => "discard",
        }
    }
}

/// The idempotency key of an upstream fact. Built only through [`InboxKey::new`], so every key
/// holds its bounds.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InboxKey {
    tenant_id: Uuid,
    source: InboxSource,
    business_id: String,
    version: i64,
}

impl InboxKey {
    /// A validated key.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] for an empty or oversized business id, one holding a
    /// control character, or a negative version.
    pub fn new(
        tenant_id: Uuid,
        source: InboxSource,
        business_id: impl Into<String>,
        version: i64,
    ) -> Result<Self, DomainError> {
        let business_id = business_id.into();
        if business_id.is_empty()
            || business_id.len() > MAX_BUSINESS_ID_LEN
            || business_id.chars().any(char::is_control)
        {
            return Err(DomainError::InvalidArgument {
                field: "business_id",
                detail: "a business id is 1-255 bytes without control characters".to_owned(),
                reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
            });
        }
        if version < 0 {
            return Err(DomainError::InvalidArgument {
                field: "version",
                detail: "a version is not negative".to_owned(),
                reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
            });
        }
        Ok(Self {
            tenant_id,
            source,
            business_id,
            version,
        })
    }

    /// The tenant that owns the payload's data (§3.7 ownership columns).
    #[must_use]
    pub const fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    /// The fact family.
    #[must_use]
    pub const fn source(&self) -> InboxSource {
        self.source
    }

    /// The producer's business id (`fact_id`, `segment_id`, …).
    #[must_use]
    pub fn business_id(&self) -> &str {
        &self.business_id
    }

    /// The producer's version of that business id.
    #[must_use]
    pub const fn version(&self) -> i64 {
        self.version
    }

    /// The entry id of this key with `digest`.
    #[must_use]
    pub fn entry_id(&self, digest: &Sha256Hex) -> Uuid {
        let name = format!(
            "{}|{}|{}|{}|{}",
            self.tenant_id,
            self.source.as_str(),
            self.business_id,
            self.version,
            digest.as_str()
        );
        Uuid::new_v5(&NS_RATING_INBOX, name.as_bytes())
    }
}

/// Where an inbound fact came from: the identity the producer check verifies.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProducerOrigin {
    /// Rating read it through the owner's SDK, so the owner is the producer by construction.
    PullRead,
    /// An event whose producer the broker authenticated. An event adapter takes `producer` from
    /// the envelope's verified producer metadata, never from the payload or another field the
    /// sender writes, so the check in [`ProducerPolicy::verify`] is an authentication.
    Event { producer: String },
}

impl ProducerOrigin {
    /// How the entry was received.
    #[must_use]
    pub const fn received_via(&self) -> ReceivedVia {
        match self {
            Self::PullRead => ReceivedVia::Pull,
            Self::Event { .. } => ReceivedVia::Event,
        }
    }
}

/// An upstream fact presented to the inbox.
///
/// The digest is Rating's own, computed here over the payload, so a caller cannot present one
/// payload under another's digest: two payloads that differ give two digests, and the second is a
/// conflict, never a duplicate. A digest the producer sends is part of the payload and is hashed
/// with it.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct InboundFact {
    /// Its idempotency key. The source adapter takes the tenant from the payload's owning-tenant
    /// field, so the grant [`Inbox::admit`] checks is that of the tenant the payload belongs to.
    ///
    /// [`Inbox::admit`]: crate::domain::inbox::Inbox::admit
    pub key: InboxKey,
    /// Where it came from.
    pub origin: ProducerOrigin,
    /// Whether the owning feature's adapter supports the payload's schema version.
    pub schema_supported: bool,
    digest: Sha256Hex,
    payload: Value,
}

impl InboundFact {
    /// A fact with its payload, normalized by the source adapter to canonical text (DESIGN §3.3),
    /// and the digest of that payload.
    #[must_use]
    pub fn new(
        key: InboxKey,
        payload: Value,
        origin: ProducerOrigin,
        schema_supported: bool,
    ) -> Self {
        Self {
            key,
            origin,
            schema_supported,
            digest: Sha256Hex::of_json(&payload),
            payload,
        }
    }

    /// The digest of the payload's canonical JSON text.
    #[must_use]
    pub fn digest(&self) -> &Sha256Hex {
        &self.digest
    }

    /// The payload as the adapter presented it.
    #[must_use]
    pub fn payload(&self) -> &Value {
        &self.payload
    }
}

/// A stored inbox entry.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct InboxEntry {
    /// `UUIDv5` of the key and digest; the `{id}` of the release route.
    pub entry_id: Uuid,
    /// The key.
    pub key: InboxKey,
    /// The payload digest.
    pub payload_digest: Sha256Hex,
    /// The payload, kept for its retention.
    pub payload: Value,
    /// How it arrived.
    pub received_via: ReceivedVia,
    /// Its state.
    pub state: InboxState,
    /// Why it is (or was) quarantined; kept after a release.
    pub quarantine: Option<Quarantine>,
    /// When it arrived.
    pub received_at: OffsetDateTime,
    /// When it was applied.
    pub applied_at: Option<OffsetDateTime>,
}

impl InboxEntry {
    /// A new entry for `fact`, `accepted`, or `quarantined` when `quarantine` names why.
    #[must_use]
    pub fn arriving(
        fact: &InboundFact,
        received_at: OffsetDateTime,
        quarantine: Option<Quarantine>,
    ) -> Self {
        let state = if quarantine.is_some() {
            InboxState::Quarantined
        } else {
            InboxState::Accepted
        };
        Self {
            entry_id: fact.key.entry_id(&fact.digest),
            key: fact.key.clone(),
            payload_digest: fact.digest.clone(),
            payload: fact.payload.clone(),
            received_via: fact.origin.received_via(),
            state,
            quarantine,
            received_at,
            applied_at: None,
        }
    }
}

/// What `Inbox::accept` did with a fact.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum AcceptOutcome {
    /// A new key: stored and applied in the same transaction.
    Applied(InboxEntry),
    /// The key and digest were already stored: nothing happened.
    Duplicate(InboxEntry),
    /// Stored quarantined (a digest conflict or an unsupported schema): nothing was applied.
    Quarantined(InboxEntry),
}

impl AcceptOutcome {
    /// The entry the outcome is about.
    #[must_use]
    pub fn entry(&self) -> &InboxEntry {
        match self {
            Self::Applied(entry) | Self::Duplicate(entry) | Self::Quarantined(entry) => entry,
        }
    }
}

/// The owning feature's apply step for one inbox source, run in the inbox transaction.
#[async_trait]
pub trait InboxApplier: Send + Sync {
    /// The source this applier owns.
    fn source(&self) -> InboxSource;

    /// Applies `entry` under `scope` (narrowed to the entry's tenant) inside `tx`. An error rolls
    /// the whole inbox transaction back, the entry included.
    ///
    /// # Errors
    ///
    /// Whatever the owning feature refuses with.
    async fn apply(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry: &InboxEntry,
    ) -> Result<(), DomainError>;
}

/// The producer identity each source accepts on its event path (feature 01 §2.1 step 1). A pull
/// read is verified by construction; an event is accepted only from the producer configured for
/// a source whose event intake is enabled.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProducerPolicy {
    event_producers: std::collections::BTreeMap<InboxSource, String>,
}

impl ProducerPolicy {
    /// A policy that accepts events of `source` from `producer` only.
    #[must_use]
    pub fn with_event_producer(mut self, source: InboxSource, producer: impl Into<String>) -> Self {
        self.event_producers.insert(source, producer.into());
        self
    }

    /// Verifies the producer identity of a fact of `source`.
    ///
    /// # Errors
    ///
    /// [`DomainError::ProducerNotVerified`] for an event of a source with no enabled event intake
    /// or from another producer.
    pub fn verify(&self, source: InboxSource, origin: &ProducerOrigin) -> Result<(), DomainError> {
        match origin {
            ProducerOrigin::PullRead => Ok(()),
            ProducerOrigin::Event { producer } => {
                if self.event_producers.get(&source) == Some(producer) {
                    Ok(())
                } else {
                    Err(DomainError::ProducerNotVerified {
                        inbox_source: source,
                    })
                }
            }
        }
    }
}

/// The appliers of the sources whose owning features are built, by source.
pub type Appliers = std::collections::BTreeMap<InboxSource, Arc<dyn InboxApplier>>;

/// A conditional state move of an inbox entry. Only a move into `applied` carries an instant.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxTransition {
    /// `accepted` → `applied`: the owning feature applied a new entry.
    ApplyAccepted { at: OffsetDateTime },
    /// `quarantined` → `applied`: an operator released the entry with `apply`.
    ApplyQuarantined { at: OffsetDateTime },
    /// `quarantined` → `discarded`: an operator released the entry with `discard`.
    DiscardQuarantined,
}

impl InboxTransition {
    /// The state the entry must still be in.
    #[must_use]
    pub const fn from(self) -> InboxState {
        match self {
            Self::ApplyAccepted { .. } => InboxState::Accepted,
            Self::ApplyQuarantined { .. } | Self::DiscardQuarantined => InboxState::Quarantined,
        }
    }

    /// The state it moves to.
    #[must_use]
    pub const fn to(self) -> InboxState {
        match self {
            Self::ApplyAccepted { .. } | Self::ApplyQuarantined { .. } => InboxState::Applied,
            Self::DiscardQuarantined => InboxState::Discarded,
        }
    }

    /// When the entry was applied, for a move into `applied`.
    #[must_use]
    pub const fn applied_at(self) -> Option<OffsetDateTime> {
        match self {
            Self::ApplyAccepted { at } | Self::ApplyQuarantined { at } => Some(at),
            Self::DiscardQuarantined => None,
        }
    }
}

/// `bss_rating__inbox`: insert-only except `state` (with `applied_at`).
#[async_trait]
pub trait InboxRepo: Send + Sync {
    /// Every stored entry of `key` within `scope`: its primary entry and any digest conflicts.
    async fn find_by_key(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &InboxKey,
    ) -> Result<Vec<InboxEntry>, DomainError>;

    /// The entry `entry_id` if it lies within `scope`.
    async fn find_by_id(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry_id: Uuid,
    ) -> Result<Option<InboxEntry>, DomainError>;

    /// Inserts a new entry; [`DomainError::Concurrent`] when the key was stored concurrently.
    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry: &InboxEntry,
    ) -> Result<(), DomainError>;

    /// Makes `transition` on `entry_id` if the entry is still in the transition's source state (a
    /// CAS): `true` when this call made it.
    async fn transition(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry_id: Uuid,
        transition: InboxTransition,
    ) -> Result<bool, DomainError>;
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
