//! Without `publisher = ...` the adapter is `::types_registry_sdk::publication`:
//! a gear crate without that dependency does not compile.
use toolkit_macros::gear;

pub fn gts_declarations() -> Vec<(String, String)> {
    Vec::new()
}

#[gear(name = "x", gts_publication(crates = [crate]))]
#[derive(Default)]
pub struct X;

#[async_trait::async_trait]
impl toolkit::Gear for X {
    async fn init(&self, _: &toolkit::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

fn main() {}
