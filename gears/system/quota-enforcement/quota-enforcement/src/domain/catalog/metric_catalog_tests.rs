#![allow(clippy::expect_used)]

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use gts::GtsTypeId;
use quota_enforcement_sdk::{MetricId, MetricKind, SubjectRef};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::PlatformTypesRegistryApi;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

use super::{MetricCatalog, UnknownMemory};
use crate::domain::catalog::{CatalogEntry, MetricMode};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{CatalogReadPhase, ValidationReason, ValidationSurface};
use crate::infra::types_registry::PlatformTypeRegistry;
use crate::test_support::{
    METRIC_OTHER, METRIC_REQUESTS, METRIC_TOKENS, RecordingMetrics, USER_SUBJECT_TYPE,
    catalog_over, fail_registry_reads, metric_type_document, platform_registry, registry_reads,
    subject_type_document,
};

const LATE_METRIC: &str = "gts.cf.core.quota.metric.v1~cf.qetest.metric.late.v1~";
const DRAFT_METRIC: &str = "gts.cf.core.quota.metric.v1~cf.qetest.metric.draft.v0~";
const ABSTRACT_METRIC: &str = "gts.cf.core.quota.metric.v1~cf.qetest.metric.family.v1~";
const UUID_SUBJECT_TYPE: &str = "gts.cf.core.quota.subj.v1~acme.core.quota.account.v1~";

fn metric(id: &str) -> MetricId {
    MetricId::parse(id).expect("metric type id")
}

fn counter(id: &str) -> serde_json::Value {
    metric_type_document(id, json!({ "kind": "counter" }), json!({}))
}

const SURFACE: ValidationSurface = ValidationSurface::RequestMetric;

#[tokio::test]
async fn a_metric_registered_after_start_is_served_on_its_first_request() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    registry.seed(LATE_METRIC, counter(LATE_METRIC));

    let served = catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect("served on first use");
    assert_eq!(served.id().as_str(), LATE_METRIC);
    let reads = registry_reads(&registry);
    assert_eq!(reads, 1);

    catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect("served again");
    assert_eq!(
        registry_reads(&registry),
        reads,
        "a held entry needs no read"
    );
}

#[tokio::test]
async fn an_invalid_metric_type_is_unavailable_while_others_are_served() {
    let registry = platform_registry();
    let metrics = Arc::new(RecordingMetrics::default());
    let catalog = catalog_over(&registry, metrics.clone());
    let mut family = counter(ABSTRACT_METRIC);
    family["x-gts-abstract"] = json!(true);
    registry.seed(ABSTRACT_METRIC, family);

    let err = catalog
        .served(&metric(ABSTRACT_METRIC), SURFACE)
        .await
        .expect_err("unavailable");
    assert!(
        matches!(err, DomainError::ProjectionNotResolvable { .. }),
        "{err:?}"
    );
    assert!(matches!(
        catalog.peek(&metric(ABSTRACT_METRIC)),
        Some(CatalogEntry::Unavailable {
            reason: ValidationReason::Abstract
        })
    ));
    assert!(
        metrics
            .contract_failures()
            .contains(&(ValidationSurface::Bootstrap, ValidationReason::Abstract))
    );
    catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("another metric is served");
}

