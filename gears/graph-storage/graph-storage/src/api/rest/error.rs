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
        // DESIGN § Error Model: `permission_denied` with
        // `SOURCE_NAMESPACE_FORBIDDEN`, "never retry; request ownership
        // transfer". Deliberately not folded into the `NotFound` /
        // `AccessDenied` arm above: anti-enumeration hides what the caller has
        // no business knowing exists, and a namespace's owner is not that.
        DomainError::SourceNamespaceForbidden { namespace } => {
            tracing::info!(
                namespace = %namespace,
                "refused a write under a source namespace owned by another producer"
            );
            GraphNodeError::permission_denied()
                .with_reason(reasons::SOURCE_NAMESPACE_FORBIDDEN)
                .create()
        }
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

#[cfg(test)]
mod tests {
    use graph_storage_sdk::models::{ItemError, ItemFamily};
    use graph_storage_sdk::plugin_api::{GraphEngineError, GraphStoreError};

    use super::{CanonicalError, DomainError};

    fn status_of(error: DomainError) -> u16 {
        CanonicalError::from(error).status_code()
    }

    /// Every variant, with the status its category fixes.
    ///
    /// Written as one table rather than a case each because the property is
    /// the mapping's totality: a variant added without a classification falls
    /// through to `unknown`/500, which is a silent 500 on a failure someone
    /// meant to be actionable.
    #[test]
    fn every_domain_failure_carries_the_status_its_category_fixes() {
        let cases: Vec<(DomainError, u16)> = vec![
            (
                DomainError::Validation {
                    items: vec![ItemError {
                        index: 0,
                        family: ItemFamily::Node,
                        gts_type: None,
                        pointer: Some("/payload/severity".to_owned()),
                        message: "not one of the accepted values".to_owned(),
                    }],
                },
                400,
            ),
            // An empty item list still has to classify as a bad request:
            // "validation failed with nothing to say" is the shape a caller
            // sees when the collection was assembled and never filled.
            (DomainError::Validation { items: Vec::new() }, 400),
            (DomainError::invalid("a message"), 400),
            (
                DomainError::InvalidQuery {
                    message: "unknown field".to_owned(),
                },
                400,
            ),
            (
                DomainError::LimitExceeded {
                    what: "depth 9 is outside 1..=3".to_owned(),
                },
                400,
            ),
            (
                DomainError::CasConflict {
                    reason: "expected version 3".to_owned(),
                },
                409,
            ),
            (DomainError::Serialization, 409),
            (
                DomainError::StaleGeneration {
                    recorded: 7,
                    offered: 6,
                },
                400,
            ),
            (DomainError::IdempotencyMismatch, 409),
            (DomainError::IdempotencyExpired, 400),
            (DomainError::NotFound, 404),
            // Denied answers exactly as absent does: anti-enumeration.
            (DomainError::AccessDenied, 404),
            (
                DomainError::SourceNamespaceForbidden {
                    namespace: "scm".to_owned(),
                },
                403,
            ),
            (
                DomainError::ScopeUnservable {
                    reason: "allow_all".to_owned(),
                },
                400,
            ),
            (
                DomainError::VectorSearchUnavailable {
                    reason: "epoch 2 != 1".to_owned(),
                },
                400,
            ),
            (
                DomainError::Unsupported {
                    what: "topology".to_owned(),
                },
                501,
            ),
            (
                DomainError::Unavailable {
                    detail: "pool exhausted".to_owned(),
                },
                503,
            ),
            (DomainError::Deadline, 504),
            (DomainError::Cancelled, 499),
            (
                DomainError::Corrupt {
                    reason: "dangling edge".to_owned(),
                },
                500,
            ),
            (DomainError::internal("a bug"), 500),
        ];
        for (error, expected) in cases {
            let rendered = error.to_string();
            assert_eq!(
                status_of(error),
                expected,
                "`{rendered}` must answer {expected}"
            );
        }
    }

    /// The one denial that is not disguised as absence keeps its reason: a
    /// caller told "not found" would try to create what already exists.
    #[test]
    fn a_forbidden_namespace_says_so_rather_than_reading_as_absent() {
        let error = CanonicalError::from(DomainError::SourceNamespaceForbidden {
            namespace: "scm".to_owned(),
        });
        assert_eq!(error.status_code(), 403);
        assert!(
            format!("{error:?}").contains(super::reasons::SOURCE_NAMESPACE_FORBIDDEN),
            "the stable reason travels in the machine-readable slot: {error:?}"
        );
    }

    /// Per-item failures are reported per item, all of them, addressed by
    /// collection, index and JSON pointer -- a producer fixes a batch in one
    /// round trip, which is only possible if the second item is in there.
    #[test]
    fn every_item_of_a_failed_batch_is_reported_with_its_address() {
        let error = CanonicalError::from(DomainError::Validation {
            items: vec![
                ItemError {
                    index: 0,
                    family: ItemFamily::Node,
                    gts_type: None,
                    pointer: Some("/payload/a".to_owned()),
                    message: "first".to_owned(),
                },
                ItemError {
                    index: 3,
                    family: ItemFamily::Edge,
                    gts_type: None,
                    pointer: None,
                    message: "second".to_owned(),
                },
            ],
        });
        let rendered = format!("{error:?}");
        assert!(rendered.contains("nodes[0]/payload/a"), "{rendered}");
        assert!(rendered.contains("edges[3]"), "{rendered}");
        assert!(rendered.contains("second"), "{rendered}");
    }

    /// The store and engine errors a plugin may return all classify; an
    /// unrecognized one becomes an internal error that names itself rather
    /// than a panic.
    #[test]
    fn plugin_errors_classify_through_the_same_mapping() {
        assert_eq!(status_of(GraphStoreError::NotFound.into()), 404);
        assert_eq!(
            status_of(
                GraphStoreError::LimitExceeded {
                    what: "too many".to_owned()
                }
                .into()
            ),
            400
        );
        assert_eq!(status_of(GraphStoreError::Serialization.into()), 409);
        assert_eq!(
            status_of(GraphStoreError::Unsupported { what: "snapshots" }.into()),
            501
        );
        assert_eq!(status_of(GraphEngineError::Deadline.into()), 504);
        assert_eq!(
            status_of(
                GraphEngineError::ScopeNotEnforceable {
                    reason: "tenant subtree".to_owned()
                }
                .into()
            ),
            400
        );
        assert_eq!(
            status_of(GraphEngineError::Internal("boom".to_owned()).into()),
            500
        );
    }
}
