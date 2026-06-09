//! Thin per-feature handler modules + the shared RFC-9457 `IntoResponse`
//! glue.
//!
//! Phase 14 replaces the Phase 4 scaffold `IntoResponse` for
//! [`ChatEngineError`] with the canonical pipeline:
//!
//! ```text
//! ChatEngineError → CanonicalError → Problem (RFC-9457)
//! ```
//!
//! The conversion travels through [`crate::api::rest::error`], so the
//! Phase 14 mapping table is the single source of truth.  The
//! [`canonical_error_middleware`](modkit::api::canonical_error_middleware)
//! installed by the gateway then enriches the resulting `Problem` with
//! `instance` + `trace_id` from the request scope.
//
// @cpt-cf-chat-engine-api-rest-handlers:p14

pub mod export;
pub mod glue;
pub mod intelligence;
pub mod messages;
pub mod reactions;
pub mod search;
pub mod session_types;
pub mod sessions;
pub mod variants;

use axum::response::{IntoResponse, Response};
use modkit_canonical_errors::{CanonicalError, Problem};

use crate::domain::error::ChatEngineError;

impl IntoResponse for ChatEngineError {
    fn into_response(self) -> Response {
        let canonical: CanonicalError = self.into();
        // Round-trip via `Problem` so the wire shape (status, type_url,
        // detail, context.violations…) matches the RFC-9457 contract
        // honoured by every other module in the workspace. The canonical
        // error middleware adds `instance` and `trace_id` post-handler.
        Problem::from(canonical).into_response()
    }
}
