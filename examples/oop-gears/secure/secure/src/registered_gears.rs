#![allow(unused_imports)]

// Ensure the gear + the in-process AuthN stack are linked and discoverable via
// inventory. The AuthN crates run *inside this OoP pod* so the forwarded bearer
// can be re-validated locally (two-plane auth):
//   - `authn_resolver`      registers the `AuthNResolverClient` in the ClientHub
//   - `static_authn_plugin` provides the actual token validation (accept_all)
//   - `types_registry`      backs GTS plugin discovery used by the resolver
use authn_resolver as _;
use secure as _;
use static_authn_plugin as _;
use types_registry as _;
