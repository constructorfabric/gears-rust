//! [`ControllerRegistry`]: one [`RateLimitController`] per token, shared by
//! every sync that uses that token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use aws_lc_rs::digest::{self, SHA256};

use super::controller::RateLimitController;

/// A token's identity for the registry: the hex SHA-256 of the token, so the
/// registry never holds the token itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenFingerprint(String);

impl TokenFingerprint {
    #[must_use]
    pub fn of(token: &str) -> Self {
        Self(hex::encode(
            digest::digest(&SHA256, token.as_bytes()).as_ref(),
        ))
    }
}

/// Registry mapping token fingerprints to their controllers.
///
/// A fingerprint seen for the first time gets a fresh controller; later
/// lookups share it, so a token's budget is tracked once however many syncs
/// use it.
#[derive(Debug, Default)]
pub struct ControllerRegistry {
    map: Mutex<HashMap<TokenFingerprint, Arc<RateLimitController>>>,
}

impl ControllerRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The controller for `fingerprint`, created on first sight.
    #[must_use]
    pub fn get_or_insert(&self, fingerprint: TokenFingerprint) -> Arc<RateLimitController> {
        Arc::clone(
            self.lock()
                .entry(fingerprint)
                .or_insert_with(|| Arc::new(RateLimitController::new())),
        )
    }

    /// The controller for `fingerprint`, if one exists.
    #[must_use]
    pub fn get(&self, fingerprint: &TokenFingerprint) -> Option<Arc<RateLimitController>> {
        self.lock().get(fingerprint).cloned()
    }

    /// Number of distinct tokens currently tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<TokenFingerprint, Arc<RateLimitController>>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(b: u8) -> TokenFingerprint {
        TokenFingerprint::of(&format!("token-{b}"))
    }

    #[test]
    fn get_or_insert_returns_same_arc() {
        let reg = ControllerRegistry::new();
        let a = reg.get_or_insert(fp(1));
        let b = reg.get_or_insert(fp(1));
        assert!(Arc::ptr_eq(&a, &b), "same fingerprint must return same Arc");
    }

    #[test]
    fn different_fingerprints_get_different_controllers() {
        let reg = ControllerRegistry::new();
        let a = reg.get_or_insert(fp(1));
        let b = reg.get_or_insert(fp(2));
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn len_and_is_empty() {
        let reg = ControllerRegistry::new();
        assert!(reg.is_empty());
        let _controller = reg.get_or_insert(fp(3));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn get_returns_none_for_unknown_fingerprint() {
        let reg = ControllerRegistry::new();
        assert!(reg.get(&fp(99)).is_none());
    }

    #[test]
    fn fingerprint_hides_the_token() {
        let fp = TokenFingerprint::of("ghp_secret");
        assert!(!fp.0.contains("ghp_secret"));
        assert_eq!(fp.0.len(), 64);
        assert_eq!(fp, TokenFingerprint::of("ghp_secret"));
    }
}
