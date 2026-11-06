use toolkit_macros::gear;

// `ready_warn_after` only bounds the readiness-signal wait, which exists only
// under `await_ready`; without it the value would silently expand to a no-op, so
// it must be a compile error.
#[gear(name="x", capabilities=[stateful], lifecycle(entry="serve", ready_warn_after="90s"))]
pub struct X;

fn main() {}
