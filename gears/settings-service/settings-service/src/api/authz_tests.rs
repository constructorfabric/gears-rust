//! Tests for the enforcement point's fail-closed contract.
//!
//! Acceptance criteria: FEATURE `gear-foundation.md` §6 — *a request whose
//! authorization decision cannot be obtained is denied rather than allowed* —
//! and CDSL steps `inst-gf-authz-4` / `inst-gf-authz-5`.
//!
//! `access_scope` itself needs a live policy decision point, so what is pinned
//! here is the projection every one of its failures passes through.

use authz_resolver_sdk::EnforcerError;
use authz_resolver_sdk::pep::ConstraintCompileError;
use serde_json::Value;
use toolkit_canonical_errors::{CanonicalError, Problem};

use super::{deny, resource};
use crate::domain::error::DomainError;
use settings_service_sdk::gts;

fn problem_of(err: DomainError) -> Value {
    serde_json::to_value(Problem::from(CanonicalError::from(err))).expect("serializes")
}

/// Every shape `EnforcerError` can take. Kept as one list so a variant added
/// upstream is a compile error here as well as in the mapping.
fn every_failure() -> Vec<EnforcerError> {
    vec![
        EnforcerError::Denied { deny_reason: None },
        // `EvaluationFailed` carries the canonical error the resolver surfaced;
        // which category it was does not matter to the projection, only that it
        // never becomes an allow.
        EnforcerError::EvaluationFailed(CanonicalError::service_unavailable().create()),
        EnforcerError::EvaluationFailed(CanonicalError::service_unavailable().create()),
        EnforcerError::CompileFailed(ConstraintCompileError::ConstraintsRequiredButAbsent),
        EnforcerError::CompileFailed(ConstraintCompileError::AllConstraintsFailed {
            reason: "unsupported constraint operator".to_owned(),
        }),
    ]
}

#[test]
fn every_enforcement_failure_denies() {
    // The core of the contract. An explicit deny, an unreachable decision
    // point, and an uncompilable constraint set are all denials — there is no
    // failure shape that lets a request through.
    for err in every_failure() {
        let label = format!("{err:?}");
        assert!(
            matches!(
                deny(&resource::CATEGORY, &err),
                DomainError::Unauthorized { .. }
            ),
            "`{label}` must deny"
        );
    }
}

#[test]
fn an_unreachable_decision_point_denies_rather_than_defaults() {
    // Step 4 of the enforcement algorithm, stated as its own test because it is
    // the one an implementation is most tempted to get wrong: an outage in the
    // policy service must not become an outage-shaped allow.
    let err = EnforcerError::EvaluationFailed(CanonicalError::service_unavailable().create());
    assert!(matches!(
        deny(&resource::CATEGORY, &err),
        DomainError::Unauthorized { .. }
    ));
}

#[test]
fn uncompilable_constraints_deny_rather_than_widen() {
    // A decision arrived, but its constraints could not become a predicate.
    // Proceeding would mean applying *no* predicate — broader than anything the
    // policy point could have returned.
    let err = EnforcerError::CompileFailed(ConstraintCompileError::AllConstraintsFailed {
        reason: "unknown constraint operator".to_owned(),
    });
    assert!(matches!(
        deny(&resource::CATEGORY, &err),
        DomainError::Unauthorized { .. }
    ));
}

#[test]
fn all_denials_are_indistinguishable_on_the_wire() {
    // A caller must not learn *why* it was denied. Told apart, the three cases
    // reveal whether the policy service is down, whether a policy names them
    // specifically, and — combined with a probe — whether the resource exists.
    let rendered: Vec<Value> = every_failure()
        .into_iter()
        .map(|err| problem_of(deny(&resource::CATEGORY, &err)))
        .collect();
    let first = &rendered[0];
    for other in &rendered[1..] {
        assert_eq!(first, other, "denials must not be distinguishable");
    }
}

#[test]
fn a_denial_discloses_nothing_about_the_target() {
    let doc = problem_of(deny(
        &resource::CATEGORY,
        &EnforcerError::Denied { deny_reason: None },
    ));
    let rendered = serde_json::to_string(&doc).expect("serializes");
    assert_eq!(doc["status"], 403);
    for leak in [
        "unreachable",
        "connection",
        "constraint",
        "not found",
        "exists",
    ] {
        assert!(
            !rendered.contains(leak),
            "`{leak}` must not appear in a denial: {rendered}"
        );
    }
}

#[test]
fn the_resource_vocabulary_is_the_gts_type_ids() {
    // Policies are written against these ids, so they are a contract with the
    // policy decision point rather than an internal naming choice.
    assert_eq!(
        resource::DECLARATION.name(),
        "gts.cf.core.settings.declaration.v1~"
    );
    assert_eq!(resource::VALUE.name(), "gts.cf.core.settings.value.v1~");
    assert_eq!(
        resource::CATEGORY.name(),
        "gts.cf.core.settings.category.v1~"
    );
}

#[test]
fn a_denial_names_the_resource_that_was_enforced() {
    // Found by running the service: every denial used to carry the declaration
    // type, so a category request was refused with the wrong resource in its
    // context -- not a leak, but wrong metadata for anyone reading a log or an
    // audit trail.
    for (resource, expected) in [
        (&resource::CATEGORY, gts::CATEGORY_SCHEMA),
        (&resource::VALUE, gts::VALUE_SCHEMA),
        (&resource::DECLARATION, gts::DECLARATION_SCHEMA),
    ] {
        let doc = problem_of(deny(resource, &EnforcerError::Denied { deny_reason: None }));
        assert_eq!(doc["context"]["resource_type"], expected);
    }
}

