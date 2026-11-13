#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::{MetricId, MetricKind, SCOPE_USER};
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::testing_platform::{Call, Fault, Seed};

use super::CachedMetricRegistry;
use crate::domain::error::{CallerRefusal, DomainError};
use crate::domain::ports::contracts::RegistryCaller;
use crate::domain::ports::metric_registry::{
    Freshness, MetricDescriptor, MetricMode, MetricRegistry,
};
use crate::test_support::{
    FailingReads, METRIC_OTHER, METRIC_REQUESTS, METRIC_TOKENS, classified_metric_document, ctx,
    metric_base_documents, mock_registry, unclassified_metric_document,
};

const TTL: Duration = Duration::from_mins(1);
const GRACE: Duration = Duration::from_mins(5);
const GAUGE_DIRECT: &str = "gts.cf.core.qe.metric_type.v1~cf.test.qe.gpu_seconds.v1";
const BAD_KIND: &str = "gts.cf.core.qe.metric_type.v1~cf.test.qe.bad_kind.v1";
const BAD_MODE: &str = "gts.cf.core.qe.metric_type.v1~cf.test.qe.bad_mode.v1";
const PLATFORM: RegistryCaller<'static> = RegistryCaller::Platform;

fn metric(id: &str) -> MetricId {
    MetricId::parse(id).expect("metric id")
}

/// The metric base plus classified, unclassified, and misclassified
/// instances; the scope instances come with the QE declarations.
fn registry() -> FailingReads {
    let base = metric_base_documents().remove(0);
    let instances = vec![
        classified_metric_document(METRIC_TOKENS, "counter", "quota_gated"),
        classified_metric_document(GAUGE_DIRECT, "gauge", "direct"),
        unclassified_metric_document(METRIC_OTHER),
        classified_metric_document(BAD_KIND, "histogram", "direct"),
        classified_metric_document(BAD_MODE, "counter", "sometimes"),
    ];
    FailingReads::new(mock_registry(&[base], &instances))
}

fn cached(registry: &Arc<FailingReads>, capacity: usize) -> CachedMetricRegistry {
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    CachedMetricRegistry::new(hub, capacity, TTL, GRACE)
}

/// A request's miss reads through the tenant contract under the caller's
/// context; the gear's own through the platform contract.
#[tokio::test]
async fn a_request_reads_under_the_callers_context_and_the_gear_through_the_platform() {
    let registry = Arc::new(registry());
    let cached = cached(&registry, 8);
    let caller = ctx();

    cached
        .describe(RegistryCaller::Request(&caller), &metric(METRIC_TOKENS))
        .await
        .expect("answer")
        .expect("registered");
    let contexts = registry.inner().caller_contexts(Call::BatchGet);
    assert_eq!(contexts.len(), 1, "one tenant read");
    assert_eq!(contexts[0].subject_id(), caller.subject_id());
    assert_eq!(registry.inner().platform_calls(Call::BatchGet), 0);

    cached
        .describe(PLATFORM, &metric(GAUGE_DIRECT))
        .await
        .expect("answer")
        .expect("registered");
    assert_eq!(registry.inner().platform_calls(Call::BatchGet), 1);
}

/// A deleted metric instance is no longer registered.
#[tokio::test]
async fn a_deleted_metric_instance_is_not_a_metric() {
    let registry = Arc::new(registry());
    registry.inner().seed_entity(
        Seed::new(
            METRIC_REQUESTS,
            classified_metric_document(METRIC_REQUESTS, "counter", "quota_gated"),
        )
        .deleted(),
    );
    let cached = cached(&registry, 8);
    assert_eq!(
        cached
            .describe(PLATFORM, &metric(METRIC_REQUESTS))
            .await
            .expect("answer"),
        None
    );
}

#[tokio::test]
async fn classifications_are_parsed_into_the_closed_enums() {
    let registry = Arc::new(registry());
    let cached = cached(&registry, 8);
    let tokens = cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("answer")
        .expect("registered");
    assert_eq!(
        tokens.descriptor,
        MetricDescriptor {
            kind: MetricKind::Counter,
            mode: MetricMode::QuotaGated
        }
    );
    assert_eq!(tokens.freshness, Freshness::Fresh);
    let gpu = cached
        .describe(PLATFORM, &metric(GAUGE_DIRECT))
        .await
        .expect("answer")
        .expect("registered");
    assert_eq!(
        gpu.descriptor,
        MetricDescriptor {
            kind: MetricKind::Gauge,
            mode: MetricMode::Direct
        }
    );
}

#[tokio::test]
async fn missing_or_unknown_classification_is_an_explicit_error_never_a_default() {
    let cached = cached(&Arc::new(registry()), 8);
    for id in [METRIC_OTHER, BAD_KIND, BAD_MODE] {
        let err = cached.describe(PLATFORM, &metric(id)).await.expect_err(id);
        assert_eq!(
            err,
            DomainError::MetricClassificationInvalid {
                metric: id.to_owned()
            },
            "{id}"
        );
    }
}

