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

#[derive(Default)]
struct ConfiguredStop {
    seconds: std::sync::atomic::AtomicU64,
    finished: std::sync::atomic::AtomicBool,
    started: tokio::sync::Notify,
}

#[lifecycle_attr(method = "serve", stop_timeout_fn = "shutdown_timeout")]
impl ConfiguredStop {
    fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.seconds.load(std::sync::atomic::Ordering::SeqCst))
    }

    async fn serve(&self, cancel: CancellationToken) -> Result<()> {
        self.started.notify_one();
        cancel.cancelled().await;
        tokio::time::sleep(Duration::from_secs(40)).await;
        self.finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_provider_reads_initialized_configuration_at_stop() {
    let wrapper = ConfiguredStop::default().into_gear();
    wrapper
        .inner()
        .seconds
        .store(60, std::sync::atomic::Ordering::SeqCst);
    wrapper.start(CancellationToken::new()).await.unwrap();
    wrapper.inner().started.notified().await;
    let began = tokio::time::Instant::now();
    wrapper.stop(CancellationToken::new()).await.unwrap();
    assert_eq!(began.elapsed(), Duration::from_secs(40));
    assert!(
        wrapper
            .inner()
            .finished
            .load(std::sync::atomic::Ordering::SeqCst)
    );
}

#[tokio::test(start_paused = true)]
async fn host_deadline_overrides_computed_timeout() {
    let wrapper = ConfiguredStop::default().into_gear();
    wrapper
        .inner()
        .seconds
        .store(60, std::sync::atomic::Ordering::SeqCst);
    wrapper.start(CancellationToken::new()).await.unwrap();
    wrapper.inner().started.notified().await;
    let deadline = CancellationToken::new();
    let token = deadline.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        token.cancel();
    });
    let began = tokio::time::Instant::now();
    wrapper.stop(deadline).await.unwrap();
    assert_eq!(began.elapsed(), Duration::from_secs(5));
    assert!(
        !wrapper
            .inner()
            .finished
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    assert_eq!(wrapper.status(), Status::Stopped);
}

static GEAR_CLEANED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static GEAR_STARTED: tokio::sync::Notify = tokio::sync::Notify::const_new();

#[toolkit::gear(name = "configured-stop-test", capabilities = [stateful], lifecycle(entry = "serve", stop_timeout_fn = "shutdown_timeout", await_ready))]
struct ConfiguredStopGear {
    timeout: Duration,
}

impl Default for ConfiguredStopGear {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
        }
    }
}

impl ConfiguredStopGear {
    fn shutdown_timeout(&self) -> Duration {
        self.timeout
    }
    async fn serve(&self, cancel: CancellationToken, ready: ReadySignal) -> Result<()> {
        ready.notify();
        GEAR_STARTED.notify_one();
        cancel.cancelled().await;
        tokio::time::sleep(Duration::from_secs(40)).await;
        GEAR_CLEANED.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}
#[async_trait::async_trait]
impl toolkit::Gear for ConfiguredStopGear {
    async fn init(&self, _: &toolkit::GearCtx) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn gear_macro_uses_computed_timeout_in_wrapper_and_registered_capability() {
    let mut builder = toolkit::registry::RegistryBuilder::default();
    __configured_stop_gear_registrator(&mut builder);
    let registry = builder.build_topo_sorted().unwrap();
    let registered = registry
        .gears()
        .iter()
        .find(|entry| entry.name() == "configured-stop-test")
        .unwrap()
        .caps()
        .query::<toolkit::registry::RunnableCap>()
        .unwrap();
    let wrapped: std::sync::Arc<dyn RunnableCapability> =
        std::sync::Arc::new(ConfiguredStopGear::default().into_gear());
    for lifecycle in [wrapped, registered] {
        GEAR_CLEANED.store(false, std::sync::atomic::Ordering::SeqCst);
        lifecycle.start(CancellationToken::new()).await.unwrap();
        GEAR_STARTED.notified().await;
        let began = tokio::time::Instant::now();
        lifecycle.stop(CancellationToken::new()).await.unwrap();
        assert_eq!(began.elapsed(), Duration::from_secs(40));
        assert!(GEAR_CLEANED.load(std::sync::atomic::Ordering::SeqCst));
    }
}
