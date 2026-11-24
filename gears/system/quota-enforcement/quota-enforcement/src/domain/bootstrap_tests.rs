#![allow(clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverApi;
use gts::{GtsSchema as _, GtsTypeId};
use quota_enforcement_sdk::testing::{InMemoryStorage, RecordingSink, quota_draft};
use quota_enforcement_sdk::{
    CONTRACT_MAJOR, MetricId, PageRequest, PolicyDraft, PolicyId, PolicySchemaSnapshot,
    PolicyScope, QuotaEnforcementStoragePluginV1, StorageError, SubjectRef,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_security::AccessScope;
use types_registry_sdk::testing_platform::MockTypesRegistry;

use super::Bootstrap;
use crate::domain::catalog::CatalogEntry;
use crate::domain::error::{Dependency, DomainError, PluginKind};
use crate::domain::plugins::PluginBinding;
use crate::domain::ports::coordination::SingletonScope;
use crate::domain::ports::metrics::{CatalogReadPhase, EngineLabel, ValidationReason};
use crate::domain::readiness::{Readiness, ReadinessState};
use crate::infra::pdp_probe::PdpReachability;
use crate::test_support::{
    DenyAllPdp, FailingPdp, METRIC_OTHER, METRIC_REQUESTS, METRIC_TOKENS, PermitTenantsPdp,
    RecordingMetrics, StaticCoordinatorBinding, USER_SUBJECT_TYPE, actor, catalog_over,
    fail_registry_reads, hub_with, idle_work, metric_type_document, platform_registry,
    register_sink, register_storage, registry_reads, sink_instance, storage_instance, tenant,
};

const INVALID_METRIC: &str = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";

struct Harness {
    hub: Arc<ClientHub>,
    storage: Arc<InMemoryStorage>,
    coordinator: Arc<StaticCoordinatorBinding>,
    pdp: Arc<dyn AuthZResolverApi>,
    registry: Arc<MockTypesRegistry>,
    metrics: Arc<RecordingMetrics>,
    readiness: Arc<Readiness>,
}

fn permitting_pdp() -> Arc<PermitTenantsPdp> {
    Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))
}

fn harness(
    storage: Arc<InMemoryStorage>,
    register_client: bool,
    pdp: Arc<dyn AuthZResolverApi>,
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
        registry: platform_registry(),
        metrics: Arc::new(RecordingMetrics::default()),
        readiness: Arc::new(Readiness::new()),
    }
}

