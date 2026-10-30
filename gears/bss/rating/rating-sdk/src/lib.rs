//! BSS Rating SDK: the transport-agnostic V1 contract of the Rating gear.
//!
//! The foundation (feature 01) ships the contract's errors (DESIGN §3.3): [`errors`] holds the
//! canonical-error resources and reason codes every method answers with. The traits and DTOs arrive
//! with the features that serve them; `ExactMoney` and `ExactQuantity` with the first DTO that
//! carries money, and collections page with `toolkit-odata`'s cursors.
//!
//! Every V1 type changes additively only: a new optional field, a new method with a default or a
//! new value of an open enum. Anything else is a new major version served beside V1 (T-D-71). Each
//! V1 DTO gets a golden fixture checked by the `rating-golden` crate.
#![forbid(unsafe_code)]

// @cpt-dod:cpt-cf-bss-rating-dod-foundation-contracts:p2

pub mod errors;
