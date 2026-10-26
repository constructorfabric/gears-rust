use toolkit_macros::gear;

#[gear(name="x", capabilities=[stateful], one_per_installation=true, one_per_installation=false)]
pub struct X;

fn main() {}
