// Updated: 2026-04-16 by Constructor Tech
// This file is used to ensure that all modules are linked and registered via inventory
// In future we can simply DX via build.rs which will collect all crates in ./modules and generate this file.
// But for now we will manually maintain this file.
#![allow(unused_imports)]

use api_egress as _;
use api_gateway as _;
use authn_resolver as _;
use authz_resolver as _;
use credstore as _;
#[cfg(not(feature = "oop-example"))]
use file_parser as _;
use grpc_hub as _;
use module_orchestrator as _;
use nodes_registry as _;
use resource_group as _;
#[cfg(not(feature = "oop-example"))]
use simple_user_settings as _;
use tenant_resolver as _;
use types as _;
use types_registry as _;

#[cfg(feature = "single-tenant")]
use single_tenant_tr_plugin as _;

#[cfg(feature = "static-tenants")]
use static_tr_plugin as _;

#[cfg(feature = "tenant-resolver-rg")]
use rg_tr_plugin as _;

#[cfg(feature = "static-authn")]
use static_authn_plugin as _;

#[cfg(feature = "static-authz")]
use static_authz_plugin as _;

#[cfg(feature = "tr-authz")]
use tr_authz_plugin as _;

#[cfg(feature = "static-credstore")]
use static_credstore_plugin as _;

// === Optional Modules ===

#[cfg(feature = "mini-chat")]
use mini_chat as _;

#[cfg(feature = "mini-chat")]
use mini_chat::infra::plugins::static_audit as _;

#[cfg(feature = "mini-chat")]
use mini_chat::infra::plugins::static_model_policy as _;

// === Example Features ===

#[cfg(feature = "users-info-example")]
use users_info as _;

#[cfg(feature = "oop-example")]
use calculator_gateway as _;

#[cfg(feature = "oop-example")]
use calculator as _;
