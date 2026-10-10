#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]
//! Provisioning: create a VM through a flaky cloud API with operator recovery.
//!
//! Mechanism:
//! - `create_vm` hits cloud throttling once; the step's `RetryPolicy` retries it automatically.
//! - `attach_volume` fails permanently on an exhausted quota. The run is `Failed` and an
//!   operator calls Retry with the observed epoch.
//! - The retried attempt is paused for a maintenance window (Cancel), the quota is raised,
//!   and Resume finishes the run.
//!
//! The network allocated in the first step is a checkpoint and is never allocated again.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL` and `DURABLE_EXAMPLE_SECRET`.
//! Run: `cargo run -p cf-gears-durable-execution --example retries_and_resume`
mod support;
use durable_execution_sdk::{observation::RunState, prelude::*};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct VmRequest {
    name: String,
    size_gb: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct NetworkLease {
    subnet: String,
    vm: VmRequest,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct VmInstance {
    instance_id: String,
    subnet: String,
    size_gb: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct VmHandle {
    volume_id: String,
    instance_id: String,
    subnet: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU32, Ordering},
        };
        let quota_exhausted = Arc::new(AtomicBool::new(true));
        let cloud_calls = Arc::new(AtomicU32::new(0));
        let networks_allocated = Arc::new(AtomicU32::new(0));
        let (create_calls, quota, allocations) = (
            cloud_calls.clone(),
            quota_exhausted.clone(),
            networks_allocated.clone(),
        );
        let builder = WorkflowBuilder::<VmRequest>::new("infra.provision-vm.v1").then(
            support::step("allocate_network", move |_, vm: VmRequest| {
                let allocations = allocations.clone();
                async move {
                    allocations.fetch_add(1, Ordering::SeqCst);
                    Ok(NetworkLease {
                        subnet: format!("10.0.7.0/24 for {}", vm.name),
                        vm,
                    })
                }
            }),
        );
        let network = builder.checkpoint();
        let workflow = builder
            .then(
                support::step("create_vm", move |_, lease: NetworkLease| {
                    let calls = create_calls.clone();
                    async move {
                        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                            // HTTP 429 from the cloud API: transient, retry later.
                            return Err(ActivityError::retryable("cloud_throttled"));
                        }
                        Ok(VmInstance {
                            instance_id: format!("i-{}", lease.vm.name),
                            subnet: lease.subnet,
                            size_gb: lease.vm.size_gb,
                        })
                    }
                })
                .retry(RetryPolicy {
                    max_attempts: 2,
                    initial_delay_secs: 1,
                    max_delay_secs: 1,
                }),
            )
            .then(support::step(
                "attach_volume",
                move |ctx, vm: VmInstance| {
                    let quota = quota.clone();
                    async move {
                        if ctx.execution_epoch == 1 {
                            // Long attach after Retry; the operator pauses it for maintenance.
                            ctx.cancellation.cancelled().await;
                            return Err(ActivityError::Cancelled);
                        }
                        if quota.load(Ordering::SeqCst) {
                            // Retrying cannot help until someone raises the quota.
                            return Err(ActivityError::permanent("quota_exceeded"));
                        }
                        Ok(VmHandle {
                            volume_id: format!("vol-{}-{}gb", vm.instance_id, vm.size_gb),
                            instance_id: vm.instance_id,
                            subnet: vm.subnet,
                        })
                    }
                },
            ))
            .build()?;
        let reference = workflow.reference();
        app.registry.register(workflow.into_definition()).await?;

        let started = app
            .client
            .start(
                &app.owner,
                &reference,
                VmRequest {
                    name: "build-agent-7".into(),
                    size_gb: 100,
                },
                StartOptions::default(),
            )
            .await?;
        let failed = app.wait(started.run_id).await?;
        assert!(matches!(failed.state, RunState::Failed { .. }));

        // Operator: "quota ticket filed, try again".
        let retried = app
            .client
            .retry(
                &app.owner,
                started.run_id,
                ContinueOptions {
                    expected_epoch: failed.epoch,
                },
            )
            .await?;
        app.client
            .cancel(
                &app.owner,
                started.run_id,
                CancelOptions {
                    expected_epoch: retried.epoch,
                    reason: Some("storage maintenance window".into()),
                },
            )
            .await?;
        let paused = app.wait(started.run_id).await?;
        assert!(matches!(paused.state, RunState::Cancelled { .. }));

        // Maintenance is over and the quota was raised.
        quota_exhausted.store(false, Ordering::SeqCst);
        app.client
            .resume(
                &app.owner,
                started.run_id,
                ContinueOptions {
                    expected_epoch: paused.epoch,
                },
            )
            .await?;
        let finished = app.wait(started.run_id).await?;
        assert!(matches!(finished.state, RunState::Succeeded { .. }));

        assert_eq!(finished.steps[0].attempts, 1);
        assert_eq!(networks_allocated.load(Ordering::SeqCst), 1);
        assert_eq!(cloud_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            app.client
                .step_result(&app.owner, started.run_id, &network)
                .await?
                .subnet,
            "10.0.7.0/24 for build-agent-7"
        );
        assert_eq!(
            app.client
                .result(&app.owner, started.run_id, &reference)
                .await?,
            VmHandle {
                volume_id: "vol-i-build-agent-7-100gb".into(),
                instance_id: "i-build-agent-7".into(),
                subnet: "10.0.7.0/24 for build-agent-7".into(),
            }
        );
        Ok(())
    })
    .await
}
