//! Who a registry call is made for (D17).
//!
//! Local clients identify callers by contract; REST uses the authenticated credential.
//! P0 applies no tenant scope or principal recording (C2/C6). Passing context now
//! lets P1 add policy without changing signatures.

use toolkit_macros::domain_model;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

/// The security context a call arrived with, by plane.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub enum CallerContext<'a> {
    /// A platform workload: the platform contract, or a platform-authenticated route.
    Platform(&'a PlatformSecurityContext),
    /// A tenant: the tenant contract, or a bearer-authenticated route.
    Tenant(&'a SecurityContext),
}