// ---- `settings_authz_denial_total`: counted at the enforcement point, before
// the refusal is folded into the one `Unauthorized`.

mod counted {
    use authz_resolver_sdk::{AccessRequest, PolicyEnforcer};
    use uuid::Uuid;

    use super::{DomainError, EnforcerError, problem_of, resource};
    use crate::api::authz::{Operation, access_scope, may_read_unmasked, probe_scope};
    use crate::domain::ports::labels;
    use crate::test_support::{
        RecordingSecurityMetrics, context_for, denying_enforcer, uncompilable_enforcer,
        unreachable_enforcer,
    };

    async fn refused(
        enforcer: &PolicyEnforcer,
        metrics: &RecordingSecurityMetrics,
        operation: Operation,
    ) -> DomainError {
        access_scope(
            enforcer,
            metrics,
            &context_for(Uuid::new_v4()),
            &resource::VALUE,
            operation,
            None,
        )
        .await
        .expect_err("refused")
    }

    #[test]
    fn the_operations_are_the_designs_counted_set() {
        // The type is the finite set: what an enforcement point can ask is
        // exactly what the counter's `operation` label may hold, in the
        // DESIGN's order. `read_unmasked` is not a variant, so it cannot
        // reach the counted path at all.
        assert_eq!(
            Operation::ALL.map(Operation::label),
            labels::AUTHZ_OPERATIONS
        );
    }

    #[tokio::test]
    async fn a_deny_counts_one_denied_under_the_operation_asked() {
        let metrics = RecordingSecurityMetrics::default();
        let err = refused(&denying_enforcer(), &metrics, Operation::Write).await;
        assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
        assert_eq!(metrics.denials(), vec![("write", "denied")]);
    }

    #[tokio::test]
    async fn an_unreachable_decision_point_counts_one_unavailable() {
        let metrics = RecordingSecurityMetrics::default();
        let err = refused(&unreachable_enforcer(), &metrics, Operation::Read).await;
        assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
        assert_eq!(metrics.denials(), vec![("read", "unavailable")]);
    }

    #[tokio::test]
    async fn uncompilable_constraints_count_one_constraints_refusal() {
        // The double really is one the enforcer cannot compile.
        let direct = uncompilable_enforcer()
            .access_scope_with(
                &context_for(Uuid::new_v4()),
                &resource::VALUE,
                "delete",
                None,
                &AccessRequest::new().require_constraints(false),
            )
            .await;
        assert!(
            matches!(direct, Err(EnforcerError::CompileFailed(_))),
            "{direct:?}"
        );

        let metrics = RecordingSecurityMetrics::default();
        let err = refused(&uncompilable_enforcer(), &metrics, Operation::Delete).await;
        assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
        assert_eq!(metrics.denials(), vec![("delete", "constraints")]);
    }

    #[tokio::test]
    async fn the_response_is_the_same_unauthorized_for_all_three_reasons() {
        let metrics = RecordingSecurityMetrics::default();
        let bodies = [
            problem_of(refused(&denying_enforcer(), &metrics, Operation::Read).await),
            problem_of(refused(&unreachable_enforcer(), &metrics, Operation::Read).await),
            problem_of(refused(&uncompilable_enforcer(), &metrics, Operation::Read).await),
        ];
        assert_eq!(bodies[0], bodies[1]);
        assert_eq!(bodies[1], bodies[2]);
        assert_eq!(
            metrics.denials(),
            vec![
                ("read", "denied"),
                ("read", "unavailable"),
                ("read", "constraints"),
            ],
            "the metric is the operator's view; the response tells the caller nothing"
        );
    }

    #[tokio::test]
    async fn a_probe_and_the_enforcement_point_refuse_alike_and_only_the_latter_counts() {
        let ctx = context_for(Uuid::new_v4());
        let enforcers = [
            ("deny", denying_enforcer()),
            ("unreachable", unreachable_enforcer()),
            ("uncompilable", uncompilable_enforcer()),
        ];
        for (kind, enforcer) in &enforcers {
            let probed = probe_scope(enforcer, &ctx, &resource::VALUE, Operation::Read, None)
                .await
                .expect_err("the probe refuses");
            let metrics = RecordingSecurityMetrics::default();
            let counted = refused(enforcer, &metrics, Operation::Read).await;
            assert!(
                matches!(probed, DomainError::Unauthorized { .. }),
                "{kind}: {probed:?}"
            );
            // Same variant, same message: the probe shares the decision.
            assert_eq!(format!("{probed:?}"), format!("{counted:?}"), "{kind}");
            assert_eq!(probed.to_string(), counted.to_string(), "{kind}");
            // The enforcement point counts the refusal once; nothing else moves.
            assert_eq!(
                metrics.denials().len(),
                1,
                "{kind}: {:?}",
                metrics.denials()
            );
            assert!(metrics.secret_uses().is_empty(), "{kind}");
            assert!(metrics.audit_events().is_empty(), "{kind}");
        }
    }

    #[tokio::test]
    async fn the_unmasked_entitlement_is_a_probe_answered_by_masking() {
        // Refused three ways, the answer is the same `false` — mask — and
        // there is no metrics port to count on: the entitlement never
        // reaches the enforcement point.
        let ctx = context_for(Uuid::new_v4());
        for (kind, enforcer) in [
            ("deny", denying_enforcer()),
            ("unreachable", unreachable_enforcer()),
            ("uncompilable", uncompilable_enforcer()),
        ] {
            assert!(!may_read_unmasked(&enforcer, &ctx).await, "{kind}");
        }
    }
}