#[tokio::test]
async fn a_metric_type_of_major_zero_is_unavailable() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    registry.seed(DRAFT_METRIC, counter(DRAFT_METRIC));

    let err = catalog
        .served(&metric(DRAFT_METRIC), SURFACE)
        .await
        .expect_err("unavailable");
    assert!(
        matches!(err, DomainError::ProjectionNotResolvable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn an_unregistered_metric_is_invalid_and_nothing_is_held() {
    let registry = platform_registry();
    let metrics = Arc::new(RecordingMetrics::default());
    let catalog = catalog_over(&registry, metrics.clone());

    for _ in 0..2 {
        let err = catalog
            .served(&metric(METRIC_OTHER), SURFACE)
            .await
            .expect_err("unregistered");
        assert!(
            matches!(err, DomainError::MetricNotRegistered { .. }),
            "{err:?}"
        );
        assert!(matches!(
            CanonicalError::from(err),
            CanonicalError::InvalidArgument { .. }
        ));
    }
    assert!(catalog.peek(&metric(METRIC_OTHER)).is_none());
    assert_eq!(
        registry_reads(&registry),
        1,
        "the second request inside the remember time makes no registry call"
    );
    assert!(
        metrics
            .contract_failures()
            .contains(&(SURFACE, ValidationReason::MetricUnregistered))
    );
}

#[tokio::test]
async fn a_deleted_metric_type_is_unregistered() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    registry.seed_entity(Seed::new(LATE_METRIC, counter(LATE_METRIC)).deleted());

    let err = catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect_err("deleted");
    assert!(
        matches!(err, DomainError::MetricNotRegistered { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn an_id_outside_the_metric_base_is_invalid_without_a_registry_call() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));

    let err = catalog
        .served(&metric(USER_SUBJECT_TYPE), SURFACE)
        .await
        .expect_err("not a metric type id");
    assert!(
        matches!(err, DomainError::InvalidArgument { .. }),
        "{err:?}"
    );
    assert_eq!(registry_reads(&registry), 0);
    assert!(MetricCatalog::parse_metric(USER_SUBJECT_TYPE).is_none());
    assert!(MetricCatalog::parse_metric("gts.cf.core.quota.metric.v1~").is_none());
    assert!(MetricCatalog::parse_metric("not a gts id").is_none());
    assert!(MetricCatalog::parse_subject_type(METRIC_TOKENS).is_none());
    assert!(MetricCatalog::parse_subject_type(USER_SUBJECT_TYPE).is_some());
}

#[tokio::test]
async fn a_registry_that_does_not_answer_fails_a_first_use_but_not_a_held_metric() {
    let registry = platform_registry();
    let metrics = Arc::new(RecordingMetrics::default());
    let catalog = catalog_over(&registry, metrics.clone());
    catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("held");
    fail_registry_reads(&registry);

    let err = catalog
        .served(&metric(METRIC_REQUESTS), SURFACE)
        .await
        .expect_err("first use while the registry is down");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
    assert!(matches!(
        CanonicalError::from(err),
        CanonicalError::ServiceUnavailable { .. }
    ));
    assert!(catalog.peek(&metric(METRIC_REQUESTS)).is_none());
    catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("a held metric needs no registry");
    assert_eq!(
        metrics.catalog_read_failures(),
        [CatalogReadPhase::FirstUse]
    );
}

#[tokio::test(start_paused = true)]
async fn after_a_failed_first_read_the_next_ones_fail_at_once_for_a_moment_and_then_read_again() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    registry.inject(
        Fault::on(Call::BatchGet).first(1).fail(
            CanonicalError::service_unavailable()
                .with_detail("registry down")
                .create(),
        ),
    );

    assert!(matches!(
        catalog.served(&metric(METRIC_TOKENS), SURFACE).await,
        Err(DomainError::TypesRegistryUnavailable(_))
    ));
    let reads = registry_reads(&registry);
    assert!(matches!(
        catalog.served(&metric(METRIC_REQUESTS), SURFACE).await,
        Err(DomainError::TypesRegistryUnavailable(_))
    ));
    assert_eq!(registry_reads(&registry), reads, "no read a moment later");

    tokio::time::advance(Duration::from_secs(2)).await;
    catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served after the registry answers");
}

#[tokio::test]
async fn requests_that_miss_the_same_metric_together_make_one_registry_read() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    let reads = registry_reads(&registry);

    let tokens = metric(METRIC_TOKENS);
    let (one, two, three) = tokio::join!(
        catalog.served(&tokens, SURFACE),
        catalog.served(&tokens, SURFACE),
        catalog.served(&tokens, SURFACE),
    );

    assert!(one.is_ok() && two.is_ok() && three.is_ok());
    assert_eq!(registry_reads(&registry), reads + 1);
}

