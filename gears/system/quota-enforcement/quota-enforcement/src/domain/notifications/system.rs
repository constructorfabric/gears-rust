//! The dispatcher's system identity.

use toolkit_security::SecurityContext;
use toolkit_security::constants::DEFAULT_TENANT_ID;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The dispatcher's subject id. Fixed, so a sink's audit trail correlates
/// every delivery under one identity; the version nibble is 0, so it cannot
/// collide with a generated v4 or v5 id.
pub const DISPATCHER_SUBJECT_ID: Uuid = uuid::uuid!("00000000-0000-cf01-0000-716564697370");

/// The dispatcher's subject type (DESIGN, "system identities").
pub const DISPATCHER_SUBJECT_TYPE: &str = "system:quota-enforcement-dispatcher";

/// The context every sink call runs under.
///
/// It identifies the caller and nothing else. Its tenant is the platform's
/// default tenant, as in other gears' background contexts, only so the context
/// is not anonymous: a sink filters on the event's own `scope`, never on this
/// tenant.
///
/// # Errors
///
/// [`DomainError::Internal`] if the context does not build; both of its
/// required fields are constants, so it always does.
pub fn dispatcher_context() -> Result<SecurityContext, DomainError> {
    SecurityContext::builder()
        .subject_id(DISPATCHER_SUBJECT_ID)
        .subject_type(DISPATCHER_SUBJECT_TYPE)
        .subject_tenant_id(DEFAULT_TENANT_ID)
        .build()
        .map_err(|e| DomainError::Internal(format!("dispatcher context: {e}")))
}
