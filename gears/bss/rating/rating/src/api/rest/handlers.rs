//! Handlers of the operator REST plane: thin projections over the domain services.

use std::sync::Arc;

use axum::Extension;
use axum::http::HeaderMap;
use toolkit::api::canonical_prelude::{CanonicalError, Json, extract};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{RatingInboxEntryView, RatingInboxReleaseRequest};
use crate::domain::inbox::{Inbox, ReleaseCommand};

/// What the handlers read through an `Extension`.
pub struct RestState {
    /// The inbox.
    pub inbox: Inbox,
}

/// The longest `x-request-id` recorded in an audit row; a longer one is not recorded.
const MAX_REQUEST_ID_LEN: usize = 128;

/// `POST /bss-rating/v1/inbox/{id}/release`. The route is `.authenticated()`, so the gateway has
/// authenticated the caller and injected its context; a missing context is a wiring fault the
/// extractor refuses, never a second, gear-local authentication.
///
/// # Errors
///
/// `400` for a malformed id or body; `403` without `usage:release`; `404` for an entry absent or
/// outside the caller's scope; `400 FailedPrecondition` for an entry that is not quarantined;
/// `409` when a concurrent release won.
#[tracing::instrument(skip_all, fields(entry_id = %entry_id))]
pub async fn release_inbox_entry(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<Arc<RestState>>,
    headers: HeaderMap,
    extract::Path(entry_id): extract::Path<Uuid>,
    extract::Json(body): extract::Json<RatingInboxReleaseRequest>,
) -> Result<Json<RatingInboxEntryView>, CanonicalError> {
    // @cpt-begin:cpt-cf-bss-rating-flow-foundation-inbox-release:p1:inst-ir-receive
    let command = ReleaseCommand::new(
        entry_id,
        body.resolution.into(),
        &body.reason,
        request_id(&headers),
    )?;
    // @cpt-end:cpt-cf-bss-rating-flow-foundation-inbox-release:p1:inst-ir-receive
    let entry = state.inbox.release(&ctx, command).await?;
    Ok(Json(entry.into()))
}

/// The caller's request id, recorded in the audit row only when it is 1-128 characters of
/// `[A-Za-z0-9._-]`: the row keeps no space, control character or other text a reader of the
/// audit could be misled by.
fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|id| {
            !id.is_empty()
                && id.len() <= MAX_REQUEST_ID_LEN
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "handlers_tests.rs"]
mod tests;
