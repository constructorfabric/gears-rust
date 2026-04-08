use axum::Extension;
use axum::response::IntoResponse;
use tracing::field::Empty;

use super::{SseBroadcaster, UserEvent, info};

/// SSE endpoint returning a live stream of `UserEvent`.
#[tracing::instrument(
    skip(sse),
    fields(request_id = Empty)
)]
pub async fn users_events(
    Extension(sse): Extension<SseBroadcaster<UserEvent>>,
) -> impl IntoResponse {
    info!("New SSE connection for user events");
    sse.sse_response_named("users_events").into_response()
}