#[tokio::test]
async fn a_registered_subject_type_is_checked_once_and_cached() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    let subject = SubjectRef {
        subject_type: GtsTypeId::new(USER_SUBJECT_TYPE),
        subject_id: "u1".to_owned(),
    };

    catalog
        .check_subject(&subject, SURFACE)
        .await
        .expect("registered");
    let reads = registry_reads(&registry);
    catalog
        .check_subject(&subject, SURFACE)
        .await
        .expect("cached");
    assert_eq!(registry_reads(&registry), reads);
}

#[tokio::test]
async fn an_unregistered_abstract_or_misfit_subject_is_rejected() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    let mut account = subject_type_document(UUID_SUBJECT_TYPE);
    account["allOf"][1]["properties"]["id"]["format"] = json!("uuid");
    registry.seed(UUID_SUBJECT_TYPE, account);
    let subject = |subject_type: &str, id: &str| SubjectRef {
        subject_type: GtsTypeId::new(subject_type),
        subject_id: id.to_owned(),
    };

    let unregistered = catalog
        .check_subject(
            &subject("gts.cf.core.quota.subj.v1~acme.core.quota.robot.v1~", "r1"),
            SURFACE,
        )
        .await
        .expect_err("unregistered");
    assert!(
        matches!(unregistered, DomainError::ProjectionNotRegistered { .. }),
        "{unregistered:?}"
    );
    let misfit = catalog
        .check_subject(&subject(UUID_SUBJECT_TYPE, "not-a-uuid"), SURFACE)
        .await
        .expect_err("the id does not fit");
    assert!(
        matches!(misfit, DomainError::InvalidArgument { .. }),
        "{misfit:?}"
    );
    catalog
        .check_subject(
            &subject(UUID_SUBJECT_TYPE, "5f0b8e0e-1c1d-4a52-9d8e-2f6f0f3e4b11"),
            SURFACE,
        )
        .await
        .expect("fits");
    let abstract_base = catalog
        .check_subject(
            &subject(quota_resolver_sdk::QUOTA_SUBJECT_TYPE_ID, "x"),
            SURFACE,
        )
        .await
        .expect_err("the base is abstract");
    assert!(
        matches!(abstract_base, DomainError::InvalidArgument { .. }),
        "{abstract_base:?}"
    );
}

#[tokio::test]
async fn a_registration_by_another_module_that_names_a_metric_changes_nothing() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    let before = catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");
    let intruder = "gts.cf.core.quota.metric.v1~cf.intruder.quota.copy.v1~";
    let mut document = metric_type_document(
        intruder,
        json!({ "kind": "gauge", "enforcement": "direct", "admitted_subjects": [] }),
        json!({ "metric": { "const": METRIC_TOKENS } }),
    );
    document["description"] = json!(METRIC_TOKENS);
    registry.seed(intruder, document);

    let fresh = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    for catalog in [&catalog, &fresh] {
        let served = catalog
            .served(&metric(METRIC_TOKENS), SURFACE)
            .await
            .expect("still served");
        assert_eq!(served.kind(), before.kind());
        assert_eq!(served.mode(), before.mode());
        assert!(served.admits(&GtsTypeId::new(USER_SUBJECT_TYPE)));
    }
}

fn catalog_remembering(
    registry: &Arc<MockTypesRegistry>,
    metrics: Arc<RecordingMetrics>,
    memory: UnknownMemory,
) -> MetricCatalog {
    MetricCatalog::new(
        Arc::new(PlatformTypeRegistry::new(
            Arc::clone(registry) as Arc<dyn PlatformTypesRegistryApi>,
            Duration::from_secs(5),
        )),
        metrics,
        memory,
    )
}

fn revise(registry: &MockTypesRegistry, id: &str, document: serde_json::Value, version: u64) {
    registry.seed_entity(Seed::new(id, document).version(version));
}

fn user() -> SubjectRef {
    SubjectRef {
        subject_type: GtsTypeId::new(USER_SUBJECT_TYPE),
        subject_id: "u1".to_owned(),
    }
}

