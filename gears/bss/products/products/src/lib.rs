//! BSS Product & SKU Registry gear.
//!
//! The authoring surface and System of Record for `Product` and `SKU`: what a
//! catalog entity is, what states it may occupy, and what has to be true before
//! it may be published. It computes no price and evaluates no plan — those are
//! the sibling pricing gear's.
//!
//! The crate is laid out as the design set is: a shared **Registry Foundation**
//! (the publish engine — entity model, draft/published state machine,
//! fail-closed validation pipeline, append-only history, idempotency, event
//! outbox and audit trail) that capability features author through. The
//! Foundation owns publish; capability features own capability policy and
//! register their rules into the pipeline.
//!
//! No crate-level allowance of the **workspace** bar: pedantic + restriction is
//! met as written rather than paid down later. The one allowance below is a
//! `rustdoc` lint, and it is a measurement rather than a concession — see its
//! own comment.

// Every module in this crate is `#[doc(hidden)]`, so a module doc that links an
// item private to it is not "public documentation" in the sense this lint
// means: no consumer can reach either end. The crate links private items from
// its `pub mod` docs deliberately and ~99 times, and `--document-private-items`
// does **not** silence the lint (109 warnings either way, measured
// 2026-08-30), so without this allowance `cargo doc` cannot be a gate command
// — and until it was one, 16 genuinely unresolved intra-doc links stood in this
// gear with no gate able to see them. `clippy` compiles doc comments but does
// not resolve their links.
#![allow(rustdoc::private_intra_doc_links)]

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod authz;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod gear;
#[doc(hidden)]
pub mod gts;
#[doc(hidden)]
pub mod infra;

/// What more than one test module needs, written once. See its own doc for
/// why a test-support module earns its place in this crate in particular.
#[cfg(test)]
pub mod test_support;
