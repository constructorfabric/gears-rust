// Ensure all platform-host gears are linked and registered via inventory.
//
// A gear becomes active purely by being linked: the `#[toolkit::gear]` macro
// emits an `inventory::submit!` registration that `GearRegistry::discover_and_build`
// collects at startup, so a `use <crate> as _;` here is enough to activate it
// (there is no central switchboard to edit).
//
// Unlike `cf-gears-example-server` (which links every gear into one embedded
// process), this crate links ONLY the co-located core + system gears that make
// up the platform-host image — see the crate's README and the DESIGN "Platform
// Host Composition" section in `docs/arch/toolkit-oop/DESIGN.md`. Gear isolation
// for OoP images is therefore achieved by the dependency graph (what a binary
// links), not by `#[cfg]` gates.
//
// This bundle is the current composition, not a permanent one: it is intended to
// be decomposed over time as gears gain the prerequisites for a split (a remote
// REST/gRPC surface where one is missing, and — for the core gears that are
// currently trust-coupled — S2S/IdP-issued credentials to replace the anonymous
// SecurityContext). Extracting one is then mechanical — move it out of this
// dependency set into its own binary's.
#![allow(unused_imports)]

// Core gears (co-located for now; currently trust-coupled)
use account_management as _;
use authz_resolver as _;
use resource_group as _;
use tenant_resolver as _;

// System gears
use api_gateway as _;
use authn_resolver as _;
use credstore as _;
use gear_orchestrator as _;
use grpc_hub as _;
use types_registry as _;

// === Plugins (selected via Cargo features; active vendor chosen by config) ===

#[cfg(feature = "static-authn")]
use static_authn_plugin as _;

#[cfg(feature = "oidc-authn")]
use oidc_authn_plugin as _;

#[cfg(feature = "static-authz")]
use static_authz_plugin as _;

#[cfg(feature = "tr-authz")]
use tr_authz_plugin as _;

#[cfg(feature = "static-tenants")]
use static_tr_plugin as _;

#[cfg(feature = "single-tenant")]
use single_tenant_tr_plugin as _;

#[cfg(feature = "tenant-resolver-rg")]
use rg_tr_plugin as _;

#[cfg(feature = "static-credstore")]
use static_credstore_plugin as _;

#[cfg(feature = "static-idp")]
use static_idp_plugin as _;
