//! Secure Gear
//!
//! An out-of-process gear that exposes a single **exposed** (`.exposed()`),
//! **authenticated** (`.authenticated()`) REST route: `GET /secure/v1/whoami`.
//!
//! It demonstrates the full two-plane authentication story
//! (`cpt-cf-adr-two-plane-auth`) for OoP gears:
//!
//! 1. The gear runs as its own process (the "OoP pod") and self-registers its
//!    REST endpoint + `OpenAPI` spec with the `DirectoryService`.
//! 2. The built-in `api-gateway` edge sees the route is authenticated and
//!    enforces the tenant-plane bearer at the edge before reverse-proxying.
//! 3. The OoP pod links an in-process `authn-resolver` (+ `static-authn-plugin`)
//!    and installs `security_context_middleware`, so the forwarded bearer is
//!    **re-validated inside the pod** (zero-trust) and the handler receives a
//!    reconstructed [`SecurityContext`](toolkit_security::SecurityContext).
//!
//! The `whoami` handler echoes the resolved subject/tenant from that
//! locally-reconstructed context — proof that re-validation happened in the pod
//! rather than being trusted from the edge.

// === MODULE DEFINITION ===
mod gear;
pub use gear::Secure;

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod api;
