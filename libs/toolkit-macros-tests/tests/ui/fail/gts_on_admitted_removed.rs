use toolkit_macros::gear;

#[gear(name = "x", gts(crates = [crate], on_admitted = Self::bind))]
pub struct X;

fn main() {}
