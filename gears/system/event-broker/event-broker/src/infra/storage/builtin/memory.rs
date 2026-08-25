//! `InMemoryStorageBackend` (`DESIGN.md:602-603`): non-persistent backend
//! for dev/test. Shell only - a runnable implementation lands with #4347.

#[derive(Debug, Default)]
pub struct InMemoryStorageBackend;

impl InMemoryStorageBackend {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
