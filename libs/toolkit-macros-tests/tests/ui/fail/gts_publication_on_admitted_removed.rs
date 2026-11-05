use toolkit_macros::gear;

#[gear(name = "x", gts_publication(crates = [crate], on_admitted = Self::bind))]
pub struct X;

fn main() {}
