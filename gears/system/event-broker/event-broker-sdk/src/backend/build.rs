//! What the gear hands a [`Provider`](super::Provider) to build one registry
//! entry (feature 0007 §2.7).

/// What the process building a backend is. A fact, not an instruction: each
/// plugin decides what, if anything, it changes - a backend may, for one,
/// run its background work only where it is appended to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Role {
    /// Appends to the backend and provisions its topics; reads only for
    /// ingest's own calls.
    Ingest,
    /// Reads from the backend.
    Delivery,
    /// Does both, in one process.
    Standalone,
}

/// Everything the gear hands a provider to build one registry entry.
///
/// Facts about the process that only the gear knows go here, as fields; what
/// a plugin configures about itself stays in its [`settings`](Self::settings).
/// Non-exhaustive, so a new fact is a new field and no provider breaks.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct BuildContext<'a> {
    /// The settings beside the entry's `type` (feature 0005 §2.1): the
    /// plugin's own, untyped.
    pub settings: &'a serde_json::Map<String, serde_json::Value>,
    /// What this process is.
    pub role: Role,
}

impl<'a> BuildContext<'a> {
    /// The context of an entry with `settings`, in a process that is `role`.
    #[must_use]
    pub fn new(settings: &'a serde_json::Map<String, serde_json::Value>, role: Role) -> Self {
        Self { settings, role }
    }
}
