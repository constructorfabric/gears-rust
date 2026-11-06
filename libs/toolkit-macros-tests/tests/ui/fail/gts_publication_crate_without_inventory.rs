//! A listed crate must declare its inventory even when it has no `local` macros.
//! `toolkit` is an existing dependency without `declare_gts_inventory!()`.
#[path = "../../common/fake_publisher.rs"]
mod publisher;

#[toolkit::gear(
    name = "missing-inventory",
    gts_publication(crates = [toolkit], publisher = publisher)
)]
#[derive(Default)]
pub struct MissingInventory;

#[async_trait::async_trait]
impl toolkit::Gear for MissingInventory {
    async fn init(&self, _: &toolkit::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

fn main() {}
