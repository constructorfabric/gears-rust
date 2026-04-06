use std::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

/// Opaque wrapper around a secret string value.
///
/// `Debug` and `Display` both print `[REDACTED]` — the inner value is never
/// exposed through formatting traits.  Use [`expose`](Self::expose) for
/// controlled access when constructing HTTP headers or form bodies.
///
/// On [`Drop`] the backing buffer is securely zeroed via the [`zeroize`] crate.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretString(String);

impl SecretString {
    /// Create a new `SecretString` from a plain value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Provide read-only access to the underlying secret.
    ///
    /// Callers must not log, store, or otherwise persist the returned slice.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for SecretString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        <String as serde::Deserialize>::deserialize(deserializer).map(SecretString::new)
    }
}

impl Clone for SecretString {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "secret_string_tests.rs"]
mod tests;
