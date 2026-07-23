//! REST handlers for the interactive per-user OAuth authorization-code flow.
//!
//! These endpoints drive the out-of-band browser enrollment for upstreams
//! whose auth plugin is `oauth2_auth_code`. They delegate to the domain
//! [`OAuthEnrollmentService`](crate::domain::services::OAuthEnrollmentService)
//! exposed on [`AppState`].

use axum::Json;
use axum::extract::{Extension, Path, Query};
use axum::response::{Html, IntoResponse, Redirect, Response};
use http::StatusCode;
use toolkit_canonical_errors::Problem;
use toolkit_security::SecurityContext;
use url::Url;

use crate::api::rest::dto::{
    BeginOAuthRequest, BeginOAuthResponse, OAuthCallbackQuery, OAuthConnectionStatusResponse,
};
use crate::api::rest::error::domain_error_to_problem;
use crate::api::rest::extractors::parse_gts_id;
use crate::domain::gts_helpers as gts;
use crate::gear::AppState;

/// POST /oagw/v1/upstreams/{id}/oauth/authorize
pub async fn begin_authorization(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
    Json(req): Json<BeginOAuthRequest>,
) -> Result<impl IntoResponse, Problem> {
    let instance = format!("/oagw/v1/upstreams/{id}/oauth/authorize");
    let uuid = parse_gts_id(&id, gts::UPSTREAM_SCHEMA, &instance)?;
    let outcome = state
        .oauth
        .begin(&ctx, uuid, req.return_to, req.client_name)
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    Ok(Json(BeginOAuthResponse {
        authorization_url: outcome.authorization_url,
        state: outcome.state,
    }))
}

/// GET /oagw/v1/oauth/callback
///
/// Unauthenticated authorization-server redirect target opened directly in the
/// user's browser. The authorization `code` is exchanged inside OAGW and never
/// returned to the caller; the acting identity is recovered from the pending
/// state resolved by the CSRF `state`.
///
/// Because the caller is a browser, every outcome completes the flow by
/// redirecting (`302`) to the allowlisted `return_to` URL captured at `begin`,
/// with an `oauth=success|error` query parameter appended so the app can react.
/// When no `return_to` can be resolved (e.g. the pending state expired), it
/// falls back to a small self-contained HTML page (never a bare `204` / JSON):
/// * success (`code` + `state`) → token persisted, redirect with `oauth=success`;
/// * error redirect (`error` [+ `error_description`], no `code`, RFC 6749
///   §4.1.2.1) → pending discarded, redirect with `oauth=error`;
/// * missing `code` with no `error`, or a failed code exchange → `oauth=error`.
pub async fn oauth_callback(
    Extension(state): Extension<AppState>,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    // Authorization-server error redirect: carries `error` (and optionally
    // `error_description`) with no `code`. Discard the pending entry and send
    // the browser back to the app rather than attempting an exchange.
    if let Some(error) = query.error.as_deref() {
        tracing::info!(
            error,
            description = query.error_description.as_deref().unwrap_or_default(),
            "OAuth authorization callback returned an error redirect"
        );
        let return_to = state.oauth.abort(query.state).await;
        return complete_flow(return_to, CallbackOutcome::Failure);
    }

    let Some(code) = query.code else {
        tracing::warn!("OAuth callback missing authorization code and no error present");
        let return_to = state.oauth.abort(query.state).await;
        return complete_flow(return_to, CallbackOutcome::Failure);
    };

    match state.oauth.complete(query.state, code).await {
        Ok(return_to) => complete_flow(Some(return_to), CallbackOutcome::Success),
        Err(e) => {
            // Error detail is not reflected to the browser; logged server-side.
            // The pending entry was consumed by `complete`, so there is no
            // `return_to` to redirect to — fall back to the HTML page.
            tracing::warn!(error = %e, "OAuth authorization-code exchange failed");
            complete_flow(None, CallbackOutcome::Failure)
        }
    }
}

/// Terminal outcome of the callback.
#[derive(Clone, Copy)]
enum CallbackOutcome {
    Success,
    Failure,
}

impl CallbackOutcome {
    /// Value appended as the `oauth` query parameter on the `return_to`
    /// redirect so the app can distinguish outcomes.
    fn query_value(self) -> &'static str {
        match self {
            CallbackOutcome::Success => "success",
            CallbackOutcome::Failure => "error",
        }
    }
}

/// Complete the browser flow: redirect (`302`) to the allowlisted `return_to`
/// with an `oauth=success|error` parameter appended, or render the HTML page
/// when no usable `return_to` is available.
///
/// `return_to` was validated against the deployment allowlist at `begin` time
/// (and stored in the pending state), so it is safe to redirect to here.
fn complete_flow(return_to: Option<String>, outcome: CallbackOutcome) -> Response {
    if let Some(return_to) = return_to {
        if let Ok(mut url) = Url::parse(&return_to) {
            url.query_pairs_mut()
                .append_pair("oauth", outcome.query_value());
            return Redirect::to(url.as_str()).into_response();
        }
        tracing::warn!("stored return_to is not a valid URL; falling back to HTML page");
    }
    callback_page(outcome)
}

/// Render the browser-completing HTML page for the OAuth callback. All copy is
/// static (no request-controlled input is reflected), so the page is not an
/// injection vector.
fn callback_page(outcome: CallbackOutcome) -> Response {
    let (title, message) = match outcome {
        CallbackOutcome::Success => (
            "Authorization complete",
            "You are connected. You can close this window and return to the application.",
        ),
        CallbackOutcome::Failure => (
            "Authorization failed",
            "The authorization could not be completed. You can close this window and try connecting again.",
        ),
    };
    let body = format!(
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         </head>\n\
         <body style=\"font-family: system-ui, -apple-system, sans-serif; max-width: 32rem; margin: 4rem auto; padding: 0 1rem; text-align: center;\">\n\
         <h1>{title}</h1>\n\
         <p>{message}</p>\n\
         </body>\n\
         </html>\n"
    );
    (StatusCode::OK, Html(body)).into_response()
}

/// DELETE /oagw/v1/upstreams/{id}/oauth
pub async fn revoke_authorization(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    let instance = format!("/oagw/v1/upstreams/{id}/oauth");
    let uuid = parse_gts_id(&id, gts::UPSTREAM_SCHEMA, &instance)?;
    state
        .oauth
        .revoke(&ctx, uuid)
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /oagw/v1/upstreams/{id}/oauth
pub async fn connection_status(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    let instance = format!("/oagw/v1/upstreams/{id}/oauth");
    let uuid = parse_gts_id(&id, gts::UPSTREAM_SCHEMA, &instance)?;
    let status = state
        .oauth
        .status(&ctx, uuid)
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    Ok(Json(OAuthConnectionStatusResponse {
        connected: status.connected,
        expires_at_unix: status.expires_at_unix,
    }))
}
