// Link the gears whose `#[toolkit::gear]` inventory registrations must be
// present in this OoP binary.
//
// This binary links ONLY:
//   * the target consumer gear (this crate's own library), and
//   * the tenant-plane authn stack it needs to turn a bearer token into a
//     `SecurityContext` locally (authn-resolver + static-authn-plugin +
//     types-registry).
//
// It deliberately does NOT link the provider gear (`cf-api-contracts`): a
// linked provider would win as a local binding and no gRPC call would be made.
#![allow(unused_imports)]

// Target gear (this crate's library target).
use cf_api_contracts_grpc_consumer as _;

// Tenant-plane authn stack (embedded per process).
use authn_resolver as _;
use static_authn_plugin as _;
use types_registry as _;
