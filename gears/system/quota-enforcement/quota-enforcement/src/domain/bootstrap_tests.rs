#![allow(clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverApi;
use gts::GtsTypeId;
use quota_enforcement_sdk::testing::{InMemoryStorage, RecordingSink, quota_draft};
use quota_enforcement_sdk::{
    CONTRACT_MAJOR, MetricId, PageRequest, PolicyDraft, PolicyId, PolicySchemaSnapshot,
    PolicyScope, QuotaEnforcementStoragePluginV1, SCOPE_TENANT, SCOPE_USER, StorageError,
    SubjectRef, SubjectScope,
};
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_security::AccessScope;

use super::{Bootstrap, CatalogBinding};
use crate::domain::catalog::CatalogConfig;
use crate::domain::error::{Dependency, DomainError, PluginKind};
use crate::domain::plugins::PluginBinding;
use crate::domain::ports::contracts::ContractRegistry;
use crate::domain::ports::coordination::SingletonScope;
use crate::domain::ports::metric_registry::MetricRegistry;
use crate::domain::ports::metrics::EngineLabel;
use crate::domain::ports::metrics::{ValidationReason, ValidationSurface};
use crate::domain::readiness::{Readiness, ReadinessState};
use crate::infra::pdp_probe::PdpReachability;
use crate::infra::types_registry::TypesRegistryContracts;
use crate::test_support::{
    DenyAllPdp, FailingPdp, FakeContractRegistry, FakeMetricRegistry, LLM_MODEL_RESOURCE,
    LLM_TENANT_PROJECTION, LLM_TOKEN_CONSTRAINT, LLM_TOKEN_REQUEST, LLM_USER_PROJECTION,
    METRIC_OTHER, METRIC_TOKENS, PermitTenantsPdp, RecordingMetrics, StaticCoordinatorBinding, ctx,
    hub_with, idle_work, in_process_registry, llm_gateway_documents, metric_base_documents,
    register_sink, register_storage, sink_instance, storage_instance, tenant,
};

fn type_id(raw: &str) -> GtsTypeId {
    GtsTypeId::try_new(raw).expect("type id")
}

fn llm_config() -> CatalogConfig {
    CatalogConfig {
        subject_projections: vec![type_id(LLM_USER_PROJECTION), type_id(LLM_TENANT_PROJECTION)],
        resource_projections: vec![type_id(LLM_MODEL_RESOURCE)],
    }
}

struct Harness {
    hub: Arc<ClientHub>,
    storage: Arc<InMemoryStorage>,
    coordinator: Arc<StaticCoordinatorBinding>,
    pdp: Arc<dyn AuthZResolverApi>,
    registry: Arc<dyn ContractRegistry>,
    metric_registry: Arc<FakeMetricRegistry>,
    config: CatalogConfig,
    metrics: Arc<RecordingMetrics>,
    readiness: Arc<Readiness>,
}

fn permitting_pdp() -> Arc<PermitTenantsPdp> {
    Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))
}

/// A harness over the `llm_gateway` fake registry with both projections configured.
fn harness(
    storage: Arc<InMemoryStorage>,
    register_client: bool,
    pdp: Arc<dyn AuthZResolverApi>,
) -> Harness {
    harness_with_registry(
        storage,
        register_client,
        pdp,
        Arc::new(FakeContractRegistry::llm_gateway()),
    )
}

fn harness_with_registry(
    storage: Arc<InMemoryStorage>,
    register_client: bool,
    pdp: Arc<dyn AuthZResolverApi>,
    registry: Arc<dyn ContractRegistry>,
) -> Harness {
    let storage_fixture = storage_instance("cf.core._.qe_db_storage.v1", "acme", 100);
    let hub = hub_with(&[&storage_fixture]);
    if register_client {
        register_storage(&hub, &storage_fixture, storage.clone());
    }
    Harness {
        hub,
        storage,
        coordinator: StaticCoordinatorBinding::ok(),
        pdp,
        registry,
        metric_registry: Arc::new(FakeMetricRegistry::classified()),
        config: llm_config(),
        metrics: Arc::new(RecordingMetrics::default()),
        readiness: Arc::new(Readiness::new()),
    }
}

