#![allow(clippy::unwrap_used, clippy::expect_used)]

use anyhow::Result;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use toolkit::{RunnableCapability, lifecycle as lifecycle_attr, lifecycle::*};

struct ReadyAware;

#[lifecycle_attr(method = "run_with_ready", stop_timeout = "200ms", await_ready = true)]
impl ReadyAware {
    pub async fn run_with_ready(
        &self,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> Result<()> {
        // Signal readiness only after a small delay to keep state in Starting
        tokio::time::sleep(Duration::from_millis(20)).await;
        ready.notify();
        // Then run until cancelled
        cancel.cancelled().await;
        Ok(())
    }
}

struct AutoNotify;

#[lifecycle_attr(method = "run_no_ready", await_ready = true)]
impl AutoNotify {
    pub async fn run_no_ready(&self, cancel: CancellationToken) -> Result<()> {
        // Just wait until cancelled
        cancel.cancelled().await;
        Ok(())
    }
}

#[tokio::test]
async fn stays_starting_until_ready_signal() {
    let m = ReadyAware.into_gear();
    let parent = CancellationToken::new();
    m.start(parent.clone()).await.unwrap();

    // Should be Starting until ReadySignal triggers inside the method
    assert_eq!(m.status(), Status::Starting);
    tokio::time::timeout(Duration::from_millis(500), async {
        while !matches!(m.status(), Status::Running) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("gear did not become Running within 500 ms");

    parent.cancel();
    m.stop(CancellationToken::new()).await.unwrap();
    assert_eq!(m.status(), Status::Stopped);
}

#[tokio::test]
async fn auto_notify_when_no_ready_param() {
    let m = AutoNotify.into_gear();
    let parent = CancellationToken::new();

    m.start(parent.clone()).await.unwrap();
    // await_ready=true, but the method has no ReadySignal parameter,
    // so readiness must be reported automatically.
    tokio::time::timeout(Duration::from_millis(500), async {
        while !matches!(m.status(), Status::Running) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("gear did not become Running after automatic readiness notification");

    parent.cancel();
    m.stop(CancellationToken::new()).await.unwrap();
    assert_eq!(m.status(), Status::Stopped);
}

#[tokio::test]
async fn drop_cleans_up_background_task() {
    let parent = CancellationToken::new();
    let handle = tokio::spawn(async move {
        let m = AutoNotify.into_gear();
        m.start(parent.clone()).await.unwrap();
        // Drop without explicit stop(); background task should be aborted/cancelled
        m
    });

    // Wait for the task to finish and drop
    let m = handle.await.unwrap();
    drop(m);
    // Nothing to assert directly; this test exercises Drop paths without hanging.
}
