//! Gear-local REST DTOs of the operator plane (DESIGN §3.3). Names carry a `Rating` prefix so they
//! stay unique in the shared `OpenAPI` document; every DTO changes additively within `/v1`
//! (`tests/golden/v1`).

use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::inbox::{InboxEntry, InboxState, Quarantine, ReceivedVia, ReleaseResolution};

/// `POST /bss-rating/v1/inbox/{id}/release` request.
#[toolkit_macros::api_dto(request)]
#[derive(Clone)]
#[serde(deny_unknown_fields)]
pub struct RatingInboxReleaseRequest {
    /// Why the operator releases the entry: 1–1000 characters once trimmed, no control characters.
    /// Audited.
    // The bounds are `domain::audit::MAX_REASON_LEN` and `OperatorReason::parse`.
    #[schema(min_length = 1, max_length = 1000)]
    pub reason: String,
    /// `apply` hands the payload to its owning feature; `discard` keeps it unapplied.
    pub resolution: RatingInboxResolution,
}

/// What a release does with a quarantined entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(request, response)]
pub enum RatingInboxResolution {
    /// Apply the entry's payload.
    Apply,
    /// Keep the entry for its retention and never apply it.
    Discard,
}

impl From<RatingInboxResolution> for ReleaseResolution {
    fn from(value: RatingInboxResolution) -> Self {
        match value {
            RatingInboxResolution::Apply => Self::Apply,
            RatingInboxResolution::Discard => Self::Discard,
        }
    }
}

/// The state of an inbox entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum RatingInboxState {
    /// A new key, applied in the same transaction.
    Accepted,
    /// Applied by its owning feature.
    Applied,
    /// Held for an operator.
    Quarantined,
    /// Released with `discard`.
    Discarded,
}

impl From<InboxState> for RatingInboxState {
    fn from(value: InboxState) -> Self {
        match value {
            InboxState::Accepted => Self::Accepted,
            InboxState::Applied => Self::Applied,
            InboxState::Quarantined => Self::Quarantined,
            InboxState::Discarded => Self::Discarded,
        }
    }
}

/// `InboxEntryView`: an inbox entry without its payload.
#[toolkit_macros::api_dto(response)]
pub struct RatingInboxEntryView {
    /// The entry id (the `{id}` of the release route).
    pub entry_id: Uuid,
    /// The tenant that owns the payload's data.
    pub tenant_id: Uuid,
    /// `fact`, `segment`, `scope`, `coverage`, `subscription_version` or `billing_hint` (an open set).
    pub source: String,
    /// The producer's business id.
    pub business_id: String,
    /// The producer's version.
    pub version: i64,
    /// Lowercase hex SHA-256 of the canonical payload.
    pub payload_digest: String,
    /// `pull` or `event` (an open set).
    pub received_via: String,
    /// The entry's state.
    pub state: RatingInboxState,
    /// `digest_conflict` or `unsupported_schema_version` (an open set), kept after a release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_reason: Option<String>,
    /// For a digest conflict, the key's primary entry: its first arrival, which may itself be
    /// quarantined (an unsupported schema version) and so never applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_of: Option<Uuid>,
    /// When the entry arrived.
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    /// When it was applied.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub applied_at: Option<OffsetDateTime>,
}

impl From<InboxEntry> for RatingInboxEntryView {
    fn from(entry: InboxEntry) -> Self {
        Self {
            entry_id: entry.entry_id,
            tenant_id: entry.key.tenant_id(),
            source: entry.key.source().as_str().to_owned(),
            business_id: entry.key.business_id().to_owned(),
            version: entry.key.version(),
            payload_digest: entry.payload_digest.as_str().to_owned(),
            received_via: match entry.received_via {
                ReceivedVia::Pull => "pull",
                ReceivedVia::Event => "event",
            }
            .to_owned(),
            state: entry.state.into(),
            quarantine_reason: entry.quarantine.map(|q| q.reason().as_str().to_owned()),
            conflict_of: entry.quarantine.and_then(Quarantine::conflict_of),
            received_at: entry.received_at,
            applied_at: entry.applied_at,
        }
    }
}
