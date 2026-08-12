// `cpt-cf-adr-instance-addressable-discovery` §1: when a gear declares `roles`, the bare `name` (the front door)
// MUST be one of them. Here `name` is absent from `roles`, so the macro rejects
// it at compile time.

#[toolkit::gear(
    name = "event-broker",
    roles = ["event-broker-ingest", "event-broker-delivery"],
    capabilities = []
)]
#[derive(Default)]
pub struct EventBrokerGear;

#[async_trait::async_trait]
impl toolkit::Gear for EventBrokerGear {
    async fn init(&self, _ctx: &toolkit::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

fn main() {}
