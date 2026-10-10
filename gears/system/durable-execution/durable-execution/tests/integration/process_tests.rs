//! Worker-process termination tests with deterministic file effects and checkpoint gates.
use super::repository::tests::{definition, isolated_url, journal, store_at};
use crate::domain::persisted::*;
use async_trait::async_trait;
use durable_execution_sdk::contracts::{ActivityInput, ErasedActivity};
use durable_execution_sdk::registration::{RegistrationState, UnregisterMode, UnregisterOptions};
use durable_execution_sdk::*;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use toolkit_security::AccessScope;

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}
#[expect(
    clippy::unwrap_used,
    reason = "Fixture setup must fail the test immediately if an invariant or dependency is unavailable."
)]
fn spawn(url: &str, queue: &str, mode: &str) -> Child {
    Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "infra::storage::process_tests::worker_child",
                "--nocapture",
            ])
            .env("DURABLE_CHILD_URL", url)
            .env("DURABLE_TEST_PG_URL", url)
            .env("DURABLE_CHILD_QUEUE", queue)
            .env("DURABLE_CHILD_MODE", mode)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    )
}
struct EffectStep(String);
#[async_trait]
impl ErasedActivity for EffectStep {
    #[expect(
        clippy::unwrap_used,
        reason = "Fixture setup must fail the test immediately if an invariant or dependency is unavailable."
    )]
    async fn execute(
        &self,
        context: ActivityContext,
        input: ActivityInput,
    ) -> Result<serde_json::Value, ActivityError> {
        if context.activity_id.0 == "one" {
            use std::io::Write;
            let path = input.run_input["marker"].as_str().unwrap();
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(file, "effect").unwrap();
            file.sync_all().unwrap();
            if self.0 == "before-checkpoint" {
                std::future::pending::<()>().await;
            }
        } else if self.0 == "after-checkpoint" {
            std::future::pending::<()>().await;
        }
        Ok(serde_json::Value::Null)
    }
}
#[tokio::test]
#[ignore = "subprocess entry point; only the parent test supplies its environment"]
async fn worker_child() {
    let url = std::env::var("DURABLE_CHILD_URL")
        .expect("worker_child is a fixture entry point; invoke only through the parent test");
    let store = store_at(&url).await;
    let mut d = definition();
    for a in &mut d.activities {
        a.handler = Arc::new(EffectStep(std::env::var("DURABLE_CHILD_MODE").unwrap()));
    }
    let registry = Arc::new(crate::domain::registry::Registry::default());
    registry.register(d).unwrap();
    let executor = Arc::new(crate::infra::executor::Executor {
        store,
        registry,
        config: crate::config::Config {
            execute_activities: true,
            default_queue: std::env::var("DURABLE_CHILD_QUEUE").unwrap(),
            queues: [(std::env::var("DURABLE_CHILD_QUEUE").unwrap(), 2)].into(),
            service_client_id: "test-worker".into(),
            lease_secs: 4,
            heartbeat_secs: 1,
            dispatch_interval_secs: 1,
            ..Default::default()
        },
    });
    let runtime = crate::infra::runtime::Runtime::prepare(executor, &url)
        .await
        .unwrap();
    runtime.run(CancellationToken::new()).await.unwrap();
}
#[tokio::test]
async fn killed_worker_recovers_effects_and_preserves_successful_checkpoint() {
    for (mode, expected_effects) in [("before-checkpoint", 2), ("after-checkpoint", 1)] {
        let url = isolated_url().await;
        let store = store_at(&url).await;
        let queue = format!("process-{}", uuid::Uuid::new_v4());
        let marker =
            std::env::temp_dir().join(format!("durable-effect-{}.txt", uuid::Uuid::new_v4()));
        let mut j = journal();
        j.input = serde_json::json!({"marker": marker});
        let id = j.run.id;
        let scope = AccessScope::allow_all();
        // Commit with the producer pipeline stopped: the child must discover
        // the persisted Outbox command without a notification from this process.
        store.insert(scope.clone(), j).await.unwrap();
        let mut first = spawn(&url, &queue, mode);
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                assert!(
                    first.0.try_wait().unwrap().is_none(),
                    "fixture worker exited early"
                );
                let j = store.get(&scope, id).await.unwrap().unwrap();
                if mode == "before-checkpoint"
                    && std::fs::read_to_string(&marker).is_ok_and(|s| s.lines().count() == 1)
                {
                    break;
                }
                if mode == "after-checkpoint"
                    && j.run.activities[1].status == ActivityStatus::Running
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        first.0.kill().unwrap();
        first.0.wait().unwrap();
        let _replacement = spawn(&url, &queue, "recover");
        let finished = tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let j = store.get(&scope, id).await.unwrap().unwrap();
                if j.run.status == RunStatus::Succeeded {
                    break j;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().lines().count(),
            expected_effects
        );
        assert_eq!(
            finished.run.activities[0].attempts,
            u32::try_from(expected_effects).unwrap()
        );
        if mode == "after-checkpoint" {
            assert_eq!(finished.run.activities[1].attempts, 2);
        }
        std::fs::remove_file(marker).unwrap();
    }
}

#[tokio::test]
async fn killed_worker_in_stopping_recovers_release_without_automatic_reactivation() {
    let url = isolated_url().await;
    let store = store_at(&url).await;
    let queue = format!("stopping-{}", uuid::Uuid::new_v4());
    let marker =
        std::env::temp_dir().join(format!("durable-stopping-{}.txt", uuid::Uuid::new_v4()));
    let mut j = journal();
    j.input = serde_json::json!({"marker":marker});
    let id = j.run.id;
    store.insert(AccessScope::allow_all(), j).await.unwrap();
    let mut worker = spawn(&url, &queue, "before-checkpoint");
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            assert!(worker.0.try_wait().unwrap().is_none());
            if marker.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let registrar = crate::infra::registrar::Registrar {
        store: store.clone(),
        registry: Arc::new(crate::domain::registry::Registry::default()),
    };
    let registration = registrar.registration("test.store.v1").await.unwrap();
    registrar
        .unregister(
            &registration.name,
            UnregisterOptions {
                mode: UnregisterMode::CancelAndRelease,
                expected_revision: registration.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        registrar
            .registration(&registration.name)
            .await
            .unwrap()
            .state,
        RegistrationState::Stopping
    );
    worker.0.kill().unwrap();
    worker.0.wait().unwrap();
    let _replacement = spawn(&url, &queue, "recover");
    let released = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let value = registrar.registration(&registration.name).await.unwrap();
            if value.state == RegistrationState::Released {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let j = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(j.run.status, RunStatus::Cancelled);
    assert!(j.run.activities[0].result.is_none());
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    let rebound = registrar.register(definition()).await.unwrap();
    assert_eq!(rebound, released);
    let next = registrar
        .activate(&registration.name, released.revision)
        .await
        .unwrap();
    assert_eq!(next.generation, 1);
    std::fs::remove_file(marker).unwrap();
}
