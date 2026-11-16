//! Links every gear the standalone binary runs in-process so their `inventory`
//! registrations are present.
//!
//! Only gear crates (carrying `#[toolkit::gear]`) are named here. The storage
//! backend plugins are not gears - the `event_broker` library wires them via
//! `backend_providers()` - and the `standalone` cluster cache provider ships
//! inside the `cluster` gear (`ClusterGear::provider_registry()`), so neither
//! needs a line here.
#![allow(unused_imports)]

use event_broker as _;

use api_gateway as _;
use authn_resolver as _;
use authz_resolver as _;
use cluster as _;
use types_registry as _;

use static_authn_plugin as _;
use static_authz_plugin as _;
