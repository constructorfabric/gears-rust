//! The reason codes of a refused record.
//!
//! A refusal from record intake, on the route and through
//! `ConstructClientV1::submit_record`, carries one field violation whose
//! `reason` is one of these codes. Consumers match on these constants instead
//! of spelling the strings.

/// The record breaks a rule of its type's schema, or of the base envelope.
pub const SCHEMA_VIOLATION: &str = "SCHEMA_VIOLATION";
/// The record names a type the types registry does not know.
pub const UNKNOWN_TYPE: &str = "UNKNOWN_TYPE";
/// The record's type does not derive from the record base type.
pub const NOT_A_RECORD_TYPE: &str = "NOT_A_RECORD_TYPE";
/// The record names an abstract type.
pub const ABSTRACT_TYPE: &str = "ABSTRACT_TYPE";
/// The record carries an `id`, which a push must not.
pub const ID_IN_PUSH: &str = "ID_IN_PUSH";
/// The connector is turned off for the tenant.
pub const CONNECTOR_OFF: &str = "CONNECTOR_OFF";
/// Personalization is off for the record's subject.
pub const PERSONALIZATION_OFF: &str = "PERSONALIZATION_OFF";
/// An erasure of the record's subject is under way.
pub const ERASURE_IN_PROGRESS: &str = "ERASURE_IN_PROGRESS";
