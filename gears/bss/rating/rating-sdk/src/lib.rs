//! BSS Rating SDK: the transport-agnostic V1 contract of the Rating gear.
//!
//! The foundation (feature 01) ships the contract rules every later trait and DTO follows
//! (DESIGN §3.3):
//!
//! - [`exact`]: `ExactMoney` and `ExactQuantity`, which cross every wire as canonical strings and
//!   reject JSON numbers, exponents, a leading `+`, leading zeros and `-0`;
//! - [`paging`]: keyset paging with an opaque cursor, `limit` 1–500 and 100 by default;
//! - [`errors`]: the canonical-error resources and reason codes every method answers with;
//! - `golden` (feature `golden`): the additive-only check CI runs over golden JSON fixtures.
//!
//! Every V1 type changes additively only: a new optional field, a new method with a default or a
//! new value of an open enum. Anything else is a new major version served beside V1 (T-D-71).
#![forbid(unsafe_code)]

// @cpt-dod:cpt-cf-bss-rating-dod-foundation-contracts:p2

pub mod errors;
pub mod exact;
#[cfg(feature = "golden")]
pub mod golden;
pub mod paging;

pub use exact::{ExactMoney, ExactNumberError, ExactQuantity};
pub use paging::{Page, PageCursor, PageInfo, PageLimit};
