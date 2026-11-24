#![allow(clippy::expect_used)]

use std::num::NonZeroUsize;
use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use quota_enforcement_sdk::testing::InMemoryStorage;
use quota_enforcement_sdk::{
    MetricId, PolicyDraft, PolicyId, PolicyPatch, PolicySchemaSnapshot, PolicyScope, PolicySpec,
    PolicyUpdate, PolicyVersionState, QuotaEnforcementStoragePluginV1,
};
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::testing_platform::MockTypesRegistry;

use crate::domain::engines::{PolicyArtifactCache, linked_registry};
use crate::domain::error::DomainError;
use crate::domain::policies::PolicyRuntimeLimits;
use crate::domain::ports::metrics::PolicyTransition;
use crate::domain::{Admission, Bound, Readiness, Service};
use crate::test_support::{
    DenyAllPdp, METRIC_OTHER, METRIC_TOKENS, NoopCoordinator, PermitUnconstrainedPdp,
    RecordingMetrics, actor, caller, catalog_over, ctx, fail_registry_reads, metric_type_document,
    platform_registry, policy_limits, test_limits,
};

const BROKEN_METRIC: &str = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";

struct Harness {
    service: Arc<Service>,
    metrics: Arc<RecordingMetrics>,
    storage: Arc<InMemoryStorage>,
    artifacts: Arc<PolicyArtifactCache>,
    registry: Arc<MockTypesRegistry>,
}

fn harness(pdp: Arc<dyn AuthZResolverApi>) -> Harness {
    harness_with(pdp, policy_limits())
}

fn harness_with(pdp: Arc<dyn AuthZResolverApi>, limits: PolicyRuntimeLimits) -> Harness {
    let metrics = Arc::new(RecordingMetrics::default());
    let service = Arc::new(Service::new(
        Admission::new(PolicyEnforcer::new(pdp), metrics.clone()),
        Arc::new(Readiness::new()),
        test_limits(),
        limits,
        crate::domain::service::OperationsRuntime {
            cache_entries: 16,
            cache_ttl: std::time::Duration::from_secs(5),
            preparation_max_attempts: std::num::NonZeroU32::new(3).expect("attempts"),
            leases: crate::domain::operations::LeaseLimits::default(),
            batch: crate::domain::operations::BatchLimits::default(),
            snapshot: crate::domain::operations::SnapshotLimits::default(),
        },
    ));
    let registry = platform_registry();
    let mut broken = metric_type_document(BROKEN_METRIC, json!({ "kind": "counter" }), json!({}));
    broken["x-gts-abstract"] = json!(true);
    registry.seed(BROKEN_METRIC, broken);
    let catalog = catalog_over(&registry, metrics.clone());
    let storage = Arc::new(InMemoryStorage::new());
    let artifacts = Arc::new(PolicyArtifactCache::new(
        NonZeroUsize::new(8).expect("capacity"),
        NonZeroUsize::new(2).expect("permits"),
    ));
    service
        .bind(Bound {
            engines: Arc::new(linked_registry().expect("engines")),
            artifacts: Arc::clone(&artifacts),
            storage: storage.clone(),
            coordinator: Arc::new(NoopCoordinator),
            catalog,
        })
        .expect("bind");
    Harness {
        service,
        metrics,
        storage,
        artifacts,
        registry,
    }
}

fn permitted() -> Arc<dyn AuthZResolverApi> {
    Arc::new(PermitUnconstrainedPdp)
}

fn tokens() -> MetricId {
    MetricId::parse(METRIC_TOKENS).expect("metric")
}

fn other() -> MetricId {
    MetricId::parse(METRIC_OTHER).expect("metric")
}

fn broken() -> MetricId {
    MetricId::parse(BROKEN_METRIC).expect("metric")
}

fn mrw(scope: PolicyScope) -> PolicySpec {
    PolicySpec {
        scope,
        engine_id: "most-restrictive-wins".to_owned(),
        engine_config: json!({}),
        timeout_ms: None,
        description: None,
        comment: Some("v1".to_owned()),
    }
}

