use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
#[toolkit_macros::gear(name = "dynamic-timeout", capabilities = [stateful], lifecycle(entry = "serve", stop_timeout_fn = "shutdown_timeout", await_ready))]
pub struct DynamicTimeout;

impl DynamicTimeout {
    fn shutdown_timeout(&self) -> Duration { Duration::from_secs(60) }
    async fn serve(&self, cancel: CancellationToken, ready: toolkit::lifecycle::ReadySignal) -> anyhow::Result<()> {
        ready.notify();
        cancel.cancelled().await;
        Ok(())
    }
}

#[async_trait::async_trait]
impl toolkit::Gear for DynamicTimeout {
    async fn init(&self, _: &toolkit::GearCtx) -> anyhow::Result<()> { Ok(()) }
}

fn main() { let _wrapper = DynamicTimeout.into_gear(); }
