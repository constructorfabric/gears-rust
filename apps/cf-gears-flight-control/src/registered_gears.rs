// Link the gears that compose the flight-control unit. A gear becomes active
// purely by being linked: the `#[toolkit::gear]` macro's `inventory::submit!`
// registration is collected at startup, so `use <crate> as _;` is enough —
// there is no switchboard to edit.
//
// This crate links ONLY the minimal control plane (directory, transport, edge,
// GTS catalogue, edge JWT validation); see the README and the DESIGN "Flight
// Control Composition" section. The AuthZ plane (authz-resolver, tenant-resolver,
// resource-group) runs as its own OoP unit, so this deployment hosts no PDP —
// `/authz-resolver/v1/evaluate` is served there, not here.
#![allow(unused_imports)]

// Control-plane system gears
use api_gateway as _;
use authn_resolver as _;
use grpc_hub as _;
use service_discovery as _;
use types_registry as _;

// === Plugins (selected via Cargo features; active vendor chosen by config) ===

#[cfg(feature = "static-authn")]
use static_authn_plugin as _;

#[cfg(feature = "oidc-authn")]
use oidc_authn_plugin as _;
