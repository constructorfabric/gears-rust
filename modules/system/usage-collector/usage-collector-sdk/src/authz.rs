//! PDP resource type, action, and property name constants for usage-record authorization.
//!
//! Shared by both the emitter (CREATE) and the collector gateway (LIST). The
//! [`USAGE_RECORD`] resource type advertises the full set of supported properties
//! to PDP for both flows; the emitter ignores the resulting `AccessScope`, while
//! the gateway compiles it and forwards it to the storage plugin for SQL filtering.

use authz_resolver_sdk::pep::ResourceType;
use modkit_security::pep_properties;

use crate::gts::USAGE_RECORD_GTS;

/// Property names for usage record ABAC (beyond shared `pep_properties`).
pub mod properties {
    /// Metered resource instance identifier.
    pub const RESOURCE_ID: &str = "resource_id";
    /// Metered resource type.
    pub const RESOURCE_TYPE: &str = "resource_type";
    /// Source module identity.
    pub const MODULE: &str = "module";
    /// Subject UUID.
    pub const SUBJECT_ID: &str = "subject_id";
    /// Subject kind.
    pub const SUBJECT_TYPE: &str = "subject_type";
}

/// PDP action names used when calling the policy enforcer.
pub mod actions {
    /// Emit (create) a usage record.
    pub const CREATE: &str = "create";
    /// List/query usage records.
    pub const LIST: &str = "list";
}

/// Usage record resource type used by both the emitter (CREATE) and the
/// collector gateway (LIST).
///
/// The `supported_properties` set declares the full attribute surface the
/// caller is prepared to enforce — PDP may return constraints over any of
/// these, and the storage plugin's `scope_to_sql` translator handles them.
pub const USAGE_RECORD: ResourceType = ResourceType {
    name: USAGE_RECORD_GTS,
    supported_properties: &[
        pep_properties::OWNER_TENANT_ID,
        properties::RESOURCE_ID,
        properties::RESOURCE_TYPE,
        properties::MODULE,
        properties::SUBJECT_ID,
        properties::SUBJECT_TYPE,
    ],
};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "authz_tests.rs"]
mod authz_tests;
