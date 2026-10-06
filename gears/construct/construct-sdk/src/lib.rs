//! Construct SDK
//!
//! The public API of the Construct gear:
//! - `ConstructClientV1` trait for inter-gear communication
//! - Model types (`FoundationNote`, `NewFoundationNote`)
//! - The person types (`person_types`): one GTS graph node type per category of a person's profile
//!
//! Trait methods return `Result<_, CanonicalError>`: callers either propagate
//! the canonical error or match on its categories.
//!
//! Consumers obtain the client from `ClientHub`:
//! ```ignore
//! let client = hub.get::<dyn ConstructClientV1>()?;
//! let note = client.get_note(&ctx, id).await?;
//! ```

#![forbid(unsafe_code)]

pub mod api;
pub mod models;
pub mod person_types;

pub use api::ConstructClientV1;
pub use models::{FoundationNote, NewFoundationNote};