fn patch(if_match_version: u32) -> PolicyPatch {
    PolicyPatch {
        if_match_version,
        engine_id: None,
        engine_config: None,
        timeout_ms: Some(3),
        comment: Some("bump".to_owned()),
    }
}

fn transitions(h: &Harness) -> Vec<PolicyTransition> {
    h.metrics.policy_transitions.lock().clone()
}

#[tokio::test]
async fn create_validates_with_the_engine_persists_the_actor_and_publishes_after_the_write() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");

    let created = policies
        .create(mrw(PolicyScope::Metric { metric: tokens() }))
        .await
        .expect("create");
    assert_eq!(created.version, 1);
    assert_eq!(created.state, PolicyVersionState::Active);
    assert_eq!(
        created.created_by,
        ctx().subject_id().to_string(),
        "actor identity comes from the security context, never the caller"
    );
    assert!(
        created.schema_snapshot.schemas.is_empty(),
        "most-restrictive-wins reads no metadata and persists an empty snapshot"
    );
    assert!(
        h.artifacts.get(&created.policy_id, 1).is_some(),
        "the compiled artifact is published once the write committed"
    );
    assert_eq!(transitions(&h), vec![PolicyTransition::Create]);

    // A second live policy at the same exact scope is refused by storage.
    let occupied = policies
        .create(mrw(PolicyScope::Metric { metric: tokens() }))
        .await
        .expect_err("occupied");
    assert!(matches!(occupied, DomainError::PolicyScopeOccupied { .. }));
    assert_eq!(transitions(&h).len(), 1, "a refused create counts nothing");
}

#[tokio::test]
async fn an_unknown_engine_or_a_bad_config_is_refused_before_anything_is_stored() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");

    let unknown = policies
        .create(PolicySpec {
            engine_id: "starlark".to_owned(),
            ..mrw(PolicyScope::Global)
        })
        .await
        .expect_err("unknown engine");
    match unknown {
        DomainError::InvalidPolicy { reason, detail, .. } => {
            assert_eq!(reason, "UNKNOWN_ENGINE");
            assert!(
                detail.contains("most-restrictive-wins") && detail.contains("cel"),
                "the rejection names the registered engines: {detail}"
            );
        }
        other => panic!("expected UNKNOWN_ENGINE, got {other:?}"),
    }

    let bad_config = policies
        .create(PolicySpec {
            engine_config: json!({ "weights": [1, 2] }),
            ..mrw(PolicyScope::Global)
        })
        .await
        .expect_err("non-empty MRW config");
    assert!(
        matches!(
            bad_config,
            DomainError::InvalidPolicy {
                reason: "INVALID_ENGINE_CONFIG",
                ..
            }
        ),
        "{bad_config:?}"
    );

    let unregistered = policies
        .create(mrw(PolicyScope::Metric { metric: other() }))
        .await
        .expect_err("unregistered metric type");
    assert!(
        matches!(unregistered, DomainError::MetricNotRegistered { .. }),
        "{unregistered:?}"
    );
    assert!(matches!(
        CanonicalError::from(unregistered),
        CanonicalError::InvalidArgument { .. }
    ));

    let unavailable = policies
        .create(mrw(PolicyScope::Metric { metric: broken() }))
        .await
        .expect_err("no served entry");
    assert!(
        matches!(unavailable, DomainError::ProjectionNotResolvable { .. }),
        "{unavailable:?}"
    );
    assert!(matches!(
        CanonicalError::from(unavailable),
        CanonicalError::FailedPrecondition { .. }
    ));

    assert!(
        h.storage
            .read_policy(&PolicyScope::Global)
            .await
            .expect("read")
            .is_none(),
        "nothing reached storage"
    );
    assert!(transitions(&h).is_empty());
}

