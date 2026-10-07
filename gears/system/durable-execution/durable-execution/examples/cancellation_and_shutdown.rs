#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]
//! Provisioning: a long image build in an external builder process.
//!
//! Mechanism: cooperative cancellation and cleanup of work the activity owns.
//! - The user cancels a build: the activity kills its builder and the run becomes `Cancelled`.
//! - The host shuts down mid-build (deploy, scale-in): the builder is killed too, but the
//!   run returns to `Queued` so another worker picks it up. Shutdown is not a business
//!   cancellation.
//!
//! Dropping a future does not stop a subprocess; the activity must stop it itself.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL` and `DURABLE_EXAMPLE_SECRET`.
//! Run: `cargo run -p cf-gears-durable-execution --example cancellation_and_shutdown`
mod support;
use durable_execution_sdk::{
    observation::{RunState, RunStatus},
    prelude::*,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ImageBuild {
    image: String,
    base: String,
}

/// Argument that turns this executable into the stand-in image builder.
const BUILDER_MODE: &str = "--image-builder";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == BUILDER_MODE) {
        // A builder that never finishes on its own.
        std::future::pending::<()>().await;
        return Ok(());
    }
    support::run(async |app: &support::App| {
        use std::sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        };
        let builders_stopped = Arc::new(AtomicU32::new(0));
        let stopped = builders_stopped.clone();
        let builder_running = Arc::new(tokio::sync::Semaphore::new(0));
        let running = builder_running.clone();
        let workflow = WorkflowBuilder::<ImageBuild>::new("infra.build-image.v1")
            .then(support::step(
                "build_image",
                move |ctx, build: ImageBuild| {
                    let stopped = stopped.clone();
                    let running = running.clone();
                    async move {
                        let executable = std::env::current_exe()
                            .map_err(|_| ActivityError::permanent("builder_missing"))?;
                        let mut builder = tokio::process::Command::new(executable)
                            .arg(BUILDER_MODE)
                            .arg(&build.image)
                            .arg(&build.base)
                            .kill_on_drop(true)
                            .spawn()
                            .map_err(|_| ActivityError::retryable("builder_spawn_failed"))?;
                        running.add_permits(1);
                        // Fires on user cancellation and on host shutdown alike.
                        ctx.cancellation.cancelled().await;
                        builder
                            .kill()
                            .await
                            .map_err(|_| ActivityError::retryable("builder_cleanup_failed"))?;
                        stopped.fetch_add(1, Ordering::SeqCst);
                        Err::<String, _>(ActivityError::Cancelled)
                    }
                },
            ))
            .build()?;
        let reference = workflow.reference();
        app.registry.register(workflow.into_definition()).await?;
        let build = ImageBuild {
            image: "runner:2026.10".into(),
            base: "ubuntu:24.04".into(),
        };

        // 1. The user cancels the build from the UI.
        let first = app
            .client
            .start(
                &app.owner,
                &reference,
                build.clone(),
                StartOptions::default(),
            )
            .await?;
        let in_progress = app.wait_status(first.run_id, RunStatus::Running).await?;
        builder_running.acquire().await?.forget();
        app.client
            .cancel(
                &app.owner,
                first.run_id,
                CancelOptions {
                    expected_epoch: in_progress.epoch,
                    reason: Some("user cancelled image build".into()),
                },
            )
            .await?;
        assert!(matches!(
            app.wait(first.run_id).await?.state,
            RunState::Cancelled { .. }
        ));
        assert_eq!(builders_stopped.load(Ordering::SeqCst), 1);

        // 2. The worker host is shut down while another build runs.
        let second = app
            .client
            .start(&app.owner, &reference, build, StartOptions::default())
            .await?;
        app.wait_status(second.run_id, RunStatus::Running).await?;
        builder_running.acquire().await?.forget();
        app.shutdown().await?;
        let progress = app.client.get(&app.owner, second.run_id).await?;
        assert!(matches!(progress.state, RunState::Queued));
        assert_eq!(builders_stopped.load(Ordering::SeqCst), 2);
        Ok(())
    })
    .await
}
