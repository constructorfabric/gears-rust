//! Retention policy primitives.
//!
//! This module is a thin re-export of the SDK [`RetentionPolicy`] enum.
//! The actual GC / cleanup loop is implemented later (Phase 12 onward) and
//! reads the policy from session metadata via
//! `domain::session::get_retention_policy`.
//
// @cpt-cf-chat-engine-domain-retention:p2

pub use chat_engine_sdk::models::RetentionPolicy;
