//! Domain errors for the Event Broker.
//!
//! Coarse-grained by HTTP-status family (`api/rest/error.rs` maps each
//! variant to one status code) rather than one variant per
//! `docs/openapi.yaml` machine code - the specific code (e.g.
//! `BadTypePattern`, `ConsumerGroupHasActiveMembers`) rides along as an
//! [`ErrorCode`] field instead, since there are dozens of them across the
//! Hard-Error Catalogs and they don't otherwise affect handling here.

use thiserror::Error;
use toolkit::domain_model;

use event_broker_sdk::error::{reasons, resources};

#[domain_model]
#[derive(Debug, Error)]
pub enum DomainError {
    /// Client-supplied input failed validation - `api/rest/error.rs` lifts
    /// this to `400`. `code` is the `docs/openapi.yaml` machine code (e.g.
    /// `BadTypePattern`, `InvalidMode`, `InvalidPartition`).
    #[error("{code}: {message}")]
    Validation { code: ErrorCode, message: String },

    /// An event's `data` did not satisfy its event type's `data_schema` -
    /// `api/rest/error.rs` lifts this to `422` with a `(payload)`/
    /// `schema_validation` `context.field_violations[]` entry and names the
    /// stream the publish targeted in `context.resource_name` (the common
    /// resource every publish error shares). The underlying `jsonschema`
    /// message is deliberately not carried: it can quote the submitted value,
    /// and the `(payload)` violation already says what is wrong without echoing
    /// request content.
    #[error("event data does not satisfy the schema on topic {topic}")]
    SchemaViolation { topic: String },

    /// A text field broke its printable-ASCII or byte-length rule - lifted to
    /// `400` with a `context.field_violations[]` entry naming the field.
    ///
    /// Distinct from `Validation` because that variant has no field slot, and
    /// a caller needs to know which field to correct. Deliberately carries no
    /// part of the submitted value: `detail` states what the rule requires and
    /// `reason` says which rule it was, so a rejection cannot become a channel
    /// for echoing request content back through logs and clients.
    #[error("{field}: {detail}")]
    TextField {
        field: &'static str,
        detail: String,
        reason: &'static str,
    },

    /// One or more seek entries named a position their partition does not
    /// admit - lifted to `400` with a `context.field_violations[]` entry per
    /// offender, so a caller correcting several does not need one round trip
    /// each.
    ///
    /// Every offender is reported rather than the first, and none of them
    /// carries the submitted value: the violation names the entry by its index
    /// in the request and the range that entry missed, both of which the
    /// caller can act on without the rejection quoting anything back.
    #[error("{} seek position(s) outside the admissible range", violations.len())]
    SeekOutOfRange {
        violations: Vec<event_broker_sdk::PositionViolation>,
    },

    /// Caller is not allowed to perform this operation on this resource -
    /// lifted to `403`. Distinct from `NotFound` even where
    /// `docs/openapi.yaml` deliberately conflates them for information
    /// hiding (callers decide whether to conflate at the REST layer).
    /// `resource` is the raw identifier (not a full sentence) - `DESIGN.md`'s
    /// Hard-Error Catalog carries this separately as `context.resource_name`.
    #[error("{code}: {message}")]
    Forbidden {
        code: ErrorCode,
        message: String,
        resource: String,
    },

    /// Referenced resource does not exist - lifted to `404`. `resource` is
    /// the raw identifier, matching `DESIGN.md`'s `context.resource_name`.
    #[error("{code}: {message}")]
    NotFound {
        code: ErrorCode,
        message: String,
        resource: String,
    },

    /// Request conflicts with current state - lifted to `409`. `resource` is
    /// the raw identifier, matching `DESIGN.md`'s `context.resource_name`.
    #[error("{code}: {message}")]
    Conflict {
        code: ErrorCode,
        message: String,
        resource: String,
    },