fn bootstrap(h: &Harness) -> Bootstrap {
    Bootstrap::new(
        PluginBinding::new(h.hub.clone(), "acme".to_owned()),
        h.coordinator.clone(),
        Arc::new(PdpReachability::new(h.pdp.clone())),
        catalog_over(&h.registry, h.metrics.clone()),
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

fn metric(id: &str) -> MetricId {
    MetricId::parse(id).expect("metric")
}

async fn seed_active_quota(storage: &InMemoryStorage, metric_id: &str) {
    let mut draft = quota_draft(
        SubjectRef {
            subject_type: GtsTypeId::new(USER_SUBJECT_TYPE),
            subject_id: "u-1".to_owned(),
        },
        Some(5),
    );
    draft.metric = metric(metric_id);
    storage
        .create_quota(&actor(), &AccessScope::allow_all(), draft, &[])
        .await
        .expect("seeded");
}

#[tokio::test]
async fn a_complete_environment_bootstraps_resolves_the_coordinator_and_becomes_ready() {
    let pdp = permitting_pdp();
    let h = harness(Arc::new(InMemoryStorage::new()), true, pdp.clone());
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
    assert_eq!(
        registry_reads(&h.registry),
        0,
        "bootstrap reads no metric type"
    );
    assert!(bound.catalog.peek(&metric(METRIC_TOKENS)).is_none());
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
async fn a_wrong_or_unregistered_metric_type_of_active_quotas_and_policies_does_not_fail_bootstrap()
{
    let storage = Arc::new(InMemoryStorage::new());
    seed_active_quota(&storage, INVALID_METRIC).await;
    seed_active_quota(&storage, METRIC_OTHER).await;
    storage
        .create_policy(
            &actor(),
            PolicyDraft {
                scope: PolicyScope::Metric {
                    metric: metric(INVALID_METRIC),
                },
                ..persisted_policy("most-restrictive-wins", json!({}))
            },
            &[],
        )
        .await
        .expect("persisted");
    let h = harness(storage, true, permitting_pdp());
    let mut broken = metric_type_document(INVALID_METRIC, json!({ "kind": "counter" }), json!({}));
    broken["x-gts-abstract"] = json!(true);
    h.registry.seed(INVALID_METRIC, broken);

    let bound = bootstrap(&h)
        .run()
        .await
        .expect("a wrong metric type never fails bootstrap");
    assert_eq!(h.readiness.snapshot(), ReadinessState::Ready);
    assert_eq!(registry_reads(&h.registry), 1, "one batch read at start");
    assert!(matches!(
        bound.catalog.peek(&metric(INVALID_METRIC)),
        Some(CatalogEntry::Unavailable {
            reason: ValidationReason::Abstract
        })
    ));
    assert_eq!(h.metrics.catalog_unavailable(), Some(1));
    assert!(
        bound
            .catalog
            .entry(&metric(METRIC_OTHER))
            .await
            .expect("remembered")
            .is_none()
    );
    assert_eq!(
        registry_reads(&h.registry),
        1,
        "the unregistered metric is remembered from the read at start"
    );
}

#[tokio::test]
async fn the_metric_types_of_active_quotas_and_policies_are_read_at_start() {
    let storage = Arc::new(InMemoryStorage::new());
    seed_active_quota(&storage, METRIC_TOKENS).await;
    storage
        .create_policy(
            &actor(),
            PolicyDraft {
                scope: PolicyScope::Metric {
                    metric: metric(METRIC_REQUESTS),
                },
                ..persisted_policy("most-restrictive-wins", json!({}))
            },
            &[],
        )
        .await
        .expect("persisted");
    let h = harness(storage, true, permitting_pdp());

    let bound = bootstrap(&h).run().await.expect("bootstrap");

    for held in [METRIC_TOKENS, METRIC_REQUESTS] {
        assert!(
            matches!(
                bound.catalog.peek(&metric(held)),
                Some(CatalogEntry::Served(_))
            ),
            "{held}"
        );
    }
    assert_eq!(registry_reads(&h.registry), 1);
}

#[tokio::test]
async fn a_start_while_the_registry_does_not_answer_fails() {
    let storage = Arc::new(InMemoryStorage::new());
    seed_active_quota(&storage, METRIC_TOKENS).await;
    let h = harness(storage, true, permitting_pdp());
    fail_registry_reads(&h.registry);

    let Err(err) = bootstrap(&h).run().await else {
        panic!("a replica must not start with an empty catalogue");
    };
    assert!(format!("{err:?}").contains("TypesRegistry"), "{err:?}");
    assert!(!h.readiness.is_ready());
    assert_eq!(h.metrics.catalog_read_failures(), [CatalogReadPhase::Start]);
}

#[tokio::test]
async fn an_active_policy_that_no_longer_fits_its_metric_type_is_not_a_bootstrap_failure() {
    let storage = Arc::new(InMemoryStorage::new());
    let snapshot = PolicySchemaSnapshot {
        schemas: [(
            METRIC_TOKENS.to_owned(),
            json!({
                "type": "object",
                "properties": {
                    "metadata": { "type": "object", "properties": { "zone": { "type": "string" } } }
                }
            }),
        )]
        .into_iter()
        .collect(),
        ..PolicySchemaSnapshot::default()
    };
    let policy = storage
        .create_policy(
            &actor(),
            PolicyDraft {
                schema_snapshot: snapshot,
                ..persisted_policy(
                    "cel",
                    json!({ "expr": r#"{"deny": {"reason": request.zone}}"# }),
                )
            },
            &[],
        )
        .await
        .expect("persisted");
    let h = harness(storage, true, permitting_pdp());

    let bound = bootstrap(&h)
        .run()
        .await
        .expect("the mismatch is found at evaluation");
    assert!(h.readiness.is_ready());
    assert!(
        bound
            .artifacts
            .get(&policy.policy_id, policy.version)
            .is_some(),
        "rebuilt from the snapshot it was checked against"
    );
}

#[test]
fn qe_registers_no_gts_type_for_subjects_or_metrics() {
    let ids: Vec<String> = toolkit::gts::all_inventory_type_schemas()
        .expect("inventory")
        .into_iter()
        .chain(toolkit::gts::all_inventory_instances().expect("inventory"))
        .filter_map(|document| {
            document["$id"]
                .as_str()
                .or_else(|| document["id"].as_str())
                .map(|id| id.trim_start_matches("gts://").to_owned())
        })
        .collect();
    for retired in [
        "subj",
        "scope",
        "res",
        "request",
        "constraint",
        "metric_type",
    ] {
        let prefix = format!("gts.cf.core.qe.{retired}.v1~");
        assert!(
            ids.iter().all(|id| !id.starts_with(&prefix)),
            "{prefix} is defined"
        );
    }
    for kept in [
        quota_enforcement_sdk::QuotaEnforcementStoragePluginSpecV1::TYPE_ID,
        quota_enforcement_sdk::QuotaNotificationSinkSpecV1::TYPE_ID,
    ] {
        assert!(ids.iter().any(|id| id == kept), "{kept} is defined");
    }
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
            &actor(),
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
            &actor(),
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
        &[EngineLabel::from_linked_id("most-restrictive-wins")]
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
        crate::test_support::actor(),
        tenant(),
        None,
        None,
        crate::domain::quotas::events::ChangeKind::Created,
        time::OffsetDateTime::now_utc(),
    );
    storage
        .create_quota(
            &actor(),
            &AccessScope::allow_all(),
            quota_draft(
                SubjectRef {
                    subject_type: GtsTypeId::new(USER_SUBJECT_TYPE),
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
        assert_eq!(received[0].event_id, event.event_id);
        assert_eq!(received[0].actor, event.actor);
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
