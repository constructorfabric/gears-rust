//! Local client adapters split by resource.
//!
//! This gear houses the object-safe streaming facades that wrap the
//! domain service. Type erasure happens here, at the SDK boundary.
//!
//! Streaming pattern (copy per resource client):
//! 1. `items_stream` turns a page fetch into an item stream (`PagerError`).
//! 2. Map `PagerError` to `UsersInfoError` (`pager_to_users_info`).
//! 3. `Box::pin` to `UsersInfoStream` for `ClientHub` / `dyn` traits.

pub mod addresses;
pub mod cities;
pub mod client;
pub mod users;

use toolkit_sdk::PagerError;
use users_info_sdk::UsersInfoError;

/// `items_stream` yields `PagerError<E>`; the SDK stream is `Result<T, UsersInfoError>`.
fn pager_to_users_info(err: PagerError<UsersInfoError>) -> UsersInfoError {
    match err {
        PagerError::Fetch(e) => e,
        PagerError::InvalidCursor(c) => {
            UsersInfoError::internal(format!("invalid cursor: {c}")).create()
        }
    }
}
