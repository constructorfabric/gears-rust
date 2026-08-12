// `cpt-cf-adr-instance-addressable-discovery` §1: a role-split gear declares its closed set of role-qualified
// directory names via `roles = [...]`. The macro emits a `ROLES` associated
// const and self-registers the set into the `DeclaredRoles` inventory, so OoP
// bootstrap enforces the role/front-door split without `main.rs` wiring. The
// bare `name` must be one of the roles.

#[toolkit::gear(
    name = "event-broker", // front-door role (must be a member of `roles`)
    roles = ["event-broker", "event-broker-ingest", "event-broker-delivery"],
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

fn main() {
    // The declared closed set is available as an associated const.
    assert_eq!(
        EventBrokerGear::ROLES,
        ["event-broker", "event-broker-ingest", "event-broker-delivery"]
    );
}
