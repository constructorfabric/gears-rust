//! Construct SDK
//!
//! The public API of the Construct gear:
//! - `ConstructClientV1` trait for inter-gear communication
//! - Model types: `RecordOutcome`, and a subject's profile as `SubjectProfile`
//!   (`ProfileCategory`, `ProfileFact`, `FactId`, `FactCategory`, `FactOrigin`)
//! - The connector record types Construct takes at intake ([`gts`])
//! - The reason codes of a refused record ([`reason`])
//! - The person types (`person_types`): one GTS graph node type per category of a person's profile
//! - The profile's own graph types (`profile_types`): the root node and the has-fact edge, and
//!   `graph_types()`, every type Construct registers in graph storage
//!
//! Trait methods return `Result<_, CanonicalError>`: callers either propagate
//! the canonical error or match on its categories.
//!
//! Consumers obtain the client from `ClientHub`:
//! ```ignore
//! let client = hub.get::<dyn ConstructClientV1>()?;
//! let outcome = client.submit_record(&ctx, tenant_id, record).await?;
//! let profile = client.read_profile(&ctx, Some(tenant_id), subject_id).await?;
//! ```

#![forbid(unsafe_code)]

pub mod api;
pub mod gts;
pub mod models;
pub mod person_types;
pub mod profile_types;
pub mod reason;

pub use api::ConstructClientV1;
pub use models::{
    FactCategory, FactId, FactOrigin, ProfileCategory, ProfileFact, RecordOutcome, SubjectProfile,
};
