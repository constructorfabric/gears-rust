//! Temporary v2 of the credstore gear, merged piece by piece (domain model,
//! repository contract and DB schema first). Not registered in the server; it
//! replaces `cf-gears-credstore` when complete.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod domain;
pub mod infra;
