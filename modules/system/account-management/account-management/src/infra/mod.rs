//! Infrastructure layer for Account Management.
//!
//! Houses the DB-error converter and the SeaORM-backed storage adapter
//! (entities, migrations, repository implementation), the Resource Group
//! SDK adapter that backs the production soft-delete ownership probe,
//! the GTS Types Registry adapter that backs the tenant-type
//! compatibility barrier, and the dev / test [`idp::NoopIdpProvider`]
//! fallback that the module wires when no
//! [`account_management_sdk::IdpPluginClient`] plugin is
//! registered in `ClientHub`.

pub mod canonical_mapping;
pub mod error_conv;
pub mod idp;
pub mod metrics;
pub mod rg;
pub mod sdk_error_mapping;
pub mod storage;
pub mod types_registry;
