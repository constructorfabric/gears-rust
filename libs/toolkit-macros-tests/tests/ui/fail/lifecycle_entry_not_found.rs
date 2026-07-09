use toolkit::gear;
use tokio_util::sync::CancellationToken;
use anyhow::Result;

#[gear(name="x", capabilities=[stateful], lifecycle(entry="serve", await_ready))]
pub struct X;

impl X {
    // Wrong signature: missing ReadySignal parameter → the generated call won't match.
    async fn serve(&self, _cancel: CancellationToken) -> Result<()> { Ok(()) }
}

#[async_trait::async_trait]
impl toolkit::Gear for X {
    async fn init(&self, _ctx: &toolkit::GearCtx) -> anyhow::Result<()> { Ok(()) }
}

fn main() {}
