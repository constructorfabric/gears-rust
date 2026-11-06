//! Repeating a collector in a single `crates` list is a compile-time error.
use toolkit_macros::gear;

#[gear(name = "x", gts_publication(crates = [some_crate, some_crate], publisher = some::adapter))]
pub struct DuplicateCrate;

fn main() {}