    /// Producer chain metadata does not match the broker's stored state.
    /// `docs/openapi.yaml` documents this as `412` - `api/rest/error.rs`
    /// keeps the canonical `FailedPrecondition` category but overrides the
    /// wire status to `412` via `TransportOverride`, with `last_sequence`
    /// echoed in the `Problem` context so the producer can resync.
    #[error(
        "sequence violation on topic {topic} partition {partition}: last_sequence={last_sequence}"
    )]
    SequenceViolation {
        topic: String,
        partition: i32,
        last_sequence: i64,
    },

    /// A batch exceeded the documented item-count ceiling. `docs/openapi.yaml`
    /// documents this as `413` - `api/rest/error.rs` overrides the wire
    /// status accordingly while keeping the canonical `InvalidArgument`
    /// category.
    #[error("batch too large: {count} events (max {max})")]
    BatchTooLarge { count: usize, max: usize },

    /// A batch's total payload exceeded the configured byte ceiling. Both the
    /// measured size and the ceiling are derived numbers, not request content,
    /// so they are safe to report. Same `413`/`InvalidArgument` treatment as
    /// [`BatchTooLarge`](Self::BatchTooLarge).
    #[error("batch payload too large: {bytes} bytes (max {max})")]
    BatchPayloadTooLarge { bytes: u64, max: u64 },

    /// Per-tenant/per-resource rate cap exceeded - lifted to `429` with a
    /// `Retry-After` header.
    #[error("{code}: {message}")]
    RateLimited {
        code: ErrorCode,
        message: String,
        retry_after_secs: u32,
    },

    /// Infrastructure failure reaching storage/cluster state - lifted to `503`.
    /// `reason` is a stable, non-sensitive category label (the wire `503`
    /// detail); the typed error rides in `source` for logs/`.source()` chaining
    /// and is never serialized, so a raw driver/cluster message can't leak into
    /// the response body. `source` is `None` for a failure with no underlying
    /// error (e.g. a not-yet-started pipeline).
    #[error("storage backend unavailable: {reason}")]
    StorageUnavailable {
        reason: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("internal error: {0}")]
    Internal(String),

    /// A recognized-but-unbuilt capability was requested (e.g. a named consumer
    /// group). Distinct from a not-found so a caller can tell "not supported yet"
    /// from "does not exist". Maps to 501.
    #[error("not implemented: {0}")]
    Unimplemented(String),
}

/// Every machine error code the gear emits, closed. Its single responsibility
/// is to map each code to its canonical target: the resource the error is
/// about ([`Self::resource_type`], a `types-registry` id) and, where the
/// category carries one, the machine reason a `Problem` body stamps
/// ([`Self::precondition_reason`]) plus the wire status a conflict overrides to
/// ([`Self::wire_override`]).
///
/// Replacing the former open `code: &'static str` set with this closed enum
/// makes a typo or a drift from the SDK's shared reason/resource vocabulary a
/// compile error instead of a silent downgrade to `EventBrokerError::Other`
/// across the wire. The resource and reason values come from
/// `event_broker_sdk::error::{resources, reasons}` so the gear and the SDK
/// cannot disagree on them.
///
/// The category (404 vs 403 vs 409 vs 400 vs 429) is not encoded here: it is
/// the `DomainError` variant that carries the code. `as_str` is the code's
/// stable wire spelling, used where the category renders the code into the
/// `Problem` detail (`Validation`) or as a violation `type_`
/// (`RateLimited`, the `Conflict` fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    // -- NotFound (404) --
    TopicNotFound,
    EventTypeNotFound,
    SubscriptionNotFound,
    ConsumerGroupNotFound,
    ProducerNotFound,
    PartitionNotFound,

    // -- Forbidden (403) --
    ProducerNotOwned,
    TopicNotAuthorized,
    EventTypeNotAuthorized,
    ConsumerGroupNotAuthorized,
    TenantIdNotAuthorized,
    NotAuthorizedToProduce,
    AuthzDenied,
    ScopeDenied,

    // -- Conflict (409/412) --
    PositionsNotSet,
    StreamingInProgress,
    ConsumerGroupHasActiveMembers,
    PartitionNotAssigned,
    TopologyVersionMismatch,

    // -- RateLimited (429) --
    RateLimited,

    // -- Validation (400) --
    BadTypePattern,
    InvalidBody,
    InvalidPath,
    InvalidQuery,
    InvalidSeekValue,
    InvalidSessionTimeout,
    InvalidSpec,
    InvalidSubjectType,
    InvalidSubjectTypePattern,
    InvalidTimestamp,
    MixedTopics,
    OffsetOutOfRange,
    PartitionKeyUnresolved,
    SubjectTypeNotAllowed,
}

