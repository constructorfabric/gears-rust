//! Links every gear/plugin the standalone binary needs so their
//! `inventory::submit!` registrations are present (matches
//! `apps/cf-gears-example-server/src/registered_gears.rs`'s pattern).
//! Unlike that binary, this one has no feature-flag plugin menu - it exists
//! only for `DeploymentMode::Standalone`, so there is exactly one sensible
//! plugin choice per dependency (decision log entry 29).
#![allow(unused_imports)]

use event_broker as _;

use api_gateway as _;
use authn_resolver as _;
use authz_resolver as _;
use cluster as _;
// `cluster` registers gRPC services, and a process with gRPC services and no hub
// refuses to start (`RegistryError::GrpcRequiresHub`). The hub used to arrive
// only through api-gateway's own dependency; it is named here so that cannot
// change underneath this binary. Default `listen_addr` is loopback.
use grpc_hub as _;
use types_registry as _;

use sqlite_event_broker_plugin as _;
use standalone_cluster_plugin as _;
use static_authn_plugin as _;
use static_authz_plugin as _;
