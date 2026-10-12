//! Merge, cursor, owner resolution and the calls to the configured sources.

pub mod cursor;
pub mod error;
pub mod merge;
pub mod owner;
pub mod query;
pub mod read;

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod cursor_tests;
#[cfg(test)]
#[path = "merge_tests.rs"]
mod merge_tests;
#[cfg(test)]
#[path = "owner_tests.rs"]
mod owner_tests;