impl ErrorCode {
    /// The code's stable wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TopicNotFound => "TopicNotFound",
            Self::EventTypeNotFound => "EventTypeNotFound",
            Self::SubscriptionNotFound => "SubscriptionNotFound",
            Self::ConsumerGroupNotFound => "ConsumerGroupNotFound",
            Self::ProducerNotFound => "ProducerNotFound",
            Self::PartitionNotFound => "PartitionNotFound",
            Self::RateLimited => "RateLimited",
            Self::ProducerNotOwned => "ProducerNotOwned",
            Self::TopicNotAuthorized => "TopicNotAuthorized",
            Self::EventTypeNotAuthorized => "EventTypeNotAuthorized",
            Self::ConsumerGroupNotAuthorized => "ConsumerGroupNotAuthorized",
            Self::TenantIdNotAuthorized => "TenantIdNotAuthorized",
            Self::NotAuthorizedToProduce => "NotAuthorizedToProduce",
            Self::AuthzDenied => "AuthzDenied",
            Self::ScopeDenied => "ScopeDenied",
            Self::PositionsNotSet => "PositionsNotSet",
            Self::StreamingInProgress => "StreamingInProgress",
            Self::ConsumerGroupHasActiveMembers => "ConsumerGroupHasActiveMembers",
            Self::PartitionNotAssigned => "PartitionNotAssigned",
            Self::TopologyVersionMismatch => "TopologyVersionMismatch",
            Self::BadTypePattern => "BadTypePattern",
            Self::InvalidBody => "InvalidBody",
            Self::InvalidPath => "InvalidPath",
            Self::InvalidQuery => "InvalidQuery",
            Self::InvalidSeekValue => "InvalidSeekValue",
            Self::InvalidSessionTimeout => "InvalidSessionTimeout",
            Self::InvalidSpec => "InvalidSpec",
            Self::InvalidSubjectType => "InvalidSubjectType",
            Self::InvalidSubjectTypePattern => "InvalidSubjectTypePattern",
            Self::InvalidTimestamp => "InvalidTimestamp",
            Self::MixedTopics => "MixedTopics",
            Self::OffsetOutOfRange => "OffsetOutOfRange",
            Self::PartitionKeyUnresolved => "PartitionKeyUnresolved",
            Self::SubjectTypeNotAllowed => "SubjectTypeNotAllowed",
        }
    }

    /// The `types-registry` id of the resource the error is about. Codes not
    /// tied to one addressable entity (request-shape validation, tenant/authz
    /// denials that name no entity) fall to the generic request resource.
    #[must_use]
    pub const fn resource_type(self) -> &'static str {
        match self {
            Self::TopicNotFound | Self::TopicNotAuthorized => resources::TOPIC,
            Self::EventTypeNotFound | Self::EventTypeNotAuthorized => resources::EVENT_TYPE,
            Self::SubscriptionNotFound
            | Self::PositionsNotSet
            | Self::StreamingInProgress
            | Self::PartitionNotAssigned
            | Self::TopologyVersionMismatch => resources::SUBSCRIPTION,
            Self::ConsumerGroupNotFound
            | Self::ConsumerGroupNotAuthorized
            | Self::ConsumerGroupHasActiveMembers => resources::CONSUMER_GROUP,
            Self::ProducerNotFound | Self::ProducerNotOwned => resources::PRODUCER,
            Self::PartitionNotFound => resources::PARTITION,
            _ => resources::REQUEST,
        }
    }

    /// The machine reason a `FailedPrecondition` body stamps for a `Conflict`
    /// code. Falls back to the code's own spelling for the generic
    /// `Aborted`-mapped conflicts, which name no shared vocabulary reason.
    #[must_use]
    pub const fn precondition_reason(self) -> &'static str {
        match self {
            Self::PositionsNotSet => reasons::POSITIONS_NOT_SET,
            Self::StreamingInProgress => reasons::STREAMING_IN_PROGRESS,
            Self::ConsumerGroupHasActiveMembers => reasons::CONSUMER_GROUP_HAS_ACTIVE_MEMBERS,
            Self::PartitionNotAssigned => reasons::PARTITION_NOT_ASSIGNED,
            Self::TopologyVersionMismatch => reasons::TOPOLOGY_VERSION_MISMATCH,
            _ => self.as_str(),
        }
    }

    /// The wire status a `Conflict` overrides to. `docs/openapi.yaml`
    /// documents these conflicts as `409`, except the topology-version
    /// mismatch, which uses the `412` if-match idiom. `None` keeps the
    /// category default.
    #[must_use]
    pub const fn wire_override(self) -> Option<u16> {
        match self {
            Self::PositionsNotSet
            | Self::StreamingInProgress
            | Self::ConsumerGroupHasActiveMembers
            | Self::PartitionNotAssigned => Some(409),
            Self::TopologyVersionMismatch => Some(412),
            _ => None,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// `toolkit_db::DbError`/`ScopeError` -> `DomainError` impls live in
// `infra/storage/error.rs`, not here - `domain/` has no infra dependencies
// (this file's own module doc comment, `domain/mod.rs`), and Rust's orphan
// rule permits a foreign-trait-for-local-type `impl` from anywhere in the
// crate, not only alongside the type's own definition.

/// `event_broker_sdk::StorageBackendError` -> `DomainError`
/// (eb-single-process-implementation D3; `event-broker-canonical-errors`'s
/// "`EventBrokerBackend` operations return backend errors with canonical
/// projection" requirement). `event_broker_sdk` is an established
/// `domain/`-allowed
/// dependency (`domain/authz.rs` already consumes its GTS constants), not
/// an infra type.
impl From<event_broker_sdk::StorageBackendError> for DomainError {
    fn from(err: event_broker_sdk::StorageBackendError) -> Self {
        use event_broker_sdk::StorageBackendError;
        match err {
            // Reached only where the caller could not attribute the failure
            // to one request entry - SEEK does, and builds
            // `SeekOutOfRange` itself. The range is broker state, so quoting
            // it here echoes nothing the caller submitted.
            StorageBackendError::OffsetOutOfRange {
                floor,
                ceiling,
                breached,
                ..
            } => DomainError::Validation {
                code: ErrorCode::OffsetOutOfRange,
                message: format!(
                    "the requested position is {} the valid range [{floor}, {ceiling}]",
                    breached.phrase()
                ),
            },
            StorageBackendError::PartitionNotFound { detail } => DomainError::NotFound {
                code: ErrorCode::PartitionNotFound,
                message: detail,
                resource: String::new(),
            },
            StorageBackendError::Unavailable { .. }
            | StorageBackendError::InvalidConfig { .. }
            | StorageBackendError::PersistFailed { .. }
            | StorageBackendError::ReadFailed { .. }
            // Never reaches a request: a retention pass runs on the broker's
            // own tick and answers no caller. Mapped rather than left to a
            // catch-all so a new variant stays a compile error here.
            | StorageBackendError::RetentionFailed { .. }
            | StorageBackendError::Internal(_) => DomainError::StorageUnavailable {
                reason: "event storage backend unavailable".to_owned(),
                source: Some(Box::new(err)),
            },
        }
    }
}

/// `EnforcerError::Denied` -> `Forbidden` with a generic `"AuthzDenied"` code;
/// call sites that need a specific `docs/openapi.yaml` code
/// (`TopicNotAuthorized`, `EventTypeNotAuthorized`, `NotAuthorizedToProduce`,
/// `TenantIdNotAuthorized`) overwrite `code` after this conversion
/// (`eb-authz-enforcement`'s design.md "code-per-call-site" - tenant scope is
/// its own `PolicyEnforcer` call via `domain::authz::tenant_authorized`, not
/// a separate SDK, so it flows through this same `From` impl). The two
/// non-denial variants are PDP/evaluation failures, not policy denials -
/// lifted to `Internal`, matching `oagw`'s own `EnforcerError`-to-domain-error
/// mapping.
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(err: authz_resolver_sdk::EnforcerError) -> Self {
        use authz_resolver_sdk::EnforcerError;
        match err {
            EnforcerError::Denied { deny_reason } => DomainError::Forbidden {
                code: ErrorCode::AuthzDenied,
                message: deny_reason
                    .and_then(|r| r.details)
                    .unwrap_or_else(|| "access denied by policy".to_owned()),
                resource: String::new(),
            },
            EnforcerError::CompileFailed(e) => {
                DomainError::Internal(format!("authz constraint compilation failed: {e}"))
            }
            EnforcerError::EvaluationFailed(e) => {
                DomainError::Internal(format!("authz evaluation failed: {e}"))
            }
        }
    }
}

impl From<event_broker_sdk::validate::TextFieldError> for DomainError {
    fn from(err: event_broker_sdk::validate::TextFieldError) -> Self {
        DomainError::TextField {
            field: err.field(),
            detail: err.detail().to_owned(),
            reason: err.rule().as_reason(),
        }
    }
}
