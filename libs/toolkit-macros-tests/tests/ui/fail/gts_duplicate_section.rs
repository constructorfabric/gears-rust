use toolkit_macros::gear;

#[gear(
    name = "x",
    gts(crates = [crate], publisher = some::adapter),
    gts(crates = [crate], publisher = some::adapter)
)]
pub struct X;

fn main() {}
