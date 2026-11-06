use toolkit_macros::gear;

#[gear(name = "x", gts_publication(crates = [crate], crates = [crate], publisher = some::adapter))]
pub struct X;

fn main() {}
