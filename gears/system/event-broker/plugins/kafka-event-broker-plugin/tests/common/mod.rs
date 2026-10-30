//! A Kafka cluster for the integration tests (`docs/TESTING.md` layer 2): a
//! single-node container on `libs/test-containers`' pinned tag, or the
//! cluster `EB_TEST_KAFKA_BOOTSTRAP` names.

#![cfg(feature = "integration")]
#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test harness: a fixture setup failure is the test failure, and not every helper is used by every test binary"
)]

use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::{
    Backend, BackendError, BuildContext, Partition, Provider, Role, TopicPartition,
};
use event_broker_sdk::{GtsInstanceId, Position};
use kafka_event_broker_plugin::KafkaBackendProvider;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::kafka::apache::{KAFKA_PORT, Kafka};
use uuid::Uuid;

/// The cluster, kept alive for as long as this value lives.
pub struct Cluster {
    pub bootstrap: String,
    _container: Option<ContainerAsync<Kafka>>,
}

/// `purpose` names the container `eb-kafka-test-<purpose>-<6 hex>`, so
/// `docker ps` shows which test started it.
pub async fn cluster(purpose: &str) -> Cluster {
    if let Ok(bootstrap) = std::env::var("EB_TEST_KAFKA_BOOTSTRAP") {
        return Cluster {
            bootstrap,
            _container: None,
        };
    }
    let container = test_containers::kafka()
        .with_container_name(format!(
            "eb-kafka-test-{purpose}-{}",
            &Uuid::new_v4().simple().to_string()[..6]
        ))
        .start()
        .await
        .expect("kafka container starts");
    let port = container
        .get_host_port_ipv4(KAFKA_PORT)
        .await
        .expect("mapped port");
    Cluster {
        bootstrap: format!("127.0.0.1:{port}"),
        _container: Some(container),
    }
}

/// A topic prefix no other test uses.
pub fn fresh_prefix() -> String {
    format!("evbk-t{}", &Uuid::new_v4().simple().to_string()[..12])
}

/// The settings every test instance gets: one broker, so one replica.
pub fn settings(
    bootstrap: &str,
    prefix: &str,
    extra: serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    let mut settings = serde_json::json!({
        "bootstrap_servers": bootstrap,
        "topic_prefix": prefix,
        "replication_factor": 1,
        "min_insync_replicas": 1,
        "fence_partitions": 4,
        "poll_interval_ms": 200,
    });
    if let (serde_json::Value::Object(base), serde_json::Value::Object(extra)) =
        (&mut settings, extra)
    {
        base.extend(extra);
    }
    let serde_json::Value::Object(settings) = settings else {
        unreachable!("an object literal");
    };
    settings
}

/// A backend over `bootstrap` under `prefix`, built and not started.
pub async fn built(bootstrap: &str, prefix: &str, extra: serde_json::Value) -> Arc<dyn Backend> {
    KafkaBackendProvider::default()
        .build(&BuildContext::new(
            &settings(bootstrap, prefix, extra),
            Role::Standalone,
        ))
        .await
        .expect("build")
}

/// A backend over `bootstrap` under `prefix`, started and answering.
pub async fn backend(bootstrap: &str, prefix: &str) -> Arc<dyn Backend> {
    backend_with(bootstrap, prefix, serde_json::json!({})).await
}

/// Like [`backend`], with `extra` settings.
pub async fn backend_with(
    bootstrap: &str,
    prefix: &str,
    extra: serde_json::Value,
) -> Arc<dyn Backend> {
    let backend = built(bootstrap, prefix, extra).await;
    backend.start().await.expect("start");
    let probe = TopicPartition::new(
        GtsInstanceId::try_new(&format!(
            "{}cf.kafkatest.probe.t{}.v1",
            toolkit_gts::gts_id!("cf.core.events.topic.v1~"),
            &Uuid::new_v4().simple().to_string()[..12]
        ))
        .expect("valid id"),
        Partition::new(0),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    while let Err(BackendError::Unavailable { .. }) =
        backend.resolve(&probe, Position::Latest).await
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "still Unavailable a minute after start"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    backend
}
