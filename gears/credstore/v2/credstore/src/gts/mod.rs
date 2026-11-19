// Created: 2026-09-11 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Credstore's link-time GTS content.
//!
//! Everything declared here reaches `types-registry` automatically through the
//! process-wide `toolkit-gts` inventory — no registration code in the gear's
//! `init` path is needed. One file per content kind keeps this directory
//! navigable (permissions today).

mod permissions;
