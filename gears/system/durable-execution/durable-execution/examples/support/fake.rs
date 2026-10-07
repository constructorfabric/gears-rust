//! In-memory stand-ins for external systems (mailer, payment gateway, cloud, CRM).
//!
//! Activities are at-least-once: an effect performed before its checkpoint commits
//! can repeat. Real providers accept an idempotency key; these fakes do the same,
//! so examples pass `ctx.idempotency_key` and observe exactly one effect.
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

pub struct IdempotentProvider<T> {
    effects: Mutex<HashMap<String, T>>,
    calls: AtomicU32,
}
impl<T: Clone> Default for IdempotentProvider<T> {
    fn default() -> Self {
        Self {
            effects: Mutex::new(HashMap::new()),
            calls: AtomicU32::new(0),
        }
    }
}
impl<T: Clone> IdempotentProvider<T> {
    /// Performs `effect` once per key; a repeated key returns the first result.
    pub fn call(&self, key: &str, effect: impl FnOnce() -> T) -> T {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.effects
            .lock()
            .entry(key.to_owned())
            .or_insert_with(effect)
            .clone()
    }
    /// Requests received, including deduplicated repeats.
    pub fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
    /// Distinct effects actually performed.
    pub fn effects(&self) -> usize {
        self.effects.lock().len()
    }
}
