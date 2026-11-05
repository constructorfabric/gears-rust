use toolkit_macros::gear;

// The startup group follows the `system` capability; there is no `stage`.
#[gear(name = "x", stage = application)]
pub struct X;

fn main() {}
