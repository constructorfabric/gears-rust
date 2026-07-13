//! Tests for the [`NodesRegistryError`](super::NodesRegistryError) projection.
//!
//! Two suites:
//!
//! * `wire_vocabulary_round_trip` — pins every wire-string constant the
//!   projection introduces ([`crate::field`], [`crate::gts`]) to its
//!   `Problem` JSON path. A drift between an SDK constant and the wire
//!   trips here.
//! * `projection_tests` — exercises `From<CanonicalError>`, verifying
//!   each canonical category nodes-registry emits lands on the expected
//!   typed variant and that unmodeled categories preserve the canonical
//!   in `Other`.

use super::NodesRegistryError;

// ─────────────────────────────────────────────────────────────────────
// Wire-vocabulary round-trip
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod wire_vocabulary_round_trip {
    use crate::{field, gts};
    use toolkit_canonical_errors::{CanonicalError, Problem, resource_error};

    // Test scope mirroring the impl crate's `#[resource_error]` marker.
    // Its literal MUST equal `gts::NODE_RESOURCE_TYPE` — the
    // `node_resource_type_round_trip` test asserts that equality.
    #[resource_error(gts_id!("cf.nodes_registry.registry.node.v1~"))]
    struct NodeScope;

    fn problem(err: CanonicalError) -> serde_json::Value {
        serde_json::to_value(Problem::from(err)).expect("Problem serializes")
    }

    #[test]
    fn node_resource_type_round_trips_to_context_resource_type() {
        let err = NodeScope::not_found("missing").with_resource("x").create();
        let json = problem(err);
        assert_eq!(
            json["context"]["resource_type"],
            gts::NODE_RESOURCE_TYPE,
            "NODE_RESOURCE_TYPE must round-trip into context.resource_type",
        );
    }

    #[test]
    fn field_vocabulary_round_trips_to_field_violations() {
        let err = NodeScope::invalid_argument()
            .with_field_violation(
                field::INPUT_FIELD,
                "bad capability key",
                field::VALIDATION_ERROR,
            )
            .create();
        let json = problem(err);
        assert_eq!(
            json["context"]["field_violations"][0]["field"],
            field::INPUT_FIELD,
            "INPUT_FIELD must round-trip into field_violations[].field",
        );
        assert_eq!(
            json["context"]["field_violations"][0]["reason"],
            field::VALIDATION_ERROR,
            "VALIDATION_ERROR must round-trip into field_violations[].reason",
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Projection — From<CanonicalError>
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod projection_tests {
    use super::NodesRegistryError;
    use crate::{field, gts};
    use toolkit_canonical_errors::{CanonicalError, resource_error};

    #[resource_error(gts_id!("cf.nodes_registry.registry.node.v1~"))]
    struct NodeScope;

    #[test]
    fn not_found_projects_to_not_found_with_resource_type_and_name() {
        let err = NodeScope::not_found("no node with id abc")
            .with_resource("abc")
            .create();
        match NodesRegistryError::from(err) {
            NodesRegistryError::NotFound {
                resource_type,
                name,
                ..
            } => {
                assert_eq!(resource_type, gts::NODE_RESOURCE_TYPE);
                assert_eq!(name, "abc");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_projects_field_reason_and_description() {
        let err = NodeScope::invalid_argument()
            .with_field_violation(
                field::INPUT_FIELD,
                "bad capability key",
                field::VALIDATION_ERROR,
            )
            .create();
        match NodesRegistryError::from(err) {
            NodesRegistryError::InvalidArgument {
                field: field_name,
                reason,
                detail,
            } => {
                assert_eq!(field_name, field::INPUT_FIELD);
                assert_eq!(reason, field::VALIDATION_ERROR);
                assert_eq!(detail, "bad capability key");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_without_field_violation_falls_back_to_empty_field_reason() {
        // nodes-registry never emits the `Format` / `Constraint` / empty
        // shapes, but the projection must still handle them: field/reason
        // go empty and `detail` falls back to the canonical detail (the
        // format message, here).
        let err = NodeScope::invalid_argument()
            .with_format("malformed request shape")
            .create();
        match NodesRegistryError::from(err) {
            NodesRegistryError::InvalidArgument {
                field,
                reason,
                detail,
            } => {
                assert!(field.is_empty(), "field should be empty, got {field:?}");
                assert!(reason.is_empty(), "reason should be empty, got {reason:?}");
                assert_eq!(detail, "malformed request shape");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn internal_projects_to_internal() {
        let err = CanonicalError::internal("collection failed").create();
        assert!(matches!(
            NodesRegistryError::from(err),
            NodesRegistryError::Internal { .. }
        ));
    }

    #[test]
    fn unmodeled_category_falls_through_to_other() {
        // nodes-registry never emits ServiceUnavailable — it must be
        // preserved verbatim in the `Other` catch-all.
        let err = CanonicalError::service_unavailable().create();
        assert!(matches!(
            NodesRegistryError::from(err),
            NodesRegistryError::Other { .. }
        ));
    }
}