#[tokio::test(start_paused = true)]
async fn an_unknown_metric_is_read_again_after_the_remember_time() {
    let registry = platform_registry();
    let memory = UnknownMemory {
        ttl: Duration::from_secs(10),
        capacity: NonZeroUsize::new(8).expect("capacity"),
    };
    let catalog = catalog_remembering(&registry, Arc::new(RecordingMetrics::default()), memory);

    assert!(catalog.served(&metric(LATE_METRIC), SURFACE).await.is_err());
    registry.seed(LATE_METRIC, counter(LATE_METRIC));
    assert!(
        catalog.served(&metric(LATE_METRIC), SURFACE).await.is_err(),
        "still remembered as unknown"
    );
    assert_eq!(registry_reads(&registry), 1);

    tokio::time::advance(Duration::from_secs(11)).await;
    catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect("read again and served");
    assert_eq!(registry_reads(&registry), 2);
}

#[tokio::test]
async fn the_memory_of_unknown_ids_drops_the_least_recently_used() {
    let registry = platform_registry();
    let memory = UnknownMemory {
        ttl: Duration::from_secs(3600),
        capacity: NonZeroUsize::new(1).expect("capacity"),
    };
    let catalog = catalog_remembering(&registry, Arc::new(RecordingMetrics::default()), memory);

    assert!(
        catalog
            .served(&metric(METRIC_OTHER), SURFACE)
            .await
            .is_err()
    );
    assert!(catalog.served(&metric(LATE_METRIC), SURFACE).await.is_err());
    assert_eq!(registry_reads(&registry), 2);
    assert!(
        catalog
            .served(&metric(METRIC_OTHER), SURFACE)
            .await
            .is_err()
    );
    assert_eq!(
        registry_reads(&registry),
        3,
        "the older answer was dropped and is read again"
    );
}

#[tokio::test]
async fn a_request_naming_several_types_not_held_reads_them_with_one_batch_read() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    let tokens = metric(METRIC_TOKENS);
    let requests = metric(METRIC_REQUESTS);
    let subject = GtsTypeId::new(USER_SUBJECT_TYPE);

    catalog
        .load([
            tokens.as_gts(),
            requests.as_gts(),
            &subject,
            tokens.as_gts(),
        ])
        .await
        .expect("read");
    assert_eq!(registry_reads(&registry), 1);

    catalog.served(&tokens, SURFACE).await.expect("held");
    catalog.served(&requests, SURFACE).await.expect("held");
    catalog.check_subject(&user(), SURFACE).await.expect("held");
    catalog
        .load([tokens.as_gts(), &subject])
        .await
        .expect("held");
    assert_eq!(registry_reads(&registry), 1, "held types need no read");
}

#[tokio::test]
async fn a_revision_reaches_a_running_replica_at_the_next_scheduled_read_as_a_fresh_one_builds_it()
{
    let registry = platform_registry();
    let metrics = Arc::new(RecordingMetrics::default());
    let running = catalog_over(&registry, metrics.clone());
    let before = running
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");
    assert_eq!(before.mode(), MetricMode::QuotaGated);
    revise(
        &registry,
        METRIC_TOKENS,
        metric_type_document(
            METRIC_TOKENS,
            json!({ "kind": "gauge", "enforcement": "direct", "admitted_subjects": [USER_SUBJECT_TYPE] }),
            json!({ "region": { "type": "string" } }),
        ),
        2,
    );
    let held = running
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("still the held entry");
    assert_eq!(
        held.mode(),
        MetricMode::QuotaGated,
        "no read before the schedule"
    );

    running.refresh().await.expect("scheduled read");

    let fresh = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    let revised = running
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");
    let restarted = fresh
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");
    assert_eq!(revised.mode(), MetricMode::Direct);
    assert_eq!(revised.kind(), MetricKind::Gauge);
    assert_eq!(format!("{revised:?}"), format!("{restarted:?}"));
    assert_eq!(revised.schema(), restarted.schema());
    assert_eq!(metrics.catalog_scheduled_reads(), 1);
}

fn with_region(id: &str) -> serde_json::Value {
    metric_type_document(
        id,
        json!({ "kind": "counter" }),
        json!({ "region": { "type": "string" } }),
    )
}

