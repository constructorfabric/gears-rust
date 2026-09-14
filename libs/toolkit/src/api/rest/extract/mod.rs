//! Canonical-error-aware replacements for axum's built-in extractors.
//!
//! Each member of this module delegates to its axum built-in counterpart and
//! maps the built-in `Rejection` to `CanonicalError`, so a failed extraction
//! renders as `application/problem+json` instead of axum's default
//! plain-text rejection body. See `canonical_error_layer.rs`'s doc comment
//! and `docs/arch/errors/ADR/0006-cpt-cf-adr-error-middleware-catchall.md`
//! for why this exists.
//!
//! **These extractors always fail as JSON.** `CanonicalError`'s
//! `IntoResponse` impl (`toolkit-canonical-errors`) unconditionally renders
//! `application/problem+json` - there is no `Accept`-header negotiation, on
//! a failed extraction here or anywhere else `CanonicalError` is used in
//! this codebase today. That's the right default for a REST/JSON API gear
//! (every gear in this workspace, currently). A gear that serves HTML
//! directly (server-rendered pages, not a JSON API) would still get a JSON
//! error body from these extractors on a failed `Json`/`Query`/`Path`
//! extraction, regardless of what its successful response would have been -
//! for that kind of route, don't adopt these; write a route-specific
//! extractor (or handle the built-in axum rejection directly) that renders
//! an HTML error instead.

mod error;
mod json;
mod path;
mod query;

pub use json::Json;
pub use path::Path;
pub use query::Query;
