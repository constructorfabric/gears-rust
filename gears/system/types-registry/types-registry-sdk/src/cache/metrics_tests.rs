use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::data::{
    AggregatedMetrics, MetricData, ResourceMetrics, ScopeMetrics, SumDataPoint,
};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{
    CacheSettings, CanonicalError, MockTypesRegistry, OTHER, PlatformTypesRegistryApi, THIRD, TYPE,
    ctx, fixture_with, full, gts_key, schema,
};
use crate::cache::CachedTypesRegistry;
use crate::contract::TypesRegistryApi;
use crate::models::BatchGetEntitiesRequest;

fn recorder() -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    (provider, exporter)
}

fn collect(provider: &SdkMeterProvider, exporter: &InMemoryMetricExporter) -> Vec<ResourceMetrics> {
    exporter.reset();
    provider.force_flush().expect("export metrics");
    exporter.get_finished_metrics().expect("read metrics")
}

fn value(metrics: &[ResourceMetrics], suffix: &str, plane: &str, labels: &[(&str, &str)]) -> i64 {
    let name = format!("types_registry_client_cache_{suffix}");
    let metric = metrics
        .iter()
        .flat_map(ResourceMetrics::scope_metrics)
        .flat_map(ScopeMetrics::metrics)
        .find(|metric| metric.name() == name)
        .expect("metric exported");
    match metric.data() {
        AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
            .data_points()
            .find(|point| has_labels(point.attributes(), plane, labels))
            .map(|point| i64::try_from(point.value()).expect("small counter")),
        AggregatedMetrics::I64(MetricData::Sum(sum)) => sum
            .data_points()
            .find(|point| has_labels(point.attributes(), plane, labels))
            .map(SumDataPoint::value),
        _ => panic!("expected a sum"),
    }
    .expect("matching labels")
}

fn has_labels<'a>(
    attrs: impl Iterator<Item = &'a KeyValue>,
    plane: &str,
    labels: &[(&str, &str)],
) -> bool {
    let expected = [("plane", plane)].into_iter().chain(labels.iter().copied());
    let mut count = 0;
    for attr in attrs {
        if !expected
            .clone()
            .any(|(key, value)| attr.key.as_str() == key && attr.value.as_str() == value)
        {
            return false;
        }
        count += 1;
    }
    count == labels.len() + 1
}

#[tokio::test]
async fn cache_metrics_report_lookups_evictions_and_live_occupancy() {
    let (provider, exporter) = recorder();
    let meter = provider.meter("cf-gears-types-registry-sdk");
    let settings = CacheSettings {
        max_entries: 1,
        max_identity_entries: 1,
        ..CacheSettings::default()
    };
    let mut first = fixture_with(MockTypesRegistry::new(), settings);
    let mut second = fixture_with(MockTypesRegistry::new(), settings);
    first.cache.cache.metrics.use_test_meter(&meter);
    second.cache.cache.metrics.use_test_meter(&meter);

    first
        .read_with(
            vec![gts_key(TYPE).into(), gts_key(TYPE).into()],
            full(),
            false,
        )
        .await
        .expect("duplicate key");
    first
        .read_with(
            vec![gts_key(TYPE).into(), gts_key(OTHER).into()],
            full(),
            false,
        )
        .await
        .expect("mixed batch");
    first.lookup(gts_key(OTHER), full(), true).await;
    first
        .cache
        .batch_resolve_identities(&ctx(), vec![gts_key(OTHER), gts_key(TYPE), gts_key(TYPE)])
        .await
        .expect("resolve");
    second.lookup(gts_key(THIRD), full(), false).await;
    second.recorder.tamper_next(|response| response.0.clear());
    assert!(matches!(
        second
            .read_with(vec![gts_key(TYPE).into()], full(), false)
            .await,
        Err(CanonicalError::Internal { .. })
    ));

    let metrics = collect(&provider, &exporter);
    for (operation, result, count) in [
        ("batch_get", "hit", 1),
        ("batch_get", "miss", 4),
        ("batch_get", "revalidate", 1),
        ("batch_resolve", "hit", 1),
        ("batch_resolve", "miss", 1),
    ] {
        assert_eq!(
            value(
                &metrics,
                "lookups",
                "platform",
                &[("operation", operation), ("result", result)]
            ),
            count,
            "{operation} {result}"
        );
    }
    assert_eq!(
        value(&metrics, "evictions", "platform", &[("cache", "snapshot")]),
        1
    );
    assert_eq!(
        value(&metrics, "evictions", "platform", &[("cache", "identity")]),
        2
    );
    assert_eq!(
        value(&metrics, "entries", "platform", &[("cache", "snapshot")]),
        2
    );
    assert_eq!(
        value(&metrics, "entries", "platform", &[("cache", "identity")]),
        2
    );
    let bytes = first.cache.store().used() + second.cache.store().used();
    assert!(bytes > 0);
    assert_eq!(
        value(&metrics, "size", "platform", &[]),
        i64::try_from(bytes).unwrap()
    );

    drop(first);
    second.cache.invalidate(&[gts_key(THIRD)]);
    let metrics = collect(&provider, &exporter);
    assert_eq!(
        value(&metrics, "evictions", "platform", &[("cache", "snapshot")]),
        1
    );
    assert_eq!(
        value(&metrics, "entries", "platform", &[("cache", "snapshot")]),
        0
    );
    assert_eq!(
        value(&metrics, "entries", "platform", &[("cache", "identity")]),
        1
    );
    assert_eq!(value(&metrics, "size", "platform", &[]), 0);
}

#[tokio::test]
async fn tenant_metrics_include_visibility_revalidation_and_skip_anonymous_bypass() {
    let (provider, exporter) = recorder();
    let meter = provider.meter("cf-gears-types-registry-sdk");
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed(TYPE, schema(TYPE, "v1"));
    let mut cache = CachedTypesRegistry::new(registry, CacheSettings::default());
    cache.cache.metrics.use_test_meter(&meter);
    let tenant = |id| {
        SecurityContext::builder()
            .subject_id(Uuid::from_u128(id + 100))
            .subject_tenant_id(Uuid::from_u128(id))
            .build()
            .expect("tenant context")
    };
    for context in [tenant(1), tenant(2), SecurityContext::anonymous()] {
        cache
            .batch_get_entities(
                &context,
                BatchGetEntitiesRequest {
                    items: vec![gts_key(TYPE).into()],
                    projection: full(),
                    fresh: false,
                },
            )
            .await
            .expect("read");
    }
    cache
        .batch_resolve_identities(&tenant(2), vec![gts_key(TYPE), gts_key(OTHER)])
        .await
        .expect("resolve");

    let metrics = collect(&provider, &exporter);
    for (operation, result) in [
        ("batch_get", "miss"),
        ("batch_get", "revalidate"),
        ("batch_resolve", "hit"),
        ("batch_resolve", "miss"),
    ] {
        assert_eq!(
            value(
                &metrics,
                "lookups",
                "tenant",
                &[("operation", operation), ("result", result)],
            ),
            1,
            "{operation} {result}"
        );
    }
    assert_eq!(
        value(&metrics, "entries", "tenant", &[("cache", "snapshot")]),
        1
    );
}