fn bootstrap(h: &Harness) -> Bootstrap {
    Bootstrap::new(
        PluginBinding::new(h.hub.clone(), "acme".to_owned()),
        h.coordinator.clone(),
        Arc::new(PdpReachability::new(h.pdp.clone())),
        CatalogBinding {
            registry: h.registry.clone(),
            config: h.config.clone(),
        },
        h.metric_registry.clone() as Arc<dyn MetricRegistry>,
        super::BootstrapReporting {
            metrics: h.metrics.clone(),
            readiness: h.readiness.clone(),
        },
        crate::test_support::policy_limits(),
    )
}

fn failed_on(h: &Harness, dependency: Dependency) {
    match h.readiness.snapshot() {
        ReadinessState::Failed {
            dependency: got, ..
        } => assert_eq!(got, dependency),
        other => panic!("expected a {dependency} failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_complete_environment_bootstraps_resolves_the_coordinator_and_becomes_ready() {
    let pdp = permitting_pdp();
    let fake = Arc::new(FakeContractRegistry::llm_gateway());
    let h = harness_with_registry(
        Arc::new(InMemoryStorage::new()),
        true,
        pdp.clone(),
        fake.clone(),
    );
    let bound = bootstrap(&h).run().await.expect("bootstrap succeeds");

    assert!(h.readiness.is_ready());
    assert_eq!(pdp.calls(), 1, "the PDP probe made one round trip");
    assert_eq!(h.storage.bootstrap_calls(), 1);
    assert_eq!(
        h.storage.seeded_defaults().map(|d| d.max_active_leases),
        Some(1000),
        "the foundation bundle seeded the PRD defaults"
    );
    assert_eq!(
        h.storage.bootstrapped_bundle().map(|b| b.contract_major),
        Some(CONTRACT_MAJOR)
    );
    assert_eq!(
        h.coordinator.calls(),
        1,
        "the cluster binding resolved once"
    );

    // The QE-owned definitions were asserted, and the catalogue is published.
    assert_eq!(fake.ensure_registered_calls(), 1);
    let registered = fake.last_registered();
    assert_eq!(registered.len(), 7);
    assert!(registered.contains(&SCOPE_USER) && registered.contains(&SCOPE_TENANT));
    let tokens = MetricId::parse(METRIC_TOKENS).expect("metric");
    assert_eq!(
        bound
            .catalog
            .map_subject(&tokens, &SubjectScope::user())
            .expect("mapped")
            .as_ref(),
        LLM_USER_PROJECTION
    );
    assert_eq!(
        bound
            .catalog
            .request_contract(&tokens)
            .map(|c| c.type_id.as_ref()),
        Some(LLM_TOKEN_REQUEST)
    );
    assert!(h.metrics.contract_failures().is_empty());

    let shutdown = CancellationToken::new();
    shutdown.cancel();
    bound
        .coordinator
        .run_while_leader(SingletonScope::LeaseSweeper, shutdown, idle_work())
        .await
        .expect("the bound coordinator is usable");
}

#[tokio::test]
async fn a_schema_mismatch_fails_bootstrap_on_the_storage_dependency() {
    let h = harness(
        Arc::new(InMemoryStorage::with_installed_schema_major(
            CONTRACT_MAJOR + 1,
        )),
        true,
        permitting_pdp(),
    );
    let err = bootstrap(&h).run().await.err().expect("mismatch");
    assert_eq!(
        err,
        DomainError::SchemaVersionMismatch {
            installed: CONTRACT_MAJOR + 1,
            expected: CONTRACT_MAJOR,
        }
    );
    failed_on(&h, Dependency::Storage);
    assert_eq!(h.coordinator.calls(), 0, "later steps never run");
}

#[tokio::test]
async fn a_missing_storage_client_fails_bootstrap_before_the_cluster_resolve() {
    let h = harness(Arc::new(InMemoryStorage::new()), false, permitting_pdp());
    let err = bootstrap(&h)
        .run()
        .await
        .err()
        .expect("client not registered");
    assert!(
        matches!(err, DomainError::PluginClientNotRegistered { .. }),
        "{err:?}"
    );
    failed_on(&h, Dependency::Storage);
    assert_eq!(h.storage.bootstrap_calls(), 0);
    assert_eq!(h.coordinator.calls(), 0);
}

#[tokio::test]
async fn a_failing_cluster_resolve_fails_bootstrap_on_the_cluster_dependency() {
    let mut h = harness(Arc::new(InMemoryStorage::new()), true, permitting_pdp());
    h.coordinator = StaticCoordinatorBinding::failing(DomainError::ClusterUnavailable(
        "no backend bound for profile `quota-enforcement`".to_owned(),
    ));
    let err = bootstrap(&h).run().await.err().expect("resolve fails");
    assert!(matches!(err, DomainError::ClusterUnavailable(_)), "{err:?}");
    match h.readiness.snapshot() {
        ReadinessState::Failed { dependency, reason } => {
            assert_eq!(dependency, Dependency::Cluster);
            assert!(reason.contains("quota-enforcement"), "{reason}");
        }
        other => panic!("expected a cluster failure, got {other:?}"),
    }
    assert_eq!(
        h.storage.bootstrap_calls(),
        1,
        "storage bootstrap ran before the cluster resolve"
    );
}

#[tokio::test]
async fn a_storage_backend_outage_at_bootstrap_is_unavailability() {
    let storage = Arc::new(InMemoryStorage::new());
    storage.fail_with(StorageError::Unavailable("db down".into()));
    let h = harness(storage, true, permitting_pdp());
    let err = bootstrap(&h).run().await.err().expect("storage down");
    assert_eq!(err, DomainError::BackendUnavailable("db down".to_owned()));
}

#[tokio::test]
async fn a_registered_but_failing_pdp_fails_bootstrap_after_the_cluster_resolve() {
    // The client exists (init accepted it); the PDP behind it does not answer.
    // Registration alone must not make the gear ready.
    let h = harness(Arc::new(InMemoryStorage::new()), true, Arc::new(FailingPdp));
    let err = bootstrap(&h).run().await.err().expect("PDP down");
    assert!(matches!(err, DomainError::PdpUnavailable(_)), "{err:?}");
    failed_on(&h, Dependency::Pdp);
    assert_eq!(h.coordinator.calls(), 1, "the cluster resolve completed");
}

#[tokio::test]
async fn a_denying_pdp_is_reachable_and_bootstrap_completes() {
    // The probe principal has no tenant, so a real PDP denies it. The probe
    // measures reachability, not permission.
    let h = harness(Arc::new(InMemoryStorage::new()), true, Arc::new(DenyAllPdp));
    bootstrap(&h).run().await.expect("a denial is an answer");
    assert!(h.readiness.is_ready());
}

#[tokio::test]
async fn an_unreachable_registry_fails_bootstrap_on_the_registry_dependency() {
    let fake = Arc::new(FakeContractRegistry::llm_gateway());
    fake.fail_all();
    let h = harness_with_registry(
        Arc::new(InMemoryStorage::new()),
        true,
        permitting_pdp(),
        fake,
    );
    let err = bootstrap(&h).run().await.err().expect("registry down");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
    failed_on(&h, Dependency::TypesRegistry);
    assert_eq!(h.storage.bootstrap_calls(), 1, "storage came first");
    assert_eq!(
        h.coordinator.calls(),
        0,
        "the catalogue precedes the cluster resolve"
    );
}

#[tokio::test]
async fn a_conflicting_owned_definition_fails_bootstrap_on_the_catalogue_dependency() {
    let fake = Arc::new(FakeContractRegistry::llm_gateway());
    fake.fail_registration_with(DomainError::CatalogInvalid {
        reason: ValidationReason::DefinitionConflict,
        subject: "gts.cf.core.qe.scope.v1~".to_owned(),
    });
    let h = harness_with_registry(
        Arc::new(InMemoryStorage::new()),
        true,
        permitting_pdp(),
        fake,
    );
    let err = bootstrap(&h).run().await.err().expect("conflict");
    assert!(
        matches!(
            err,
            DomainError::CatalogInvalid {
                reason: ValidationReason::DefinitionConflict,
                ..
            }
        ),
        "{err:?}"
    );
    failed_on(&h, Dependency::Catalog);
    assert!(!h.readiness.is_ready());
}

#[tokio::test]
async fn an_inconsistent_catalogue_fails_bootstrap_and_never_marks_ready() {
    let fake = Arc::new(FakeContractRegistry::llm_gateway());
    fake.remove_type(LLM_TOKEN_CONSTRAINT);
    let h = harness_with_registry(
        Arc::new(InMemoryStorage::new()),
        true,
        permitting_pdp(),
        fake,
    );
    let err = bootstrap(&h).run().await.err().expect("inconsistent");
    assert!(
        matches!(
            err,
            DomainError::CatalogInvalid {
                reason: ValidationReason::ConstraintInvalid,
                ..
            }
        ),
        "{err:?}"
    );
    failed_on(&h, Dependency::Catalog);
    assert_eq!(
        h.metrics.contract_failures(),
        vec![(
            ValidationSurface::Bootstrap,
            ValidationReason::ConstraintInvalid
        )]
    );
    assert_eq!(h.coordinator.calls(), 0);
}

#[tokio::test]
async fn an_active_quota_the_catalogue_no_longer_admits_fails_bootstrap() {
    let storage = Arc::new(InMemoryStorage::new());
    let mut stranded = quota_draft(
        SubjectRef {
            projection_type: type_id(LLM_USER_PROJECTION),
            subject_id: "u-1".to_owned(),
        },
        Some(5),
    );
    stranded.metric = MetricId::parse(METRIC_OTHER).expect("metric");
    storage
        .create_quota(&ctx(), &AccessScope::allow_all(), stranded, &[])
        .await
        .expect("seeded");
    let h = harness(storage, true, permitting_pdp());
    let err = bootstrap(&h).run().await.err().expect("stranded Quota");
    assert!(
        matches!(
            err,
            DomainError::CatalogInvalid {
                reason: ValidationReason::IncompatibleState,
                ..
            }
        ),
        "{err:?}"
    );
    failed_on(&h, Dependency::Catalog);
    assert_eq!(h.coordinator.calls(), 0);
}

#[tokio::test]
async fn a_compatible_active_quota_passes_the_compatibility_check() {
    let storage = Arc::new(InMemoryStorage::new());
    let mut bound_quota = quota_draft(
        SubjectRef {
            projection_type: type_id(LLM_USER_PROJECTION),
            subject_id: "u-1".to_owned(),
        },
        Some(5),
    );
    bound_quota.metric = MetricId::parse(METRIC_TOKENS).expect("metric");
    storage
        .create_quota(&ctx(), &AccessScope::allow_all(), bound_quota, &[])
        .await
        .expect("seeded");
    let h = harness(storage, true, permitting_pdp());
    bootstrap(&h)
        .run()
        .await
        .expect("the catalogue admits the binding");
    assert!(h.readiness.is_ready());
}

#[tokio::test]
async fn an_active_quota_on_a_removed_metric_is_flagged_and_bootstrap_completes() {
    let storage = Arc::new(InMemoryStorage::new());
    let mut stranded = quota_draft(
        SubjectRef {
            projection_type: type_id(LLM_USER_PROJECTION),
            subject_id: "u-1".to_owned(),
        },
        Some(5),
    );
    stranded.metric = MetricId::parse(METRIC_TOKENS).expect("metric");
    storage
        .create_quota(&ctx(), &AccessScope::allow_all(), stranded, &[])
        .await
        .expect("seeded");
    let h = harness(storage, true, permitting_pdp());
    h.metric_registry.remove(METRIC_TOKENS);
    bootstrap(&h)
        .run()
        .await
        .expect("a removed metric is flagged, never fatal");
    assert!(h.readiness.is_ready());
    assert_eq!(
        h.metric_registry.calls(),
        2,
        "each distinct metric is looked up once: the one the catalogue admits, \
         and the removed one an active Quota is still bound to"
    );
}

#[tokio::test]
async fn a_registry_that_does_not_answer_the_metric_scan_fails_on_the_registry_dependency() {
    let storage = Arc::new(InMemoryStorage::new());
    let mut bound_quota = quota_draft(
        SubjectRef {
            projection_type: type_id(LLM_USER_PROJECTION),
            subject_id: "u-1".to_owned(),
        },
        Some(5),
    );
    bound_quota.metric = MetricId::parse(METRIC_TOKENS).expect("metric");
    storage
        .create_quota(&ctx(), &AccessScope::allow_all(), bound_quota, &[])
        .await
        .expect("seeded");
    let h = harness(storage, true, permitting_pdp());
    h.metric_registry.fail_all();
    let err = bootstrap(&h).run().await.err().expect("registry outage");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
    failed_on(&h, Dependency::TypesRegistry);
    assert_eq!(
        h.coordinator.calls(),
        0,
        "the cluster resolve never ran after the failed scan"
    );
}

/// The mandatory real-registry test: registration, discovery, resolution,
/// compilation, and an idempotent restart, with nothing mocked between the
/// gear and `types-registry`.
#[tokio::test]
async fn the_real_registry_bootstraps_registers_discovers_compiles_and_restarts_idempotently() {
    let mut extra = llm_gateway_documents();
    extra.extend(metric_base_documents());
    let client = in_process_registry(extra);
    let registry: Arc<dyn ContractRegistry> = Arc::new(TypesRegistryContracts::new(client.clone()));

    let first = harness_with_registry(
        Arc::new(InMemoryStorage::new()),
        true,
        permitting_pdp(),
        registry.clone(),
    );
    let bound = bootstrap(&first).run().await.expect("first bootstrap");
    assert!(first.readiness.is_ready());
    let tokens = MetricId::parse(METRIC_TOKENS).expect("metric");
    assert_eq!(
        bound
            .catalog
            .map_subject(&tokens, &SubjectScope::tenant())
            .expect("discovered and mapped")
            .as_ref(),
        LLM_TENANT_PROJECTION
    );
    let request = bound
        .catalog
        .request_contract(&tokens)
        .expect("discovered from the registry listing");
    assert_eq!(request.type_id.as_ref(), LLM_TOKEN_REQUEST);
    request
        .contract
        .validate(&serde_json::json!({ "type": LLM_TOKEN_REQUEST, "metadata": { "region": "eu" } }))
        .expect("compiled from the resolved registry content");

    // A restart against the same registry re-asserts the QE definitions
    // (byte-identical: a silent success) and publishes an equivalent catalogue.
    let second = harness_with_registry(
        Arc::new(InMemoryStorage::new()),
        true,
        permitting_pdp(),
        registry,
    );
    let again = bootstrap(&second).run().await.expect("second bootstrap");
    assert!(second.readiness.is_ready());
    assert_eq!(
        again
            .catalog
            .map_subject(&tokens, &SubjectScope::user())
            .expect("mapped")
            .as_ref(),
        LLM_USER_PROJECTION
    );
    assert!(first.metrics.contract_failures().is_empty());
    assert!(second.metrics.contract_failures().is_empty());
}

/// A policy an operator persisted before this deployment was built.
fn persisted_policy(engine_id: &str, engine_config: serde_json::Value) -> PolicyDraft {
    PolicyDraft {
        schema_snapshot: PolicySchemaSnapshot::default(),
        scope: PolicyScope::Metric {
            metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
        },
        engine_id: engine_id.to_owned(),
        engine_config,
        timeout_ms: None,
        description: None,
        comment: None,
        created_by: "operator".to_owned(),
    }
}

#[tokio::test]
async fn bootstrap_registers_both_engines_seeds_the_global_policy_and_publishes_its_artifact() {
    let h = harness(Arc::new(InMemoryStorage::new()), true, permitting_pdp());
    let first = bootstrap(&h);
    let bound = first.run().await.expect("bootstrap succeeds");
    assert_eq!(
        bound.engines.ids().collect::<Vec<_>>(),
        vec!["cel", "most-restrictive-wins"]
    );
    let seeded = h
        .storage
        .read_policy(&PolicyScope::Global)
        .await
        .expect("read")
        .expect("seeded after registration");
    assert_eq!(
        (seeded.version, seeded.engine_id.as_str()),
        (1, "most-restrictive-wins")
    );
    assert_eq!(seeded.engine_config, serde_json::json!({}));
    assert!(
        bound.artifacts.get(&PolicyId::global(), 1).is_some(),
        "the active policy's artifact is rebuilt from its persisted config"
    );
    assert!(h.metrics.engine_bootstrap_failures.lock().is_empty());

    // A restart's bootstrap finds the scope occupied and seeds nothing new.
    first.delivery().stop().await;
    let restarted = Arc::new(h.storage.restarted());
    let again = harness(restarted.clone(), true, permitting_pdp());
    bootstrap(&again).run().await.expect("repeat");
    let versions = restarted
        .list_policy_versions(&PolicyId::global(), PageRequest::first(10))
        .await
        .expect("history");
    assert_eq!(versions.items.len(), 1, "seeded exactly once");
}

#[tokio::test]
async fn an_active_policy_naming_an_unregistered_engine_fails_readiness_on_engine() {
    let storage = Arc::new(InMemoryStorage::new());
    storage
        .create_policy(
            &ctx(),
            persisted_policy("starlark", serde_json::json!({})),
            &[],
        )
        .await
        .expect("persisted");
    let h = harness(storage, true, permitting_pdp());
    let err = bootstrap(&h)
        .run()
        .await
        .err()
        .expect("unsupported active policy");
    assert!(
        matches!(
            err,
            DomainError::InvalidPolicy {
                reason: "UNKNOWN_ENGINE",
                ..
            }
        ),
        "{err:?}"
    );
    failed_on(&h, Dependency::Engine);
    assert!(
        !h.readiness.is_ready(),
        "no silent fallback to most-restrictive-wins"
    );
}

#[tokio::test]
async fn an_active_policy_whose_config_no_longer_compiles_fails_readiness_and_is_counted() {
    let storage = Arc::new(InMemoryStorage::new());
    storage
        .create_policy(
            &ctx(),
            persisted_policy(
                "most-restrictive-wins",
                serde_json::json!({ "weights": [1] }),
            ),
            &[],
        )
        .await
        .expect("persisted");
    let h = harness(storage, true, permitting_pdp());
    assert!(
        bootstrap(&h).run().await.is_err(),
        "config the engine refuses"
    );
    failed_on(&h, Dependency::Engine);
    assert_eq!(
        h.metrics.engine_bootstrap_failures.lock().as_slice(),
        &[EngineLabel::MostRestrictiveWins]
    );
}

/// A harness whose registry also lists `sinks`, each registered as a scoped
/// client unless it is `None`.
fn harness_with_sinks(
    sinks: &[(
        &crate::test_support::PluginFixture,
        Option<Arc<RecordingSink>>,
    )],
) -> Harness {
    let mut h = harness(Arc::new(InMemoryStorage::new()), false, permitting_pdp());
    let storage_fixture = storage_instance("cf.core._.qe_db_storage.v1", "acme", 100);
    let mut fixtures = vec![&storage_fixture];
    fixtures.extend(sinks.iter().map(|(fixture, _)| *fixture));
    h.hub = hub_with(&fixtures);
    register_storage(&h.hub, &storage_fixture, h.storage.clone());
    for (fixture, sink) in sinks {
        if let Some(sink) = sink {
            register_sink(&h.hub, fixture, sink.clone());
        }
    }
    h
}

/// Commit one Quota with one `quota-changed` event into the double's outbox.
async fn commit_one_event(storage: &InMemoryStorage) -> quota_enforcement_sdk::NotificationEvent {
    let event = crate::domain::quotas::events::quota_changed(
        tenant(),
        None,
        None,
        crate::domain::quotas::events::ChangeKind::Created,
        time::OffsetDateTime::now_utc(),
    );
    storage
        .create_quota(
            &ctx(),
            &AccessScope::allow_all(),
            quota_draft(
                SubjectRef {
                    projection_type: type_id(LLM_USER_PROJECTION),
                    subject_id: "u-1".to_owned(),
                },
                Some(10),
            ),
            std::slice::from_ref(&event),
        )
        .await
        .expect("committed");
    event
}

#[tokio::test]
async fn bootstrap_starts_delivery_to_every_sink_of_every_vendor() {
    let acme = sink_instance("cf.core._.qe_sink_acme.v1", "acme");
    let globex = sink_instance("cf.core._.qe_sink_globex.v1", "globex");
    let a = Arc::new(RecordingSink::new("acme-audit"));
    let g = Arc::new(RecordingSink::new("globex-billing"));
    let h = harness_with_sinks(&[(&acme, Some(a.clone())), (&globex, Some(g.clone()))]);
    let bootstrap = bootstrap(&h);
    bootstrap.run().await.expect("bootstrap succeeds");
    assert!(bootstrap.delivery().is_running());

    let event = commit_one_event(&h.storage).await;
    let report = h.storage.drain_notifications().await;
    assert_eq!(report.delivered, 1);
    for sink in [&a, &g] {
        let received = sink.received();
        assert_eq!(received.len(), 1, "every vendor's sink receives the event");
        assert_eq!(received[0].1.event_id, event.event_id);
        assert!(!received[0].0.is_anonymous());
    }

    bootstrap.delivery().stop().await;
    assert!(!bootstrap.delivery().is_running());
}

#[tokio::test]
async fn without_sinks_bootstrap_is_ready_and_events_are_acknowledged() {
    let h = harness_with_sinks(&[]);
    let bootstrap = bootstrap(&h);
    bootstrap
        .run()
        .await
        .expect("zero sinks is a valid deployment");
    assert_eq!(h.readiness.snapshot(), ReadinessState::Ready);
    commit_one_event(&h.storage).await;
    assert_eq!(h.storage.drain_notifications().await.delivered, 1);
}

#[tokio::test]
async fn a_still_failing_event_is_delivered_eleven_times_then_dead_lettered() {
    let fixture = sink_instance("cf.core._.qe_sink_busy.v1", "acme");
    let busy = Arc::new(RecordingSink::answering(
        "busy",
        (0..11)
            .map(|_| {
                Err(quota_enforcement_sdk::DispatchError::Transient(
                    "busy".to_owned(),
                ))
            })
            .collect(),
    ));
    let h = harness_with_sinks(&[(&fixture, Some(busy.clone()))]);
    bootstrap(&h).run().await.expect("bootstrap succeeds");
    commit_one_event(&h.storage).await;

    for call in 1..=10 {
        let report = h.storage.drain_notifications().await;
        assert!(report.retry_pending, "call {call} is retried");
    }
    let report = h.storage.drain_notifications().await;
    assert_eq!((report.rejected, report.retry_pending), (1, false));
    assert_eq!(busy.received().len(), 11);
    assert_eq!(h.storage.dead_letters().len(), 1);
    assert_eq!(h.metrics.outbox_rejections(), 1);
    assert_eq!(h.metrics.dispatch_failures().len(), 11);
}

#[tokio::test]
async fn two_sinks_answering_one_id_fail_readiness_on_the_sinks() {
    let first = sink_instance("cf.core._.qe_sink_one.v1", "acme");
    let second = sink_instance("cf.core._.qe_sink_two.v1", "globex");
    let h = harness_with_sinks(&[
        (&first, Some(Arc::new(RecordingSink::new("audit")))),
        (&second, Some(Arc::new(RecordingSink::new("audit")))),
    ]);
    let bootstrap = bootstrap(&h);
    let err = bootstrap.run().await.err().expect("duplicate sink id");
    assert!(
        matches!(
            &err,
            DomainError::InvalidPluginInstance { kind: PluginKind::NotificationSink, gts_id, reason }
                if gts_id == &second.instance_id && reason.contains("audit")
        ),
        "{err:?}"
    );
    failed_on(&h, Dependency::NotificationSinks);
    assert!(!bootstrap.delivery().is_running());
}

#[tokio::test]
async fn a_malformed_sink_instance_fails_readiness_on_the_sinks() {
    let broken = crate::test_support::PluginFixture::malformed_sink("cf.core._.qe_sink_broken.v1");
    let h = harness_with_sinks(&[(&broken, None)]);
    let err = bootstrap(&h).run().await.err().expect("malformed");
    assert!(
        matches!(
            &err,
            DomainError::InvalidPluginInstance { kind: PluginKind::NotificationSink, gts_id, .. }
                if gts_id == &broken.instance_id
        ),
        "{err:?}"
    );
    failed_on(&h, Dependency::NotificationSinks);
}

#[tokio::test]
async fn a_sink_instance_without_its_client_fails_readiness_on_the_sinks() {
    let orphan = sink_instance("cf.core._.qe_sink_orphan.v1", "acme");
    let h = harness_with_sinks(&[(&orphan, None)]);
    let err = bootstrap(&h).run().await.err().expect("client missing");
    assert_eq!(
        err,
        DomainError::PluginClientNotRegistered {
            kind: PluginKind::NotificationSink,
            gts_id: orphan.instance_id.clone(),
        }
    );
    failed_on(&h, Dependency::NotificationSinks);
}

#[tokio::test]
async fn a_storage_that_refuses_delivery_fails_readiness_on_storage() {
    let h = harness_with_sinks(&[]);
    // Delivery already runs on this plugin instance: a second start is refused.
    let _running = h
        .storage
        .start_notification_delivery(Arc::new(
            crate::domain::notifications::NotificationDispatcher::new(
                Vec::new(),
                crate::domain::notifications::dispatcher_context().expect("context"),
                crate::domain::notifications::DispatchLimits::default(),
                h.metrics.clone(),
            ),
        ))
        .await
        .expect("first start");
    let err = bootstrap(&h).run().await.err().expect("second start");
    assert!(matches!(err, DomainError::Internal(_)), "{err:?}");
    failed_on(&h, Dependency::Storage);
}
