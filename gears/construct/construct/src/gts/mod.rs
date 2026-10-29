//! Construct's link-time GTS content: its permissions. They reach the types
//! registry through the process-wide `toolkit-gts` inventory, so the gear's
//! `init` registers nothing itself.

mod permissions;

#[cfg(test)]
#[path = "permissions_test.rs"]
mod permissions_test;
