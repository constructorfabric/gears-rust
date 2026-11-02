use toolkit_macros::gear;

#[gear(name = "x", gts(crates = [crate], publisher = some::adapter, retry = 3))]
pub struct X;

fn main() {}
