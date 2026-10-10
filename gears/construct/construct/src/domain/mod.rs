// TODO: DE0301 - the domain layer uses toolkit_db::DbError, DBRunner and DBProvider,
// like the other gears on this template; remove with the platform-wide refactor.
#![allow(unknown_lints, reason = "dylint lint names are unknown to rustc")]
#![allow(
    de0301_no_infra_in_domain,
    reason = "domain uses toolkit_db types like the other gears on this template until the platform-wide refactor"
)]

/// The database provider every domain service is built on.
pub(crate) type DbProvider = toolkit_db::DBProvider<toolkit_db::DbError>;

pub mod error;
pub mod local_client;
pub mod model_client;
pub mod plan;
pub mod planner;
pub mod profile;
pub mod record_intake;
pub mod subject_settings;

#[cfg(test)]
mod record_intake_test;
#[cfg(test)]
mod subject_settings_test;