#[tokio::test]
async fn a_reread_serves_a_revision_before_the_schedule() {
    let registry = platform_registry();
    registry.seed(LATE_METRIC, counter(LATE_METRIC));
    let catalog = catalog_remembering(
        &registry,
        Arc::new(RecordingMetrics::default()),
        UnknownMemory {
            ttl: Duration::ZERO,
            capacity: NonZeroUsize::MIN,
        },
    );
    let document = json!({ "type": LATE_METRIC, "metadata": { "region": "eu" } });
    let before = catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect("served");
    assert!(before.validate(&document).is_err(), "no such attribute yet");
    assert!(
        catalog.reread(&metric(LATE_METRIC)).await.is_none(),
        "the type did not change"
    );

    revise(&registry, LATE_METRIC, with_region(LATE_METRIC), 2);

    let revised = catalog
        .reread(&metric(LATE_METRIC))
        .await
        .expect("the revision");
    assert!(revised.validate(&document).is_ok());
    let served = catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect("served");
    assert!(Arc::ptr_eq(&revised, &served), "the catalogue holds it");
}

#[tokio::test]
async fn a_metric_is_reread_once_in_the_remember_time() {
    let registry = platform_registry();
    registry.seed(LATE_METRIC, counter(LATE_METRIC));
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect("served");
    let reads = registry_reads(&registry);

    assert!(catalog.reread(&metric(LATE_METRIC)).await.is_none());
    revise(&registry, LATE_METRIC, with_region(LATE_METRIC), 2);
    assert!(
        catalog.reread(&metric(LATE_METRIC)).await.is_none(),
        "read a moment ago"
    );

    assert_eq!(registry_reads(&registry), reads + 1);
}

#[tokio::test]
async fn an_unchanged_type_is_not_rebuilt_by_a_scheduled_read() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    let before = catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");

    catalog.refresh().await.expect("scheduled read");

    let after = catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");
    assert!(Arc::ptr_eq(&before, &after));
}

#[tokio::test]
async fn a_revision_that_stops_admitting_a_subject_type_leaves_the_metric_served() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    let before = catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");
    assert!(before.admits(&GtsTypeId::new(USER_SUBJECT_TYPE)));
    revise(
        &registry,
        METRIC_TOKENS,
        metric_type_document(
            METRIC_TOKENS,
            json!({ "kind": "counter" }),
            json!({ "region": { "type": "string" }, "model": { "type": "string" } }),
        ),
        2,
    );

    catalog.refresh().await.expect("scheduled read");

    let after = catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("still served");
    assert!(!after.admits(&GtsTypeId::new(USER_SUBJECT_TYPE)));
}

#[tokio::test]
async fn an_unavailable_metric_is_served_after_its_type_becomes_valid() {
    let registry = platform_registry();
    let metrics = Arc::new(RecordingMetrics::default());
    let catalog = catalog_over(&registry, metrics.clone());
    let mut family = counter(LATE_METRIC);
    family["x-gts-abstract"] = json!(true);
    registry.seed(LATE_METRIC, family);
    assert!(matches!(
        catalog.served(&metric(LATE_METRIC), SURFACE).await,
        Err(DomainError::ProjectionNotResolvable { .. })
    ));
    assert_eq!(metrics.catalog_unavailable(), Some(1));
    assert!(
        metrics
            .contract_failures()
            .contains(&(ValidationSurface::Bootstrap, ValidationReason::Abstract))
    );

    revise(&registry, LATE_METRIC, counter(LATE_METRIC), 2);
    catalog.refresh().await.expect("scheduled read");

    catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect("served after the revision");
    assert_eq!(metrics.catalog_unavailable(), Some(0));
}

