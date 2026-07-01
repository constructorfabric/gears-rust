//! Domain layer (control plane): errors, authorization, services, local client.
//!
//! The control-plane services orchestrate persistence directly over the
//! tenant-scoped SecureORM repositories, so this layer legitimately names
//! `toolkit_db` runner/provider types and the `infra` repositories — the same
//! accepted pattern as the resource-group gear. DE0301 is therefore allowed
//! module-wide here.
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
