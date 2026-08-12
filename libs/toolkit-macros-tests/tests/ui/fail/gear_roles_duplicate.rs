// `cpt-cf-adr-instance-addressable-discovery` §1: the declared `roles` set is a
// closed set of distinct role-qualified directory names. A repeated role name is
// a declaration bug, so the macro rejects it at compile time.

#[toolkit::gear(
    name = "event-broker",
    roles = ["event-broker", "event-broker-ingest", "event-broker-ingest"],
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
