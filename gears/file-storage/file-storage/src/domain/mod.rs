//! Domain layer (control plane): errors, authorization, services, local client.
//!
//! Services work directly over the tenant-scoped `SecureORM` repositories, so this
//! layer names `toolkit_db` and `infra` types; DE0301 is allowed module-wide.
#![allow(unknown_lints)]
#![allow(de0301_no_infra_in_domain)]

pub mod audit;
pub mod authz;
pub mod cleanup;
pub mod data_plane;
pub mod error;
pub mod error_convert;
pub mod etag;
pub mod idempotency;
pub mod local_client;
pub mod multipart;
pub mod multipart_service;
pub mod policy;
pub mod policy_service;
pub mod ports;
pub mod service;
pub(crate) mod storage_layout;
