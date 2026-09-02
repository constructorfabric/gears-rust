//! `DomainError -> CanonicalError`: the single authoritative mapping.
//!
//! The adapter only renders — REST and `ClientHub` share this `From` impl, so
//! they cannot classify one failure differently. Category names are exactly
//! those `#[resource_error]` generates; there is no `internal` category, so
//! unexpected failures map to `unknown`. The stable reason travels in the
//! machine-readable slot each category provides (`with_reason`,
//! `with_precondition_violation`, `with_field_violation`), never in the
//! human-readable detail — clients never parse detail strings.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::{DomainError, reasons};

/// Errors attributable to a graph node as a resource.
#[resource_error(gts_id!("cf.core.graph.node.v1~"))]
pub struct GraphNodeError;

/// Errors attributable to a registered ontology type.
#[resource_error(gts_id!("cf.core.graph.type.v1~"))]
pub struct GraphTypeError;

/// The field name a per-item violation is reported under: which collection,
/// which index, and the JSON pointer inside it.
fn violation_field(item: &graph_storage_sdk::models::ItemError) -> String {
    let family = match item.family {
        graph_storage_sdk::models::ItemFamily::Node => "nodes",
        graph_storage_sdk::models::ItemFamily::Edge => "edges",
    };
    format!(
        "{family}[{}]{}",
        item.index,
        item.pointer.as_deref().unwrap_or("")
    )
}

/// Per-item validation failures, every one of them, so a producer fixes a
/// whole batch in one round trip.
fn validation_error(items: &[graph_storage_sdk::models::ItemError]) -> CanonicalError {
    let Some((first, rest)) = items.split_first() else {
        return GraphNodeError::invalid_argument()
            .with_field_violation("request", "validation failed", reasons::SCHEMA_VIOLATION)
            .create();
    };
    let mut builder = GraphNodeError::invalid_argument().with_field_violation(
        violation_field(first),
        first.message.clone(),
        reasons::SCHEMA_VIOLATION,
    );
    for item in rest {
        builder = builder.with_field_violation(
            violation_field(item),
            item.message.clone(),
            reasons::SCHEMA_VIOLATION,
        );
    }
    builder.create()
}

/// The failures a caller can act on by changing the request.
fn client_correctable(error: DomainError) -> Result<CanonicalError, DomainError> {
    Ok(match error {
        DomainError::Validation { items } => validation_error(&items),
        DomainError::InvalidArgument { message } => GraphNodeError::invalid_argument()
            .with_field_violation("request", message, reasons::LIMIT_COMBINATION)
            .create(),
        DomainError::InvalidQuery { message } => GraphNodeError::invalid_argument()
            .with_field_violation("query", message, reasons::SCHEMA_VIOLATION)
            .create(),
        DomainError::LimitExceeded { what } => GraphNodeError::out_of_range(what.clone())
            .with_field_violation("limit", what, reasons::LIMIT_EXCEEDED)
            .create(),
        DomainError::CasConflict { reason } => GraphNodeError::aborted(reason)
            .with_reason(reasons::CAS_CONFLICT)
            .create(),
        DomainError::Serialization => {
            GraphNodeError::aborted("serialization failure under concurrent ingest")
                .with_reason(reasons::SERIALIZATION)
                .create()
        }
        DomainError::StaleGeneration { recorded, offered } => GraphNodeError::failed_precondition()
            .with_precondition_violation(
                "source_generation",
                format!("generation {offered} is older than the recorded {recorded}"),
                reasons::STALE_GENERATION,
            )
            .create(),
        DomainError::IdempotencyMismatch => {
            GraphNodeError::aborted("idempotency key reused with a different request")
                .with_reason(reasons::IDEMPOTENCY_MISMATCH)
                .create()
        }
        DomainError::IdempotencyExpired => GraphNodeError::failed_precondition()
            .with_precondition_violation(
                "idempotency_key",
                "receipt expired; reconcile and issue a new logical request",
                reasons::IDEMPOTENCY_KEY_EXPIRED,
            )
            .create(),
        other => return Err(other),
    })
}

/// Routing and capability outcomes: which implementation, if any, could have
/// served the call.
fn routing_outcome(error: DomainError) -> Result<CanonicalError, DomainError> {
    Ok(match error {
        // Unauthorized and unknown are indistinguishable by contract
        // (anti-enumeration).
        DomainError::NotFound | DomainError::AccessDenied => GraphNodeError::not_found("not found")
            .with_resource("")
            .create(),
        DomainError::ScopeUnservable { reason } => GraphNodeError::failed_precondition()
            .with_precondition_violation("scope", reason, reasons::SCOPE_UNSERVABLE)
            .create(),
        // Not `unavailable`: nothing is down and a retry cannot help. The
        // deployment has to re-embed, and the caller has to hear which
        // precondition is unmet rather than be told to wait.
        DomainError::VectorSearchUnavailable { reason } => GraphNodeError::failed_precondition()
            .with_precondition_violation(
                "embedding_space",
                reason,
                reasons::EMBEDDING_SPACE_MISMATCH,
            )
            .create(),
        DomainError::Unsupported { what } => GraphNodeError::unimplemented(what).create(),
        other => return Err(other),
    })
}

/// A dependency outage. The reason is a protected diagnostic: it goes to the
/// log, never into the public detail.
fn unavailable(detail: &str) -> CanonicalError {
    tracing::warn!(reason = %detail, "graph-storage dependency unavailable");
    CanonicalError::service_unavailable()
        .with_retry_after_seconds(5)
        .create()
}

fn corrupt(reason: String) -> CanonicalError {
    tracing::error!(reason = %reason, "graph-storage detected durable corruption");
    GraphNodeError::data_loss(reason).with_resource("").create()
}

fn unexpected(error: &DomainError) -> CanonicalError {
    tracing::error!(detail = %error, "unexpected graph-storage failure");
    GraphNodeError::unknown("internal error").create()
}

/// Operational outcomes: a dependency, a deadline, corruption, or something
/// unforeseen. None of them is fixable by changing the request.
fn operational_outcome(error: DomainError) -> CanonicalError {
    match error {
        DomainError::Unavailable { detail } => unavailable(&detail),
        DomainError::Deadline => {
            GraphNodeError::deadline_exceeded("operation exceeded its deadline").create()
        }
        DomainError::Cancelled => GraphNodeError::cancelled().create(),
        DomainError::Corrupt { reason } => corrupt(reason),
        // The earlier classifications answered every other arm.
        ref other => unexpected(other),
    }
}

impl From<DomainError> for CanonicalError {
    fn from(error: DomainError) -> Self {
        // Three classifications, tried in order, so no failure can fall
        // through unclassified: what the caller can fix, where it could have
        // been served, and what went wrong underneath.
        client_correctable(error)
            .or_else(routing_outcome)
            .unwrap_or_else(operational_outcome)
    }
}
