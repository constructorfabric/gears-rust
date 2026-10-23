//! Engine generation 1's frozen copy of `bss_products_sdk::derived` (rating ADR-0006).
//!
//! `derived.rs` and `derived_tests.rs` are byte-for-byte copies recorded in `vendor/LOCK.toml`;
//! only this wrapper is written by hand, because the module is a crate of its own here. Its
//! documentation links to the rest of products-sdk, which is not vendored.
#![allow(
    rustdoc::broken_intra_doc_links,
    reason = "the verbatim copy links to products-sdk items that are not vendored; `expect` would \
              fire in every build that does not run rustdoc"
)]

#[path = "derived.rs"]
mod derived;

pub use derived::*;
