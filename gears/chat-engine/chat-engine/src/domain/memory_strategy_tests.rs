use super::*;

#[test]
fn default_is_full() {
    assert!(matches!(default_memory_strategy(), MemoryStrategy::Full));
}
