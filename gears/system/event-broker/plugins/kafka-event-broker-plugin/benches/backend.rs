//! The backend bench against Kafka: the cluster `EB_BENCH_KAFKA_BOOTSTRAP`
//! names, or a single-node container on `libs/test-containers`' pinned tag.

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use event_broker_backend_bench::{Harness, Settings, Sizing, run_bench};
use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use kafka_event_broker_plugin::KafkaBackendProvider;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::kafka::apache::{KAFKA_PORT, Kafka};
use uuid::Uuid;

/// Every build reaches one prefix of one cluster with the plugin's default
/// settings but the replication a single broker allows: the bench measures
/// the plugin as configured out of the box.
struct KafkaHarness {
    bootstrap: String,
    prefix: String,
    /// Replicas of the bench's topics: 1 on the container, the default on a
    /// named cluster.
    replication: Option<u16>,
    _container: Option<ContainerAsync<Kafka>>,
}

impl KafkaHarness {
    async fn start() -> Result<Self, String> {
        let prefix = format!("evbk-bench{}", &Uuid::new_v4().simple().to_string()[..8]);
        if let Ok(bootstrap) = std::env::var("EB_BENCH_KAFKA_BOOTSTRAP") {
            return Ok(Self {
                bootstrap,
                prefix,
                replication: None,
                _container: None,
            });
        }
        let container = test_containers::kafka()
            .with_container_name(format!(
                "eb-kafka-bench-{}",
                &Uuid::new_v4().simple().to_string()[..6]
            ))
            .start()
            .await
            .map_err(|err| format!("kafka container did not start: {err}"))?;
        let port = container
            .get_host_port_ipv4(KAFKA_PORT)
            .await
            .map_err(|err| format!("kafka container port: {err}"))?;
        Ok(Self {
            bootstrap: format!("127.0.0.1:{port}"),
            prefix,
            replication: Some(1),
            _container: Some(container),
        })
    }
}

#[async_trait]
impl Harness for KafkaHarness {
    async fn build(&self, sizing: Sizing) -> Arc<dyn Backend> {
        let mut settings = serde_json::json!({
            "bootstrap_servers": self.bootstrap,
            "topic_prefix": self.prefix,
            "consumer_pool": sizing.connections,
        });
        if let (Some(replicas), serde_json::Value::Object(map)) = (self.replication, &mut settings)
        {
            map.insert("replication_factor".to_owned(), replicas.into());
            map.insert("min_insync_replicas".to_owned(), replicas.into());
        }
        let serde_json::Value::Object(settings) = settings else {
            unreachable!("an object literal");
        };
        match KafkaBackendProvider::default()
            .build(&BuildContext::new(&settings, Role::Standalone))
            .await
        {
            Ok(backend) => backend,
            Err(err) => {
                panic!("the kafka backend builds from bootstrap servers and a prefix: {err}")
            }
        }
    }

    fn name(&self) -> &'static str {
        "kafka"
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let settings = Settings::parse_from(std::env::args());
    match KafkaHarness::start().await {
        Ok(harness) => run_bench(&harness, &settings).await,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
