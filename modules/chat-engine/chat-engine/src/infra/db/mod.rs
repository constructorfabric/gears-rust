//! Database adapter surface for the `chat_engine` crate.
//!
//! Phase 1 populates only schema and entity definitions. Repository
//! implementations and concrete `DBRunner` integration happen in later phases
//! (4, 5, 9, 11, 12) — those phases consume `&impl modkit_db::secure::DBRunner`
//! directly per the workspace pattern in `mini-chat`.

pub mod entity;
pub mod migrations;
pub mod repo;

pub use entity::{
    VARIANT_INDEX_MAX_RETRIES, compute_next_variant_index, is_variant_unique_violation, message,
    message_reaction, plugin_config, session, session_type,
};
pub use migrations::Migrator;

use std::sync::Arc;

/// Newtype wrapper over the shared `modkit_db::Db` handle.
///
/// Repositories in later phases accept `&impl DBRunner` directly (the
/// workspace-wide pattern), so this newtype exists to give the application
/// layer (`module.rs`, registered in Phase 15) a single concrete handle to
/// thread through dependency wiring. The inner `Db` is kept private; callers
/// reach the underlying runner through `as_db()` and the workspace
/// `DBRunner` impls on `DbConn` / `SecureConn`.
#[derive(Clone, Debug)]
pub struct Connection {
    inner: Arc<modkit_db::Db>,
}

impl Connection {
    /// Construct a `Connection` wrapping the shared `Db` instance handed in
    /// by the runtime (`DatabaseCapability::connection()` — wired in Phase
    /// 15).
    #[must_use]
    pub fn new(db: Arc<modkit_db::Db>) -> Self {
        Self { inner: db }
    }

    /// Borrow the underlying `modkit_db::Db` for low-level access. Later
    /// phases prefer `&impl DBRunner` over this, but the application layer
    /// uses it for migration registration and lifecycle wiring.
    #[must_use]
    pub fn as_db(&self) -> &modkit_db::Db {
        &self.inner
    }

    /// Clone the `Arc<Db>` for ownership transfer into long-lived services.
    #[must_use]
    pub fn handle(&self) -> Arc<modkit_db::Db> {
        Arc::clone(&self.inner)
    }
}
