//! Configured queue routing/concurrency through SDK, journal, Outbox and real workers.
use super::*;
use crate::{
    domain::registry::Registry,
    infra::{executor::Executor, service::Service, storage::repository::tests::store},
};
use authz_resolver_sdk::PolicyEnforcer;
use durable_execution_sdk::contracts::{
    ActivityDefinition, ActivityInput, ErasedActivity, ExecutionDefinition,
};
use durable_execution_sdk::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_security::SecurityContext;

struct Gated {
    label: &'static str,
    gate: Arc<tokio::sync::Semaphore>,
    started: tokio::sync::mpsc::UnboundedSender<&'static str>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl ErasedActivity for Gated {
    #[expect(
        clippy::unwrap_used,
        reason = "Fixture setup must fail the test immediately if an invariant or dependency is unavailable."
    )]
    async fn execute(
        &self,
        ctx: ActivityContext,
        _: ActivityInput,
    ) -> Result<serde_json::Value, ActivityError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let _active = Active(self.active.clone());
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.started.send(self.label).unwrap();
        tokio::select! {
            permit=self.gate.acquire() => permit.unwrap().forget(),
            ()=ctx.cancellation.cancelled() => return Err(ActivityError::Cancelled),
        }
        Ok(serde_json::json!(self.label))
    }
}
#[expect(
    clippy::unwrap_used,
    reason = "Fixture setup must fail the test immediately if an invariant or dependency is unavailable."
)]
async fn observed(rx: &mut tokio::sync::mpsc::UnboundedReceiver<&'static str>) -> &'static str {
    tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn sdk_routes_named_and_unknown_versions_and_enforces_each_configured_concurrency() {
    let url = crate::test_postgres::database().await;
    let store = store().await;
    let registry = Arc::new(Registry::default());
    let tenant = uuid::Uuid::new_v4();
    let ctx = SecurityContext::builder()
        .subject_id(uuid::Uuid::new_v4())
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap();
    let (started, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let slow_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let fast_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let slow_active = Arc::new(AtomicUsize::new(0));
    let fast_active = Arc::new(AtomicUsize::new(0));
    let slow_peak = Arc::new(AtomicUsize::new(0));
    let fast_peak = Arc::new(AtomicUsize::new(0));
    for (name, label, gate, active, peak) in [
        (
            "queue.slow.v1",
            "slow",
            slow_gate.clone(),
            slow_active.clone(),
            slow_peak.clone(),
        ),
        (
            "queue.fast.v1",
            "fast",
            fast_gate.clone(),
            fast_active.clone(),
            fast_peak.clone(),
        ),
        (
            "queue.fast.v2",
            "fallback",
            slow_gate.clone(),
            slow_active.clone(),
            slow_peak.clone(),
        ),
    ] {
        crate::infra::registrar::Registrar {
            store: store.clone(),
            registry: registry.clone(),
        }
        .register(ExecutionDefinition {
            name: name.into(),
            parallel_groups: vec![],
            activities: vec![ActivityDefinition {
                id: ActivityId("work".into()),
                timeout: Duration::from_secs(60),
                retry: RetryPolicy::default(),
                handler: Arc::new(Gated {
                    label,
                    gate,
                    started: started.clone(),
                    active,
                    peak,
                }),
            }],

            flow: None,
        })
        .await
        .unwrap();
    }
    let slow = format!("slow-{}", uuid::Uuid::new_v4());
    let fast = format!("fast-{}", uuid::Uuid::new_v4());
    let config = crate::config::Config {
        execute_activities: true,
        default_queue: slow.clone(),
        queues: [(slow, 1), (fast.clone(), 2)].into(),
        definition_queues: [("queue.fast.v1".into(), fast)].into(),
        service_client_id: "fixture".into(),
        dispatch_interval_secs: 1,
        shutdown_timeout_secs: 10,
        ..Default::default()
    };
    let runtime = Runtime::prepare(
        Arc::new(Executor {
            store: store.clone(),
            registry: registry.clone(),
            config,
        }),
        &url,
    )
    .await
    .unwrap();
    let hub = toolkit::ClientHub::new();
    hub.register::<dyn DurableExecution>(Arc::new(Service {
        store: store.clone(),
        enforcer: PolicyEnforcer::new(Arc::new(crate::gear::tests::TenantPolicy(tenant))),
    }));
    let sdk = DurableExecutionClient::resolve(&hub).unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(runtime.run(stop.clone()));
    let first = sdk
        .raw()
        .start(
            &ctx,
            "queue.slow.v1",
            serde_json::Value::Null,
            StartOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(observed(&mut rx).await, "slow");
    let fallback = sdk
        .raw()
        .start(
            &ctx,
            "queue.fast.v2",
            serde_json::Value::Null,
            StartOptions::default(),
        )
        .await
        .unwrap();
    let mut ids = vec![first.run_id, fallback.run_id];
    for _ in 0..3 {
        ids.push(
            sdk.raw()
                .start(
                    &ctx,
                    "queue.fast.v1",
                    serde_json::Value::Null,
                    StartOptions::default(),
                )
                .await
                .unwrap()
                .run_id,
        );
    }
    assert_eq!(observed(&mut rx).await, "fast");
    assert_eq!(observed(&mut rx).await, "fast");
    // Hold all slots for more than one production polling interval.
    let unexpected = tokio::time::timeout(Duration::from_secs(6), rx.recv()).await;
    assert!(
        unexpected.is_err(),
        "N+1 task or fallback bypassed configured capacity: {unexpected:?}"
    );
    assert_eq!(slow_active.load(Ordering::SeqCst), 1);
    assert_eq!(fast_active.load(Ordering::SeqCst), 2);
    fast_gate.add_permits(1);
    assert_eq!(observed(&mut rx).await, "fast");
    fast_gate.add_permits(2);
    slow_gate.add_permits(1);
    assert_eq!(observed(&mut rx).await, "fallback");
    slow_gate.add_permits(1);
    let completed = tokio::time::timeout(Duration::from_secs(20), async {
        for id in ids {
            loop {
                let run = sdk.get(&ctx, id).await.unwrap();
                if run.state.status() == durable_execution_sdk::observation::RunStatus::Succeeded {
                    assert_eq!(run.steps[0].attempts, 1);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    })
    .await;
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    completed.unwrap();
    assert_eq!(slow_peak.load(Ordering::SeqCst), 1);
    assert_eq!(fast_peak.load(Ordering::SeqCst), 2);
}
