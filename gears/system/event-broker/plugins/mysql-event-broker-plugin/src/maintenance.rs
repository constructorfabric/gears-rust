//! Background maintenance (`docs/DESIGN.md` §4.9): segments kept ahead of
//! every partition's head, full segments sealed with their weight, whole
//! segments expired by retention and then dropped, and every DDL the server
//! did not finish, or finished without its catalog row, settled.
//!
//! One worker per instance runs every step for one partition at a time,
//! under the partition's named lock ([`steps`]); `append` checks each head
//! against the [`hub`]'s registry and offers the partitions that need work,
//! and the [`rescan`] catches what those offers miss.

pub mod catalog;
pub mod hub;
pub mod rescan;
pub mod start;
pub mod steps;
pub mod worker;

#[cfg(test)]
mod hub_tests;
