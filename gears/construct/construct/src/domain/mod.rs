// TODO: DE0301 - the domain layer uses toolkit_db::DbError, DBRunner and DBProvider,
// like the other gears on this template; remove with the platform-wide refactor.
#![allow(unknown_lints)]
#![allow(de0301_no_infra_in_domain)]

pub mod error;
pub mod local_client;
pub mod repo;
pub mod service;

#[cfg(test)]
mod service_test;
