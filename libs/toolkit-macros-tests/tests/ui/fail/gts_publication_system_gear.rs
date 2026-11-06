use toolkit_macros::gear;

#[gear(
    name = "x",
    capabilities = [system],
    gts_publication(crates = [crate], publisher = some::adapter)
)]
pub struct X;

fn main() {}
