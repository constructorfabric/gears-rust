//! Per-session memory strategy primitives.
//!
//! The SDK enum [`MemoryStrategy`] does not implement [`Default`], because
//! the SDK is consumed by plugin authors who shouldn't be forced into a
//! particular default. The service crate, however, must materialize an
//! explicit default for sessions that don't override it — per the PRD this
//! default is [`MemoryStrategy::Full`].
//
// @cpt-cf-chat-engine-domain-memory-strategy:p2

pub use chat_engine_sdk::models::MemoryStrategy;

/// Service-default memory strategy: send the entire active path to the
/// backend plugin. Used by `domain::session::get_memory_strategy` callers
/// when the reserved metadata key is absent.
#[must_use]
pub fn default_memory_strategy() -> MemoryStrategy {
    MemoryStrategy::Full
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_full() {
        assert!(matches!(default_memory_strategy(), MemoryStrategy::Full));
    }
}
