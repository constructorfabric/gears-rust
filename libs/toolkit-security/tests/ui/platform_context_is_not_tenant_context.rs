extern crate toolkit_security;

use toolkit_security::{PlatformIdentity, PlatformSecurityContext, SecurityContext};

// A tenant-plane consumer (stand-in for the tenant PolicyEnforcer / handlers)
// that only accepts the tenant SecurityContext.
fn takes_tenant_context(_ctx: &SecurityContext) {}

fn main() {
    let platform = PlatformSecurityContext::new(PlatformIdentity::KubernetesServiceAccount {
        namespace: "toolkit".to_owned(),
        service_account: "flight-control".to_owned(),
        pod: None,
    });

    // Must NOT compile: a platform context is a distinct type and can never be
    // passed where a tenant SecurityContext is expected (cpt-cf-adr-two-plane-auth).
    takes_tenant_context(&platform);
}
