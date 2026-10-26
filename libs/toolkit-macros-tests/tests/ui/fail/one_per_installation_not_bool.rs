use toolkit_macros::gear;

#[gear(name="x", capabilities=[stateful], one_per_installation="true")]
pub struct X;

fn main() {}
