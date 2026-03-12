pub mod attachments;
pub mod chats;
pub mod messages;
pub mod models;
pub mod reactions;
pub mod turns;

use modkit::api::prelude::*;

/// Helper to create a 501 Not Implemented problem response.
#[allow(dead_code)]
pub(crate) fn not_implemented() -> Problem {
    Problem::new(
        StatusCode::NOT_IMPLEMENTED,
        "Not Implemented",
        "This endpoint is not yet implemented",
    )
}