#[tokio::test]
async fn a_cel_policy_is_type_checked_against_the_metric_type_and_keeps_its_schema() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    let expr = r#"{ "debit_plan": quotas
        .filter(q, request.region == "eu")
        .map(q, [{ "id": q.id, "amount": amount }]) }"#;
    let created = policies
        .create(PolicySpec {
            engine_id: "cel".to_owned(),
            engine_config: json!({ "expr": expr }),
            ..mrw(PolicyScope::Metric { metric: tokens() })
        })
        .await
        .expect("cel policy");
    assert_eq!(
        created.schema_snapshot.schemas.keys().collect::<Vec<_>>(),
        vec![METRIC_TOKENS],
        "the version keeps the metric type schema it was checked against, and nothing else"
    );
    assert_eq!(
        created.schema_snapshot.inputs,
        quota_enforcement_sdk::EnvironmentInputs { request: true }
    );
    assert!(h.artifacts.get(&created.policy_id, 1).is_some());

    let err = policies
        .create(PolicySpec {
            engine_id: "cel".to_owned(),
            engine_config: json!({ "expr": r#"{ "deny": { "reason": request.zone } }"# }),
            ..mrw(PolicyScope::Metric { metric: tokens() })
        })
        .await
        .expect_err("a property the metric type does not declare");
    match err {
        DomainError::InvalidPolicy { reason, detail, .. } => {
            assert_eq!(reason, "INVALID_ENGINE_CONFIG");
            assert!(detail.contains("absent"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_policy_update_for_a_metric_with_no_served_entry_is_refused() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    let id = h
        .storage
        .create_policy(
            &actor(),
            PolicyDraft {
                scope: PolicyScope::Metric { metric: broken() },
                engine_id: "most-restrictive-wins".to_owned(),
                engine_config: json!({}),
                ..stored_cel_draft()
            },
            &[],
        )
        .await
        .expect("v1")
        .policy_id;
    let err = policies
        .update(id, patch(1))
        .await
        .expect_err("no served entry");
    assert!(
        matches!(err, DomainError::ProjectionNotResolvable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_registry_that_does_not_answer_fails_a_policy_write_for_a_metric_not_held() {
    let h = harness(permitted());
    fail_registry_reads(&h.registry);
    let err = h
        .service
        .policies(caller())
        .expect("bound")
        .create(mrw(PolicyScope::Metric { metric: tokens() }))
        .await
        .expect_err("registry down");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
    assert!(matches!(
        CanonicalError::from(err),
        CanonicalError::ServiceUnavailable { .. }
    ));
}

#[tokio::test]
async fn rollback_and_delete_of_a_policy_for_an_unavailable_metric_run() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    let id = h
        .storage
        .create_policy(
            &actor(),
            PolicyDraft {
                scope: PolicyScope::Metric { metric: broken() },
                ..stored_cel_draft()
            },
            &[],
        )
        .await
        .expect("v1")
        .policy_id;
    h.storage
        .update_policy(&actor(), id.clone(), stored_cel_update(1), &[])
        .await
        .expect("v2");
    fail_registry_reads(&h.registry);

    let back = policies
        .rollback(id.clone(), 1, None)
        .await
        .expect("a rollback makes no catalogue check");
    assert_eq!(back.version, 1);
    policies
        .delete(id, None)
        .await
        .expect("a delete makes no catalogue check");
}

#[tokio::test]
async fn update_is_conditional_and_a_stale_version_counts_a_conflict_not_a_transition() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    let created = policies
        .create(mrw(PolicyScope::Global))
        .await
        .expect("create");
    let id = created.policy_id.clone();

    let stale = policies
        .update(id.clone(), patch(7))
        .await
        .expect_err("stale");
    assert!(matches!(
        stale,
        DomainError::VersionConflict {
            expected: 7,
            actual: 1
        }
    ));
    assert_eq!(*h.metrics.policy_conflicts.lock(), 1);
    assert_eq!(transitions(&h), vec![PolicyTransition::Create]);
    assert!(
        h.artifacts.get(&id, 2).is_none(),
        "a refused write publishes nothing to the artifact cache"
    );

    let updated = policies.update(id.clone(), patch(1)).await.expect("update");
    assert_eq!(updated.version, 2);
    assert_eq!(
        updated.timeout_ms,
        Some(3),
        "the requested timeout is stored, not a clamped one"
    );
    assert!(h.artifacts.get(&id, 2).is_some());
    assert_eq!(
        transitions(&h),
        vec![PolicyTransition::Create, PolicyTransition::Update]
    );
    assert_eq!(policies.read(&id, None).await.expect("active").version, 2);
}

#[tokio::test]
async fn rollback_and_delete_count_only_the_transition_that_actually_moved_state() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    let created = policies
        .create(mrw(PolicyScope::Metric { metric: tokens() }))
        .await
        .expect("create");
    let id = created.policy_id.clone();
    policies.update(id.clone(), patch(1)).await.expect("v2");

    let back = policies
        .rollback(id.clone(), 1, Some("undo".to_owned()))
        .await
        .expect("rollback");
    assert_eq!((back.version, back.state), (1, PolicyVersionState::Active));
    let replay = policies
        .rollback(id.clone(), 1, Some("retry".to_owned()))
        .await
        .expect("replay is a no-op");
    assert_eq!(replay.version, 1);
    assert_eq!(
        transitions(&h),
        vec![
            PolicyTransition::Create,
            PolicyTransition::Update,
            PolicyTransition::Rollback
        ],
        "the replay counted nothing"
    );
    assert!(matches!(
        policies.rollback(id.clone(), 2, None).await,
        Err(DomainError::VersionRolledBack { version: 2, .. })
    ));
    assert!(matches!(
        policies.rollback(id.clone(), 9, None).await,
        Err(DomainError::UnknownPolicyVersion { version: 9, .. })
    ));

    policies.delete(id.clone(), None).await.expect("delete");
    policies
        .delete(id.clone(), None)
        .await
        .expect("repeated delete is a no-op");
    assert_eq!(transitions(&h).last(), Some(&PolicyTransition::Delete));
    assert_eq!(transitions(&h).len(), 4);
    assert!(matches!(
        policies.read(&id, None).await,
        Err(DomainError::PolicyDeleted { .. })
    ));
    assert_eq!(
        policies.read(&id, Some(2)).await.expect("retained").state,
        PolicyVersionState::RolledBack
    );
    assert!(matches!(
        policies
            .read(&PolicyId::new("never-created".to_owned()), None)
            .await,
        Err(DomainError::PolicyNotFound { .. })
    ));
}

#[tokio::test]
async fn the_global_policy_cannot_be_deleted_and_history_pages_are_bounded() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    policies
        .create(mrw(PolicyScope::Global))
        .await
        .expect("global");
    assert!(matches!(
        policies.delete(PolicyId::global(), None).await,
        Err(DomainError::CannotDeleteSeededGlobalPolicy)
    ));

    let page = policies
        .list(&PolicyId::global(), Some(10), None)
        .await
        .expect("history");
    assert_eq!(page.items.len(), 1);
    for limit in [0, policy_limits().authoring.list_limit + 1] {
        assert!(
            matches!(
                policies.list(&PolicyId::global(), Some(limit), None).await,
                Err(DomainError::InvalidPolicy {
                    reason: "INVALID_PAGE",
                    ..
                })
            ),
            "limit {limit} is outside the configured bound"
        );
    }
}

fn stored_cel_draft() -> PolicyDraft {
    PolicyDraft {
        schema_snapshot: PolicySchemaSnapshot::default(),
        scope: PolicyScope::Metric { metric: tokens() },
        engine_id: "cel".to_owned(),
        engine_config: json!({ "expr": r#"{ "deny": { "reason": "none" } }"# }),
        timeout_ms: None,
        description: None,
        comment: None,
        created_by: "operator".to_owned(),
    }
}

fn stored_cel_update(if_match_version: u32) -> PolicyUpdate {
    PolicyUpdate {
        schema_snapshot: Some(PolicySchemaSnapshot::default()),
        if_match_version,
        engine_id: None,
        engine_config: None,
        timeout_ms: None,
        comment: None,
        created_by: "operator".to_owned(),
    }
}

#[tokio::test]
async fn a_terminal_rollback_target_is_reported_in_storage_order() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");

    let deleted = h
        .storage
        .create_policy(&actor(), stored_cel_draft(), &[])
        .await
        .expect("v1")
        .policy_id;
    h.storage
        .update_policy(&actor(), deleted.clone(), stored_cel_update(1), &[])
        .await
        .expect("v2");
    h.storage
        .delete_policy(&actor(), deleted.clone(), None, &[])
        .await
        .expect("delete");
    let err = policies.rollback(deleted, 1, None).await;
    assert!(
        matches!(err, Err(DomainError::PolicyDeleted { .. })),
        "{err:?}"
    );

    let rolled = h
        .storage
        .create_policy(
            &actor(),
            PolicyDraft {
                scope: PolicyScope::Metric { metric: other() },
                ..stored_cel_draft()
            },
            &[],
        )
        .await
        .expect("v1")
        .policy_id;
    h.storage
        .update_policy(&actor(), rolled.clone(), stored_cel_update(1), &[])
        .await
        .expect("v2");
    h.storage
        .rollback_policy(&actor(), rolled.clone(), 1, None, &[])
        .await
        .expect("v2 rolled back");
    let err = policies.rollback(rolled, 2, None).await;
    assert!(
        matches!(err, Err(DomainError::VersionRolledBack { version: 2, .. })),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_deleted_policy_reports_deletion_for_a_rolled_back_or_unknown_target() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    let id = policies
        .create(mrw(PolicyScope::Metric { metric: tokens() }))
        .await
        .expect("v1")
        .policy_id;
    policies.update(id.clone(), patch(1)).await.expect("v2");
    policies
        .rollback(id.clone(), 1, None)
        .await
        .expect("v2 rolled back");
    policies.delete(id.clone(), None).await.expect("delete");
    for target in [2, 9] {
        let err = policies.rollback(id.clone(), target, None).await;
        assert!(
            matches!(err, Err(DomainError::PolicyDeleted { .. })),
            "target {target}: {err:?}"
        );
    }
    let never = PolicyId::new(uuid::Uuid::now_v7().to_string());
    let err = policies.rollback(never, 1, None).await;
    assert!(
        matches!(err, Err(DomainError::PolicyNotFound { .. })),
        "{err:?}"
    );
}

#[tokio::test]
async fn an_omitted_history_limit_is_the_platform_default_within_the_configured_bound() {
    let limits = crate::config::PoliciesSection {
        list_max_limit: 500,
        ..crate::config::PoliciesSection::default()
    }
    .to_limits()
    .expect("limits");
    let h = harness_with(permitted(), limits);
    let policies = h.service.policies(caller()).expect("bound");
    let id = policies
        .create(mrw(PolicyScope::Metric { metric: tokens() }))
        .await
        .expect("v1")
        .policy_id;
    for version in 1..=100 {
        policies
            .update(id.clone(), patch(version))
            .await
            .expect("next version");
    }
    let page = policies.list(&id, None, None).await.expect("page");
    assert_eq!(page.items.len(), 100);
    assert!(page.next_cursor.is_some());
}

#[tokio::test]
async fn a_denied_operator_never_reaches_storage() {
    let h = harness(Arc::new(DenyAllPdp));
    let policies = h.service.policies(caller()).expect("bound");
    let denied = policies
        .create(mrw(PolicyScope::Global))
        .await
        .expect_err("denied");
    assert!(
        matches!(denied, DomainError::PdpDenied { .. }),
        "{denied:?}"
    );
    assert!(
        h.storage
            .read_policy(&PolicyScope::Global)
            .await
            .expect("read")
            .is_none()
    );
    let id = PolicyId::global();
    let denials = [
        policies.read(&id, None).await.err(),
        policies.list(&id, Some(10), None).await.err(),
        policies.update(id.clone(), patch(1)).await.err(),
        policies.rollback(id.clone(), 1, None).await.err(),
        policies.delete(id.clone(), None).await.err(),
    ];
    for denial in denials {
        assert!(
            matches!(denial, Some(DomainError::PdpDenied { .. })),
            "every policy operation is refused at admission: {denial:?}"
        );
    }
    assert!(transitions(&h).is_empty());
}

#[tokio::test]
async fn a_policy_write_waits_for_a_compilation_permit() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    // The harness allows two concurrent compilations; take both.
    let held = [
        h.artifacts.compile_permit().await.expect("permit"),
        h.artifacts.compile_permit().await.expect("permit"),
    ];

    let write = policies.create(mrw(PolicyScope::Global));
    tokio::pin!(write);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut write)
            .await
            .is_err(),
        "validation does not run while every compilation permit is taken"
    );

    drop(held);
    let created = write.await.expect("create once a permit is free");
    assert_eq!(created.version, 1);
}

#[tokio::test]
async fn a_policy_that_reads_no_request_keeps_no_schema_and_rebuilds_from_what_storage_holds() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    let expr = r#"{ "debit_plan": quotas
        .filter(q, q['tier'] == "subject")
        .map(q, [{ "id": q.id, "amount": amount }]) }"#;
    let created = policies
        .create(PolicySpec {
            engine_id: "cel".to_owned(),
            engine_config: json!({ "expr": expr }),
            ..mrw(PolicyScope::Metric { metric: tokens() })
        })
        .await
        .expect("cel policy");
    assert!(!created.schema_snapshot.inputs.request);
    assert!(
        created.schema_snapshot.schemas.is_empty(),
        "a policy that reads no request keeps no metric type schema"
    );

    let stored = h
        .storage
        .read_active_policies()
        .await
        .expect("read")
        .into_iter()
        .find(|version| version.policy_id == created.policy_id)
        .expect("the created policy is active");
    linked_registry()
        .expect("engines")
        .get(&stored.engine_id)
        .expect("cel")
        .validate_config(quota_enforcement_sdk::EngineValidationInput {
            raw: &stored.engine_config,
            schemas: &stored.schema_snapshot,
        })
        .expect("the version rebuilds from what storage holds");
}

#[tokio::test]
async fn an_oversized_config_a_zero_timeout_or_a_long_comment_is_refused_before_any_write() {
    let h = harness(permitted());
    let policies = h.service.policies(caller()).expect("bound");
    let reason = |err: DomainError| match err {
        DomainError::InvalidPolicy { reason, .. } => reason,
        other => panic!("expected an invalid policy, got {other:?}"),
    };

    let mut oversized = mrw(PolicyScope::Metric { metric: tokens() });
    oversized.engine_config = json!({ "padding": "x".repeat(20_000) });
    let mut zero_timeout = mrw(PolicyScope::Metric { metric: tokens() });
    zero_timeout.timeout_ms = Some(0);
    let mut long_comment = mrw(PolicyScope::Metric { metric: tokens() });
    long_comment.comment = Some("c".repeat(2_000));

    for (spec, expected) in [
        (oversized, "POLICY_CONFIG_TOO_LARGE"),
        (zero_timeout, "INVALID_TIMEOUT"),
        (long_comment, "COMMENT_TOO_LONG"),
    ] {
        let err = policies.create(spec).await.expect_err(expected);
        assert_eq!(reason(err), expected);
    }
    assert!(
        transitions(&h).is_empty(),
        "a refused policy reaches neither storage nor the transition metric"
    );
}
