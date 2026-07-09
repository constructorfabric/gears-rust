//! Wire `type` vocabulary for precondition violations under
//! [`CanonicalError::FailedPrecondition`].
//!
//! Types-registry emits exactly one `FailedPrecondition` shape, produced by the
//! local client's batch-registration parent pre-check: an entity cannot be
//! registered because its required parent type-schema is not yet registered.
//! That disposition has **no `DomainError` / REST-ladder arm** — it is an
//! adapter-only, in-process concept — so it is constructed directly on the
//! in-process boundary and carried losslessly in canonical's typed slots:
//!
//! * `violations[].type` = [`PARENT_NOT_REGISTERED`] (the discriminator),
//! * `violations[].subject` = the missing **parent** type-schema id,
//! * `resource_name` = the **dependent** entity id that failed,
//! * `violations[].description` = the human message.
//!
//! The projection ([`crate::error::TypesRegistryError::ParentNotRegistered`])
//! reconstructs `{ parent_type_id, dependent_id }` from exactly those slots, so
//! the structured batch-registration outcome survives the canonical round-trip.
//! The round-trip tests in [`crate::error`] pin the constant to its `Problem`
//! JSON path.
//!
//! [`CanonicalError::FailedPrecondition`]: toolkit_canonical_errors::CanonicalError::FailedPrecondition

/// The `violations[].type` token types-registry emits for a
/// parent-type-schema-not-registered precondition failure.
///
/// It is the discriminator the projection keys on to reconstruct
/// [`crate::error::TypesRegistryError::ParentNotRegistered`].
pub const PARENT_NOT_REGISTERED: &str = "PARENT_NOT_REGISTERED";
