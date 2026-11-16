// Link the gears whose `#[toolkit::gear]` inventory registrations must be
// present in this OoP binary.
//
// This binary links ONLY:
//   * the target provider gear (this crate's own library); built with
//     `grpc-server`, the library also brings the `api-contracts-grpc` gear and
//     grpc-hub, which serve PaymentApi over gRPC and publish it in the
//     directory, and
//   * the tenant-plane authn stack it needs to turn a bearer token into a
//     `SecurityContext` locally (authn-resolver + static-authn-plugin +
//     types-registry), for both REST and gRPC calls.
//
// It deliberately does NOT link api-gateway or authz-resolver: the HTTP
// surface comes from the OoP bootstrap and the directory is dialed remotely
// (`TOOLKIT_DIRECTORY_ENDPOINT`).
#![allow(unused_imports)]

// Target gear (this crate's library target).
use cf_api_contracts as _;

// Tenant-plane authn stack (embedded per process).
use authn_resolver as _;
use static_authn_plugin as _;
use types_registry as _;
