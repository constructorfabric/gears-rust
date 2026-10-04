//! Public models of the Construct gear.
//!
//! Transport-agnostic data structures that define the contract between the
//! gear and its consumers. `#[domain_model]` keeps infrastructure types out of
//! them at compile time.

use toolkit_macros::domain_model;
use uuid::Uuid;

/// A stored foundation note. It exists only to give the gear shell one real
/// entity behind the secure ORM and is replaced by Construct's own model.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundationNote {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub text: String,
}

/// Input for creating a [`FoundationNote`]. The tenant comes from the caller's
/// security context.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFoundationNote {
    pub text: String,
}
