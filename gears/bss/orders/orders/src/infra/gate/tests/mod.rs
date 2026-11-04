//! In-crate gate tests. They run in the `orders-pg` CI lane with `--features gate-dev-stubs` (D-244).
mod authority;
mod budget;
pub mod doubles;
mod failure;
#[cfg(feature = "gate-dev-stubs")]
mod faults;
#[cfg(feature = "gate-dev-stubs")]
pub mod orchestrate;
#[cfg(feature = "gate-dev-stubs")]
mod preview;
mod pricing_read;
mod readiness;
mod signals;
mod stubs;
mod writer;