#[tokio::test]
async fn a_deleted_metric_type_and_a_deleted_subject_type_leave_the_catalogue() {
    let registry = platform_registry();
    let catalog = catalog_over(&registry, Arc::new(RecordingMetrics::default()));
    catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");
    catalog
        .check_subject(&user(), SURFACE)
        .await
        .expect("cached");
    registry.seed_entity(
        Seed::new(
            METRIC_TOKENS,
            registry.content(METRIC_TOKENS).expect("content"),
        )
        .version(2)
        .deleted(),
    );
    registry.seed_entity(
        Seed::new(
            USER_SUBJECT_TYPE,
            registry.content(USER_SUBJECT_TYPE).expect("content"),
        )
        .version(2)
        .deleted(),
    );

    catalog.refresh().await.expect("scheduled read");

    assert!(catalog.peek(&metric(METRIC_TOKENS)).is_none());
    assert!(matches!(
        catalog.served(&metric(METRIC_TOKENS), SURFACE).await,
        Err(DomainError::MetricNotRegistered { .. })
    ));
    assert!(matches!(
        catalog.check_subject(&user(), SURFACE).await,
        Err(DomainError::ProjectionNotRegistered { .. })
    ));
}

#[tokio::test]
async fn a_failed_scheduled_read_changes_nothing_and_is_counted() {
    let registry = platform_registry();
    let metrics = Arc::new(RecordingMetrics::default());
    let catalog = catalog_over(&registry, metrics.clone());
    catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");
    revise(
        &registry,
        METRIC_TOKENS,
        metric_type_document(METRIC_TOKENS, json!({ "kind": "gauge" }), json!({})),
        2,
    );
    fail_registry_reads(&registry);

    assert!(matches!(
        catalog.refresh().await,
        Err(DomainError::TypesRegistryUnavailable(_))
    ));

    let served = catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("still served");
    assert_eq!(served.kind(), MetricKind::Counter);
    assert_eq!(
        metrics.catalog_read_failures(),
        [CatalogReadPhase::Scheduled]
    );
    assert_eq!(metrics.catalog_scheduled_reads(), 0);
}

#[tokio::test]
async fn a_read_at_start_fills_the_catalogue_and_a_failed_one_is_an_error() {
    let registry = platform_registry();
    let metrics = Arc::new(RecordingMetrics::default());
    let catalog = catalog_over(&registry, metrics.clone());
    catalog
        .read_at_start(&[metric(METRIC_TOKENS), metric(METRIC_REQUESTS)])
        .await
        .expect("read");
    assert_eq!(registry_reads(&registry), 1);
    catalog
        .served(&metric(METRIC_REQUESTS), SURFACE)
        .await
        .expect("held");
    assert_eq!(registry_reads(&registry), 1);

    let silent = platform_registry();
    fail_registry_reads(&silent);
    let cold = catalog_over(&silent, metrics.clone());
    let err = cold
        .read_at_start(&[metric(METRIC_TOKENS)])
        .await
        .expect_err("the registry does not answer");
    assert!(matches!(err, DomainError::TypesRegistryUnavailable(_)));
    assert!(cold.peek(&metric(METRIC_TOKENS)).is_none());
    assert_eq!(metrics.catalog_read_failures(), [CatalogReadPhase::Start]);
}

#[tokio::test]
async fn a_stored_id_outside_the_metric_base_is_unavailable_without_a_registry_read() {
    let registry = platform_registry();
    let metrics = Arc::new(RecordingMetrics::default());
    let catalog = catalog_over(&registry, metrics.clone());
    let not_derived = |entry: Option<CatalogEntry>| {
        matches!(
            entry,
            Some(CatalogEntry::Unavailable {
                reason: ValidationReason::NotDerived
            })
        )
    };

    catalog
        .read_at_start(&[metric(USER_SUBJECT_TYPE)])
        .await
        .expect("no registry read is needed");
    let entry = catalog
        .entry(&metric(UUID_SUBJECT_TYPE))
        .await
        .expect("no registry read to fail");

    assert!(not_derived(entry));
    assert!(not_derived(catalog.peek(&metric(USER_SUBJECT_TYPE))));
    assert_eq!(metrics.catalog_unavailable(), Some(2));
    assert_eq!(
        metrics.contract_failures(),
        [(ValidationSurface::Bootstrap, ValidationReason::NotDerived); 2]
    );
    let again = catalog
        .entry(&metric(UUID_SUBJECT_TYPE))
        .await
        .expect("held");
    assert!(not_derived(again));
    catalog.refresh().await.expect("nothing to read");
    assert_eq!(registry_reads(&registry), 0);
}

