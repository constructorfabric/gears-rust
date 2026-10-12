//! Proportional allocation from `bss-money`: shares rounded once at the stored
//! currency scale, the exact residual assigned by the pinned Last or Largest rule.

pub use bss_money::allocate::{Residual, allocate};
