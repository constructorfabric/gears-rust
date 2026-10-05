//! BSS approvals inbox.
//!
//! One paged list, one count, one card and one vote door over the approval units of every
//! configured BSS gear. The units stay in those gears. This crate has no database.

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
pub mod gear;

pub use gear::BssApprovalsGear;

#[cfg(test)]
pub mod test_support;
