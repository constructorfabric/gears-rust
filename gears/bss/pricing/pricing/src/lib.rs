//! `PriceBook` pricing gear: runtime and transport skeleton.

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod authz;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod gts;
#[doc(hidden)]
pub mod infra;
#[doc(hidden)]
pub mod module;

#[cfg(test)]
#[path = "source_scan_tests.rs"]
mod source_scan;

#[cfg(test)]
mod test_support;
