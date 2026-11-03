//! The provider lives in the parent process and retains deduplication across worker deaths.
use super::repository::tests::{isolated_url, store_at};
use crate::{ExecutionOwner, prelude::*};
use crate::{
    catalog::registry::Registry,
    config::Config,
    engine::executor::Executor,
    state::{journal::Journal, persisted::RunStatus},
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;
use toolkit_security::AccessScope;

type Effects = Arc<parking_lot::Mutex<BTreeMap<String, u32>>>;

async fn provider_call(address: &str, key: &str, value: u32) -> u32 {
    let socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let (reader, mut writer) = socket.into_split();
    writer
        .write_all(format!("{}\n", serde_json::json!([key, value])).as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    BufReader::new(reader).read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

fn workflow(address: String, mode: String) -> crate::workflow::Workflow<Vec<u32>, ()> {
    let reverse_address = address.clone();
    let reverse_mode = mode.clone();
    WorkflowBuilder::<Vec<u32>>::new("process.provider-saga.v1")
        .saga()
        .for_each(
            Step::new("effect", Duration::from_secs(60), move |ctx, n: u32| {
                let address = address.clone();
                let mode = mode.clone();
                async move {
                    let value = provider_call(&address, ctx.idempotency_key(), n).await;
                    if mode == "forward-crash" {
                        std::future::pending::<()>().await;
                    }
                    Ok(value)
                }
            })
            .compensate(Compensation::new(
                Duration::from_secs(60),
                move |ctx, n: u32, receipt: u32| {
                    let address = reverse_address.clone();
                    let mode = reverse_mode.clone();
                    async move {
                        assert_eq!(receipt, n * 2);
                        provider_call(&address, ctx.idempotency_key(), n).await;
                        if mode == "reverse-crash" {
                            std::future::pending::<()>().await;
                        }
                        Ok(())
                    }
                },
            )),
            FanOutOptions::default().concurrency(2),
        )
        .then(Step::new(
            "reject",
            Duration::from_secs(2),
            |_, _: Vec<u32>| async { Err::<(), _>(ActivityError::permanent("business_rejected")) },
        ))
        .build()
        .unwrap()
}

struct Child(std::process::Child, std::path::PathBuf);
impl Drop for Child {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}
fn spawn(url: &str, queue: &str, address: &str, mode: &str) -> Child {
    let path = std::env::temp_dir().join(format!(
        "durable-provider-{}-{mode}.log",
        uuid::Uuid::new_v4()
    ));
    let output = std::fs::File::create(&path).unwrap();
    Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "store::provider_stages_tests::worker_child",
                "--nocapture",
            ])
            .env("DURABLE_PROVIDER_URL", url)
            .env("DURABLE_PROVIDER_QUEUE", queue)
            .env("DURABLE_PROVIDER_ADDRESS", address)
            .env("DURABLE_PROVIDER_MODE", mode)
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap(),
        path,
    )
}

#[tokio::test]
#[ignore = "subprocess entry point; the parent owns the provider and disposable database"]
async fn worker_child() {
    let url = std::env::var("DURABLE_PROVIDER_URL").unwrap();
    let queue = std::env::var("DURABLE_PROVIDER_QUEUE").unwrap();
    let registry = Arc::new(Registry::default());
    registry
        .register(
            workflow(
                std::env::var("DURABLE_PROVIDER_ADDRESS").unwrap(),
                std::env::var("DURABLE_PROVIDER_MODE").unwrap(),
            )
            .into_definition(),
        )
        .unwrap();
    let executor = Arc::new(Executor {
        store: store_at(&url).await,
        registry,
        config: Config {
            execute_activities: true,
            default_queue: queue.clone(),
            queues: [(queue, 2)].into(),
            lease_secs: 4,
            heartbeat_secs: 1,
            dispatch_interval_secs: 1,
            ..Default::default()
        },
    });
    crate::engine::runtime::Runtime::prepare(executor, &url)
        .await
        .unwrap()
        .run(CancellationToken::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn provider_deduplication_survives_forward_and_compensation_worker_crashes() {
    for mode in ["forward-crash", "reverse-crash"] {
        let database = isolated_url().await;
        let store = store_at(&database).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let effects: Effects = Arc::default();
        let saved = effects.clone();
        let reversals = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let reversed = reversals.clone();
        let provider = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let (reader, mut writer) = socket.into_split();
                let mut line = String::new();
                // A killed worker can reset a second connection before sending its request.
                // That connection commits no effect and must not terminate the provider.
                if !matches!(BufReader::new(reader).read_line(&mut line).await, Ok(bytes) if bytes > 0)
                {
                    continue;
                }
                let Ok((key, n)) = serde_json::from_str::<(String, u32)>(&line) else {
                    continue;
                };
                let value = {
                    let mut saved = saved.lock();
                    if !saved.contains_key(&key) && key.contains(":c/") {
                        reversed.lock().push(n);
                    }
                    *saved.entry(key).or_insert(n * 2)
                };
                // A killed worker may close its connection after the effect was committed.
                drop(writer.write_all(format!("{value}\n").as_bytes()).await);
            }
        });
        let definition = workflow(address.clone(), "recover".into()).into_definition();
        let journal = Journal::new(
            RunId(uuid::Uuid::new_v4()),
            ExecutionOwner {
                tenant_id: uuid::Uuid::new_v4(),
                subject_id: uuid::Uuid::new_v4(),
            },
            definition.contract(),
            serde_json::json!([0, 1, 2]),
            chrono::Utc::now(),
        )
        .unwrap();
        let id = journal.run.id;
        let scope = AccessScope::allow_all();
        store.insert(scope.clone(), journal).await.unwrap();
        let queue = format!("provider-{}", uuid::Uuid::new_v4());
        let mut first = spawn(&database, &queue, &address, mode);
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                assert!(
                    first.0.try_wait().unwrap().is_none(),
                    "worker exited before crash barrier"
                );
                let ready = if mode == "forward-crash" {
                    !effects.lock().is_empty()
                } else {
                    !reversals.lock().is_empty()
                };
                if ready {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        first.0.kill().unwrap();
        first.0.wait().unwrap();
        let mut replacement = spawn(&database, &queue, &address, "recover");
        let complete = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                assert!(
                    replacement.0.try_wait().unwrap().is_none(),
                    "replacement worker exited; diagnostic log: {}",
                    replacement.1.display()
                );
                let journal = store.get(&scope, id).await.unwrap().unwrap();
                if journal.run.status == RunStatus::Compensated {
                    break journal;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(effects.lock().len(), 6); // three forward and three reverse effects, even after replay
        assert_eq!(*reversals.lock(), [2, 1, 0]);
        assert_eq!(
            complete.execution.cause(),
            Some(&crate::state::execution::Cause::Failure(
                "business_rejected".into()
            ))
        );
        provider.abort();
        drop(provider.await);
    }
}
