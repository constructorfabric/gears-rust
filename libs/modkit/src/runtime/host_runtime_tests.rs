use super::*;
use crate::context::ModuleCtx;
use crate::contracts::{Module, RunnableCapability, SystemCapability};
use crate::registry::RegistryBuilder;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex;

#[derive(Default)]
#[allow(dead_code)]
struct DummyCore;
#[async_trait::async_trait]
impl Module for DummyCore {
    async fn init(&self, _ctx: &ModuleCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

struct StopOrderTracker {
    my_order: usize,
    stop_order: Arc<AtomicUsize>,
}

impl StopOrderTracker {
    fn new(counter: &Arc<AtomicUsize>, stop_order: Arc<AtomicUsize>) -> Self {
        let my_order = counter.fetch_add(1, Ordering::SeqCst);
        Self {
            my_order,
            stop_order,
        }
    }
}

#[async_trait::async_trait]
impl Module for StopOrderTracker {
    async fn init(&self, _ctx: &ModuleCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl RunnableCapability for StopOrderTracker {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
    async fn stop(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        let order = self.stop_order.fetch_add(1, Ordering::SeqCst);
        tracing::info!(
            my_order = self.my_order,
            stop_order = order,
            "Module stopped"
        );
        Ok(())
    }
}

#[tokio::test]
async fn test_stop_phase_reverse_order() {
    let counter = Arc::new(AtomicUsize::new(0));
    let stop_order = Arc::new(AtomicUsize::new(0));

    let module_a = Arc::new(StopOrderTracker::new(&counter, stop_order.clone()));
    let module_b = Arc::new(StopOrderTracker::new(&counter, stop_order.clone()));
    let module_c = Arc::new(StopOrderTracker::new(&counter, stop_order.clone()));

    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("a", &[], module_a.clone() as Arc<dyn Module>);
    builder.register_core_with_meta("b", &["a"], module_b.clone() as Arc<dyn Module>);
    builder.register_core_with_meta("c", &["b"], module_c.clone() as Arc<dyn Module>);

    builder.register_stateful_with_meta("a", module_a.clone() as Arc<dyn RunnableCapability>);
    builder.register_stateful_with_meta("b", module_b.clone() as Arc<dyn RunnableCapability>);
    builder.register_stateful_with_meta("c", module_c.clone() as Arc<dyn RunnableCapability>);

    let registry = builder.build_topo_sorted().unwrap();

    // Verify module order is a -> b -> c
    let module_names: Vec<_> = registry.modules().iter().map(|m| m.name).collect();
    assert_eq!(module_names, vec!["a", "b", "c"]);

    let client_hub = Arc::new(ClientHub::new());
    let cancel = CancellationToken::new();
    let config_provider: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

    let runtime = HostRuntime::new(
        registry,
        config_provider,
        DbOptions::None,
        client_hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );

    // Run stop phase
    runtime.run_stop_phase().await.unwrap();

    // Verify modules stopped in reverse order: c (stop_order=0), b (stop_order=1), a (stop_order=2)
    // Module order is: a=0, b=1, c=2
    // Stop order should be: c=0, b=1, a=2
    assert_eq!(stop_order.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_stop_phase_continues_on_error() {
    struct FailingModule {
        should_fail: bool,
        stopped: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Module for FailingModule {
        async fn init(&self, _ctx: &ModuleCtx) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl RunnableCapability for FailingModule {
        async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stop(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
            self.stopped.fetch_add(1, Ordering::SeqCst);
            if self.should_fail {
                anyhow::bail!("Intentional failure")
            }
            Ok(())
        }
    }

    let stopped = Arc::new(AtomicUsize::new(0));
    let module_a = Arc::new(FailingModule {
        should_fail: false,
        stopped: stopped.clone(),
    });
    let module_b = Arc::new(FailingModule {
        should_fail: true,
        stopped: stopped.clone(),
    });
    let module_c = Arc::new(FailingModule {
        should_fail: false,
        stopped: stopped.clone(),
    });

    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("a", &[], module_a.clone() as Arc<dyn Module>);
    builder.register_core_with_meta("b", &["a"], module_b.clone() as Arc<dyn Module>);
    builder.register_core_with_meta("c", &["b"], module_c.clone() as Arc<dyn Module>);

    builder.register_stateful_with_meta("a", module_a.clone() as Arc<dyn RunnableCapability>);
    builder.register_stateful_with_meta("b", module_b.clone() as Arc<dyn RunnableCapability>);
    builder.register_stateful_with_meta("c", module_c.clone() as Arc<dyn RunnableCapability>);

    let registry = builder.build_topo_sorted().unwrap();

    let client_hub = Arc::new(ClientHub::new());
    let cancel = CancellationToken::new();
    let config_provider: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

    let runtime = HostRuntime::new(
        registry,
        config_provider,
        DbOptions::None,
        client_hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );

    // Run stop phase - should not fail even though module_b fails
    runtime.run_stop_phase().await.unwrap();

    // All modules should have attempted to stop
    assert_eq!(stopped.load(Ordering::SeqCst), 3);
}

struct EmptyConfigProvider;
impl ConfigProvider for EmptyConfigProvider {
    fn get_module_config(&self, _module_name: &str) -> Option<&serde_json::Value> {
        None
    }
}

#[tokio::test]
async fn test_post_init_runs_after_all_init_and_system_first() {
    #[derive(Clone)]
    struct TrackHooks {
        name: &'static str,
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Module for TrackHooks {
        async fn init(&self, _ctx: &ModuleCtx) -> anyhow::Result<()> {
            self.events.lock().await.push(format!("init:{}", self.name));
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl SystemCapability for TrackHooks {
        fn pre_init(&self, _sys: &crate::runtime::SystemContext) -> anyhow::Result<()> {
            Ok(())
        }

        async fn post_init(&self, _sys: &crate::runtime::SystemContext) -> anyhow::Result<()> {
            self.events
                .lock()
                .await
                .push(format!("post_init:{}", self.name));
            Ok(())
        }
    }

    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let sys_a = Arc::new(TrackHooks {
        name: "sys_a",
        events: events.clone(),
    });
    let user_b = Arc::new(TrackHooks {
        name: "user_b",
        events: events.clone(),
    });
    let user_c = Arc::new(TrackHooks {
        name: "user_c",
        events: events.clone(),
    });

    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("sys_a", &[], sys_a.clone() as Arc<dyn Module>);
    builder.register_core_with_meta("user_b", &["sys_a"], user_b.clone() as Arc<dyn Module>);
    builder.register_core_with_meta("user_c", &["user_b"], user_c.clone() as Arc<dyn Module>);
    builder.register_system_with_meta("sys_a", sys_a.clone() as Arc<dyn SystemCapability>);

    let registry = builder.build_topo_sorted().unwrap();

    let client_hub = Arc::new(ClientHub::new());
    let cancel = CancellationToken::new();
    let config_provider: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

    let runtime = HostRuntime::new(
        registry,
        config_provider,
        DbOptions::None,
        client_hub,
        cancel,
        Uuid::new_v4(),
        None,
    );

    // Run init phase for all modules, then post_init as a separate barrier phase.
    runtime.run_init_phase().await.unwrap();
    runtime.run_post_init_phase().await.unwrap();

    let events = events.lock().await.clone();
    let first_post_init = events
        .iter()
        .position(|e| e.starts_with("post_init:"))
        .expect("expected post_init events");
    assert!(
        events[..first_post_init]
            .iter()
            .all(|e| e.starts_with("init:")),
        "expected all init events before post_init, got: {events:?}"
    );

    // system-first order within each phase
    assert_eq!(
        events,
        vec![
            "init:sys_a",
            "init:user_b",
            "init:user_c",
            "post_init:sys_a",
        ]
    );
}