#[tokio::test(start_paused = true)]
async fn the_scheduled_read_runs_every_interval_until_cancelled() {
    let registry = platform_registry();
    let metrics = Arc::new(RecordingMetrics::default());
    let catalog = catalog_over(&registry, metrics.clone());
    catalog
        .served(&metric(METRIC_TOKENS), SURFACE)
        .await
        .expect("served");
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let catalog = Arc::clone(&catalog);
        let cancel = cancel.clone();
        async move {
            catalog
                .run_scheduled_reads(Duration::from_secs(60), cancel)
                .await;
        }
    });

    tokio::time::sleep(Duration::from_secs(59)).await;
    assert_eq!(metrics.catalog_scheduled_reads(), 0);
    tokio::time::sleep(Duration::from_secs(62)).await;
    assert_eq!(metrics.catalog_scheduled_reads(), 2);

    cancel.cancel();
    task.await.expect("stops on cancel");
}

#[tokio::test]
async fn a_reread_does_not_overwrite_a_newer_revision_stored_meanwhile() {
    let registry = platform_registry();
    registry.seed(LATE_METRIC, counter(LATE_METRIC));
    let held = HeldRead::idle(&registry, LATE_METRIC);
    let catalog = catalog_on(Arc::clone(&held));
    catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect("served");

    // The reread's registry read answers with revision 2, and the answer is
    // held back...
    revise(&registry, LATE_METRIC, with_region(LATE_METRIC), 2);
    held.hold_next_answer();
    let reread = tokio::spawn({
        let catalog = Arc::clone(&catalog);
        async move { catalog.reread(&metric(LATE_METRIC)).await }
    });
    held.entered.notified().await;
    // ...while a scheduled read stores revision 3.
    let zone = metric_type_document(
        LATE_METRIC,
        json!({ "kind": "counter" }),
        json!({ "zone": { "type": "string" } }),
    );
    revise(&registry, LATE_METRIC, zone, 3);
    catalog.refresh().await.expect("scheduled read");
    held.release.notify_one();
    reread.await.expect("reread");

    let served = catalog
        .served(&metric(LATE_METRIC), SURFACE)
        .await
        .expect("served");
    let zoned = json!({ "type": LATE_METRIC, "metadata": { "zone": "a" } });
    let regional = json!({ "type": LATE_METRIC, "metadata": { "region": "eu" } });
    assert!(served.validate(&zoned).is_ok(), "revision 3 stays");
    assert!(served.validate(&regional).is_err(), "revision 2 is dropped");
}

/// A registry whose first read naming `held` waits until released, and that
/// counts the reads of each type. It can instead hold back the answer of one
/// read: the read is made, and its answer waits until released.
struct HeldRead {
    inner: PlatformTypeRegistry,
    held: GtsTypeId,
    first: std::sync::atomic::AtomicBool,
    answer: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    reads: parking_lot::Mutex<Vec<GtsTypeId>>,
}

impl HeldRead {
    fn over(registry: &Arc<MockTypesRegistry>, held: &str) -> Arc<Self> {
        Arc::new(Self {
            inner: PlatformTypeRegistry::new(
                Arc::clone(registry) as Arc<dyn PlatformTypesRegistryApi>,
                Duration::from_secs(5),
            ),
            held: GtsTypeId::new(held),
            first: std::sync::atomic::AtomicBool::new(true),
            answer: std::sync::atomic::AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            reads: parking_lot::Mutex::new(Vec::new()),
        })
    }

    /// One that holds no read until [`Self::hold_next_answer`].
    fn idle(registry: &Arc<MockTypesRegistry>, held: &str) -> Arc<Self> {
        let idle = Self::over(registry, held);
        idle.first.store(false, std::sync::atomic::Ordering::SeqCst);
        idle
    }

