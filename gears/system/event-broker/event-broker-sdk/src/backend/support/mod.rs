//! Helpers more than one backend plugin uses. Nothing here is part of the
//! contract: a plugin may use them or meet the contract its own way.

mod heads;
mod keys;
mod poll;

#[cfg(test)]
mod heads_tests;
#[cfg(test)]
mod keys_tests;
#[cfg(test)]
mod poll_tests;

pub use heads::LocalHeads;
pub use keys::{partition_for, topic_uuid};
pub use poll::poll_heads;
