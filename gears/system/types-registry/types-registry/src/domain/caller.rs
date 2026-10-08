//! Who a registry call is made for (D17).
//!
//! Every entry point names its caller: the local client by the contract it serves, REST by
//! the credential its route authenticated. P0 reads it nowhere — no tenant scope, no
//! recorded principal (C2/C6) — so P1 adds the policy without touching a signature.

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