    /// Hold back the answer of the next read naming `held`.
    fn hold_next_answer(&self) {
        self.answer.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn reads_of(&self, id: &str) -> usize {
        self.reads
            .lock()
            .iter()
            .filter(|read| read.as_ref() == id)
            .count()
    }
}

#[async_trait::async_trait]
impl crate::domain::ports::type_registry::TypeRegistry for HeldRead {
    async fn read_types(
        &self,
        reads: Vec<(
            GtsTypeId,
            Option<crate::domain::ports::type_registry::TypeEtag>,
        )>,
    ) -> Result<
        std::collections::HashMap<GtsTypeId, crate::domain::ports::type_registry::TypeRead>,
        DomainError,
    > {
        self.reads
            .lock()
            .extend(reads.iter().map(|(id, _)| id.clone()));
        if reads.iter().any(|(id, _)| *id == self.held)
            && self.first.swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        let names_held = reads.iter().any(|(id, _)| *id == self.held);
        let answers = self.inner.read_types(reads).await;
        if names_held && self.answer.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        answers
    }
}

fn catalog_on(registry: Arc<HeldRead>) -> Arc<MetricCatalog> {
    Arc::new(MetricCatalog::new(
        registry,
        Arc::new(RecordingMetrics::default()),
        UnknownMemory {
            ttl: Duration::from_secs(10),
            capacity: NonZeroUsize::MIN,
        },
    ))
}

#[tokio::test]
async fn a_read_of_another_type_does_not_wait_for_one_in_progress() {
    let held = HeldRead::over(&platform_registry(), METRIC_TOKENS);
    let catalog = catalog_on(Arc::clone(&held));
    let first = tokio::spawn({
        let catalog = Arc::clone(&catalog);
        async move {
            catalog
                .served(&metric(METRIC_TOKENS), SURFACE)
                .await
                .map(|_| ())
        }
    });
    held.entered.notified().await;

    tokio::time::timeout(
        Duration::from_secs(1),
        catalog.served(&metric(METRIC_REQUESTS), SURFACE),
    )
    .await
    .expect("not queued behind the held read")
    .expect("served");

    held.release.notify_one();
    first.await.expect("join").expect("served");
}

#[tokio::test]
async fn a_request_waits_for_the_read_of_a_type_it_shares_and_reads_the_rest_itself() {
    let held = HeldRead::over(&platform_registry(), METRIC_TOKENS);
    let catalog = catalog_on(Arc::clone(&held));
    let first = tokio::spawn({
        let catalog = Arc::clone(&catalog);
        async move {
            catalog
                .served(&metric(METRIC_TOKENS), SURFACE)
                .await
                .map(|_| ())
        }
    });
    held.entered.notified().await;

    let both = tokio::spawn({
        let catalog = Arc::clone(&catalog);
        async move {
            let ids = [
                GtsTypeId::new(METRIC_TOKENS),
                GtsTypeId::new(METRIC_REQUESTS),
            ];
            catalog.load(ids.iter()).await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !both.is_finished(),
        "it waits for the read of the shared type"
    );

    held.release.notify_one();
    first.await.expect("join").expect("served");
    both.await.expect("join").expect("loaded");
    assert_eq!(
        held.reads_of(METRIC_TOKENS),
        1,
        "the shared type is read once"
    );
    assert_eq!(held.reads_of(METRIC_REQUESTS), 1);
}

#[tokio::test]
async fn a_read_whose_leader_is_dropped_is_made_again_by_its_waiter() {
    let held = HeldRead::over(&platform_registry(), METRIC_TOKENS);
    let catalog = catalog_on(Arc::clone(&held));
    let leader = tokio::spawn({
        let catalog = Arc::clone(&catalog);
        async move {
            catalog
                .served(&metric(METRIC_TOKENS), SURFACE)
                .await
                .map(|_| ())
        }
    });
    held.entered.notified().await;
    let waiter = tokio::spawn({
        let catalog = Arc::clone(&catalog);
        async move {
            catalog
                .served(&metric(METRIC_TOKENS), SURFACE)
                .await
                .map(|_| ())
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    leader.abort();
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("the waiter does not hang on the dropped read")
        .expect("join")
        .expect("served by its own read");
    assert_eq!(held.reads_of(METRIC_TOKENS), 2);
}
