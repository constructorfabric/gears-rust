//! Background maintenance (`docs/DESIGN.md` §4.9): segments kept ahead of
//! every partition's head, full segments sealed with their size on disk,
//! whole-segment retention, dropping expired segments, and a cross-check of the segment
//! rows with the catalog.
//!
//! Each job has a [`prioritizer::Prioritizer`] of partitions and is its own
//! `toolkit-taskward` worker taking from it; [`start`] spawns them all, and
//! a failed run is classified by [`classify`]. `append` checks each head
//! against the [`registry`] in memory and offers work through the
//! [`hub::Hub`]; the [`rescan`] catches what those offers miss.

pub mod catalog;
pub mod classify;
pub mod crosscheck;
pub mod detacher;
pub mod extender;
pub mod hub;
pub mod prioritizer;
pub mod reconcile;
pub mod registry;
pub mod rescan;
pub mod retention;
pub mod sealer;
pub mod start;

#[cfg(test)]
mod classify_tests;
#[cfg(test)]
mod crosscheck_tests;
#[cfg(test)]
mod extender_tests;
#[cfg(test)]
mod prioritizer_tests;
#[cfg(test)]
mod registry_tests;
