// FileStorage domain layer.
//
// Note: the `modkit_db::DbError` and runner types appear in the repository
// trait so the service can drive transactional flows. The split is the same
// pragmatic compromise documented in the simple-user-settings reference
// module — see DECOMPOSITION 2.2.
#![allow(unknown_lints)]
#![allow(de0301_no_infra_in_domain)]

pub mod error;
pub mod etag;
pub mod local_client;
pub mod repo;
pub mod service;

#[cfg(test)]
mod error_test;
#[cfg(test)]
mod etag_test;
#[cfg(test)]
mod local_client_test;
#[cfg(test)]
mod service_test;
