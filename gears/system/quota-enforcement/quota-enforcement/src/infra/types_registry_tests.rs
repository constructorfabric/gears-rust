#![allow(clippy::expect_used)]

use std::time::Duration;

use gts::GtsTypeId;
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::testing_platform::{Call, Fault, Seed};

use super::PlatformTypeRegistry;
use crate::domain::error::DomainError;
use crate::domain::ports::type_registry::{RegisteredType, TypeEtag, TypeRead, TypeRegistry};
use crate::test_support::{
    METRIC_OTHER, METRIC_REQUESTS, METRIC_TOKENS, metric_type_document, platform_registry,
};

const DEADLINE: Duration = Duration::from_millis(200);

async fn read_one(
    adapter: &PlatformTypeRegistry,
    id: &str,
    etag: Option<TypeEtag>,
) -> Result<TypeRead, DomainError> {
    let id = GtsTypeId::new(id);
    let mut answers = adapter.read_types(vec![(id.clone(), etag)]).await?;
    Ok(answers.remove(&id).expect("every asked id is answered"))
}

fn found(read: TypeRead) -> RegisteredType {
    match read {
        TypeRead::Found(registered) => *registered,
        other => panic!("expected a document, got {other:?}"),
    }
}

#[tokio::test]
async fn a_registered_type_reads_with_its_traits_schema_and_validator() {
    let adapter = PlatformTypeRegistry::new(platform_registry(), DEADLINE);
    let read = found(read_one(&adapter, METRIC_TOKENS, None).await.expect("read"));
    assert!(!read.is_abstract);
    assert_eq!(read.effective_traits["kind"], json!("counter"));
    let validator = read.payload.expect("concrete types compile");
    assert!(validator.is_valid(&json!({
        "type": METRIC_TOKENS,
        "metadata": { "region": "eu" }
    })));
    assert!(!validator.is_valid(&json!({
        "type": METRIC_TOKENS,
        "metadata": { "unknown": 1 }
    })));
}

#[tokio::test]
async fn an_unknown_or_deleted_type_reads_as_absent() {
    let registry = platform_registry();
    let deleted = "gts.cf.core.quota.metric.v1~cf.qetest.metric.gone.v1~";
    registry.seed_entity(
        Seed::new(
            deleted,
            metric_type_document(deleted, json!({ "kind": "counter" }), json!({})),
        )
        .deleted(),
    );
    let adapter = PlatformTypeRegistry::new(registry, DEADLINE);
    for id in [METRIC_OTHER, deleted] {
        assert!(
            matches!(
                read_one(&adapter, id, None).await.expect("read"),
                TypeRead::Absent
            ),
            "{id}"
        );
    }
}

#[tokio::test]
async fn an_abstract_type_has_no_validator() {
    let adapter = PlatformTypeRegistry::new(platform_registry(), DEADLINE);
    let base = found(
        read_one(&adapter, quota_resolver_sdk::QUOTA_METRIC_TYPE_ID, None)
            .await
            .expect("read"),
    );
    assert!(base.is_abstract);
    assert!(base.payload.is_err());
}

#[tokio::test]
async fn a_read_with_the_current_token_is_unchanged_until_the_type_changes() {
    let registry = platform_registry();
    let adapter = PlatformTypeRegistry::new(registry.clone(), DEADLINE);
    let etag = found(read_one(&adapter, METRIC_TOKENS, None).await.expect("read")).etag;

    assert!(matches!(
        read_one(&adapter, METRIC_TOKENS, Some(etag.clone()))
            .await
            .expect("read"),
        TypeRead::Unchanged
    ));
    registry.seed_entity(
        Seed::new(
            METRIC_TOKENS,
            metric_type_document(METRIC_TOKENS, json!({ "kind": "gauge" }), json!({})),
        )
        .version(2),
    );
    let revised = found(
        read_one(&adapter, METRIC_TOKENS, Some(etag))
            .await
            .expect("read"),
    );
    assert_eq!(revised.effective_traits["kind"], json!("gauge"));
}

#[tokio::test]
async fn several_types_are_read_with_one_batch_read() {
    let registry = platform_registry();
    let adapter = PlatformTypeRegistry::new(registry.clone(), DEADLINE);
    let answers = adapter
        .read_types(
            [METRIC_TOKENS, METRIC_REQUESTS, METRIC_OTHER]
                .into_iter()
                .map(|id| (GtsTypeId::new(id), None))
                .collect(),
        )
        .await
        .expect("read");
    assert_eq!(answers.len(), 3);
    assert_eq!(registry.platform_calls(Call::BatchGet), 1);
}

#[tokio::test]
async fn a_failed_read_is_unavailable() {
    let registry = platform_registry();
    registry.inject(
        Fault::on(Call::BatchGet).fail(
            CanonicalError::service_unavailable()
                .with_detail("down")
                .create(),
        ),
    );
    let adapter = PlatformTypeRegistry::new(registry, DEADLINE);
    let err = read_one(&adapter, METRIC_TOKENS, None)
        .await
        .expect_err("unavailable");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_read_that_reaches_its_time_limit_is_unavailable() {
    let registry = platform_registry();
    registry.inject(Fault::on(Call::BatchGet).delay(Duration::from_secs(60)));
    let adapter = PlatformTypeRegistry::new(registry, DEADLINE);
    let err = read_one(&adapter, METRIC_TOKENS, None)
        .await
        .expect_err("timed out");
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn every_read_asks_the_registry_itself() {
    let registry = platform_registry();
    let adapter = PlatformTypeRegistry::new(registry.clone(), DEADLINE);
    for _ in 0..2 {
        read_one(&adapter, METRIC_TOKENS, None).await.expect("read");
    }
    assert_eq!(registry.platform_calls(Call::BatchGet), 2);
}
