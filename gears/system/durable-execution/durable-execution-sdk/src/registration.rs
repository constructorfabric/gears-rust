use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Persisted catalog lifecycle, separate from run epochs and instance fences.
#[non_exhaustive]
pub enum RegistrationState {
    /// Accept new starts and retain matching executable bindings.
    Active,
    /// Reject new starts while preserving bindings for admitted runs.
    Retired,
    /// Drain forward work and saga reversal before releasing bindings.
    Stopping,
    /// Bindings may be evicted; reactivation advances registration generation.
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Choose retained retirement or drain-and-release behavior.
#[non_exhaustive]
pub enum UnregisterMode {
    /// Retire admission and retain bindings for existing work.
    Retain,
    /// Stop admitted work; sagas reverse confirmed effects before bindings are released.
    CancelAndRelease,
}

/// Revision-checked definition retirement request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UnregisterOptions {
    /// Requested catalog retirement behavior.
    pub mode: UnregisterMode,
    /// Catalog revision observed by the caller; stale mutations are rejected.
    pub expected_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Persisted catalog identity, generation and lifecycle revision.
pub struct Registration {
    /// Versioned workflow registration name.
    pub name: String,
    /// Binding generation advanced by activation after release.
    pub generation: u64,
    /// Catalog CAS revision, independent of run execution epochs.
    pub revision: u64,
    /// Current phase and safe transition metadata.
    pub state: RegistrationState,
}
impl UnregisterOptions {
    /// Retire with `mode`, guarded by the catalog revision the caller observed.
    #[must_use]
    pub fn new(mode: UnregisterMode, expected_revision: u64) -> Self {
        Self {
            mode,
            expected_revision,
        }
    }
}