#[tokio::test]
async fn an_unknown_instance_or_one_of_another_type_is_not_a_metric() {
    let cached = cached(&Arc::new(registry()), 8);
    assert_eq!(
        cached
            .describe(PLATFORM, &metric(METRIC_REQUESTS))
            .await
            .expect("answer"),
        None,
        "not registered"
    );
    assert_eq!(
        cached
            .describe(PLATFORM, &MetricId::parse(SCOPE_USER).expect("instance id"))
            .await
            .expect("answer"),
        None,
        "an instance of the scope type is not a metric"
    );
}

#[tokio::test(start_paused = true)]
async fn cached_answers_are_served_within_the_ttl_and_refreshed_after_it() {
    let registry = Arc::new(registry());
    let cached = cached(&registry, 8);
    cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("first");
    cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("second");
    assert_eq!(registry.reads(), 1, "a fresh hit makes no call");
    tokio::time::advance(TTL + Duration::from_secs(1)).await;
    cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("third");
    assert_eq!(
        registry.reads(),
        2,
        "past the ttl the registry is asked again"
    );
}

#[tokio::test(start_paused = true)]
async fn an_outage_serves_a_stale_answer_within_the_grace_and_fails_closed_beyond_it() {
    let registry = Arc::new(registry());
    let cached = cached(&registry, 8);
    cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("warm");
    registry.fail_reads();

    let within_ttl = cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("hit");
    assert_eq!(within_ttl.map(|c| c.freshness), Some(Freshness::Fresh));

    tokio::time::advance(TTL + Duration::from_secs(1)).await;
    let stale = cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("served from the cache")
        .expect("known");
    assert_eq!(stale.freshness, Freshness::Stale);
    assert_eq!(stale.descriptor.kind, MetricKind::Counter);

    let cold = cached
        .describe(PLATFORM, &metric(GAUGE_DIRECT))
        .await
        .expect_err("never cached");
    assert!(
        matches!(cold, DomainError::TypesRegistryUnavailable(_)),
        "{cold:?}"
    );

    tokio::time::advance(GRACE).await;
    let expired = cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect_err("beyond ttl + grace nothing is served");
    assert!(
        matches!(expired, DomainError::TypesRegistryUnavailable(_)),
        "{expired:?}"
    );

    registry.recover();
    let again = cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("answer")
        .expect("known");
    assert_eq!(again.freshness, Freshness::Fresh);
}

#[tokio::test]
async fn the_cache_evicts_the_least_recently_used_entry_at_capacity() {
    let registry = Arc::new(registry());
    let cached = cached(&registry, 1);
    cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("a");
    cached
        .describe(PLATFORM, &metric(GAUGE_DIRECT))
        .await
        .expect("b evicts a");
    cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("a again");
    assert_eq!(registry.reads(), 3);
}

/// A metric a tenant owns answers its reader alone: it is never cached, so
/// every call reads it again under its caller's context.
#[tokio::test]
async fn a_tenant_owned_metric_is_never_cached() {
    let registry = Arc::new(registry());
    registry.answer_tenant_owned();
    let cached = cached(&registry, 8);
    let caller = ctx();
    for _ in 0..2 {
        let answer = cached
            .describe(RegistryCaller::Request(&caller), &metric(METRIC_TOKENS))
            .await
            .expect("answer")
            .expect("registered");
        assert_eq!(answer.freshness, Freshness::Fresh);
    }
    assert_eq!(registry.reads(), 2, "every call reads the registry");
}

/// A registry that refuses a request's caller answered: the refusal is
/// returned, never a stale entry, and the entry survives for the next outage.
/// The gear's own reads keep the outage policy.
#[tokio::test(start_paused = true)]
async fn a_refused_caller_is_never_served_a_stale_entry() {
    let registry = Arc::new(registry());
    let cached = cached(&registry, 8);
    let caller = ctx();
    cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("warm");
    tokio::time::advance(TTL + Duration::from_secs(1)).await;

    let unauthenticated = CanonicalError::unauthenticated()
        .with_reason("TOKEN_EXPIRED")
        .create();
    let denied = crate::infra::canonical_mapping::QuotaResource::permission_denied()
        .with_reason("FORBIDDEN")
        .create();
    for (refusal, expected) in [
        (unauthenticated.clone(), CallerRefusal::Unauthenticated),
        (denied, CallerRefusal::PermissionDenied),
    ] {
        registry
            .inner()
            .inject(Fault::on(Call::BatchGet).first(2).fail(refusal));
        for id in [METRIC_TOKENS, GAUGE_DIRECT] {
            let err = cached
                .describe(RegistryCaller::Request(&caller), &metric(id))
                .await
                .expect_err("refused");
            assert_eq!(
                err,
                DomainError::RegistryRefusedCaller(expected.clone()),
                "{id}: within the grace and cold alike"
            );
        }
    }

    registry
        .inner()
        .inject(Fault::on(Call::BatchGet).first(1).fail(unauthenticated));
    let platform = cached
        .describe(PLATFORM, &metric(METRIC_TOKENS))
        .await
        .expect("the gear's own read keeps the outage policy")
        .expect("known");
    assert_eq!(platform.freshness, Freshness::Stale);

    registry.fail_reads();
    let stale = cached
        .describe(RegistryCaller::Request(&caller), &metric(METRIC_TOKENS))
        .await
        .expect("an outage serves the entry the refusals left intact")
        .expect("known");
    assert_eq!(stale.freshness, Freshness::Stale);
}
