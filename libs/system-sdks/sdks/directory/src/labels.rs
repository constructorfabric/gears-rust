//! Shared validation for directory labels and label selectors.
//!
//! Labels are the single mechanism for shard/peer targeting
//! ([`resolve_by_labels`](crate::DirectoryClient::resolve_by_labels)), so their
//! rules must be enforced identically at **every** boundary a label can enter
//! the system through — not just the gRPC `RegisterInstance` front-end:
//!
//! - the gRPC server (remote, untrusted registrants);
//! - the in-process store ([`LocalDirectoryClient`](crate::DirectoryClient), so
//!   a second transport or an in-process bug cannot smuggle an invalid label
//!   past the checks);
//! - a gear's own start-up, so a mis-configured label fails fast rather than
//!   being rejected later by the directory.
//!
//! Centralising the rules here makes this module the single source of truth;
//! each boundary maps [`LabelValidationError`] onto its own error type.

use std::collections::BTreeMap;

use crate::LabelSelector;

/// Maximum number of labels on a single instance. Unbounded labels bloat
/// directory memory and every downstream list/resolve payload.
pub const MAX_LABELS: usize = 64;
/// Maximum length of a label key (mirrors the Kubernetes label convention).
pub const MAX_LABEL_KEY_LEN: usize = 63;
/// Maximum length of a label value (mirrors the Kubernetes label convention).
pub const MAX_LABEL_VALUE_LEN: usize = 63;

/// A label set or selector violated the directory's label rules.
///
/// The offending key/value is **never** carried in the message: labels are
/// caller-controlled and interpolating them would reflect arbitrary bytes into
/// an error string that is logged and returned over the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelValidationError {
    /// More than [`MAX_LABELS`] entries.
    TooMany {
        /// The offending count.
        count: usize,
    },
    /// A label key was empty or longer than [`MAX_LABEL_KEY_LEN`].
    KeyLength,
    /// A label value was longer than [`MAX_LABEL_VALUE_LEN`].
    ValueLength,
    /// A label key was outside the allowed charset.
    KeyCharset,
    /// A label value was outside the allowed charset.
    ValueCharset,
}

impl std::fmt::Display for LabelValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LabelValidationError::TooMany { count } => {
                write!(f, "too many labels: {count} (max {MAX_LABELS})")
            }
            LabelValidationError::KeyLength => write!(
                f,
                "a label key has length out of range (1..={MAX_LABEL_KEY_LEN})"
            ),
            LabelValidationError::ValueLength => write!(
                f,
                "a label value has length out of range (0..={MAX_LABEL_VALUE_LEN})"
            ),
            LabelValidationError::KeyCharset => write!(
                f,
                "a label key contains characters outside the allowed set \
                 (ASCII alphanumeric, '-', '_', '.'; must start and end alphanumeric)"
            ),
            LabelValidationError::ValueCharset => write!(
                f,
                "a label value contains characters outside the allowed set \
                 (ASCII alphanumeric, '-', '_', '.'; must start and end alphanumeric)"
            ),
        }
    }
}

impl std::error::Error for LabelValidationError {}

/// Whether `s` is a valid label key/value segment: ASCII alphanumeric plus
/// `-`, `_`, `.`, beginning and ending with an alphanumeric character (the
/// Kubernetes label convention). Rejects CR/LF, control characters, `=`,
/// quotes, whitespace, and arbitrary non-ASCII — none of which any label
/// consumer needs and all of which are unsafe to echo/store.
#[must_use]
pub fn is_valid_label_segment(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut last = first;
    for c in chars {
        if !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
            return false;
        }
        last = c;
    }
    last.is_ascii_alphanumeric()
}

/// Validate a single `(key, value)` label pair against the charset/length rules.
///
/// An empty value is permitted (Kubernetes convention); a non-empty one must
/// satisfy the same charset/boundary rules as a key.
///
/// # Errors
/// Returns a [`LabelValidationError`] describing the first violated rule.
pub fn validate_label_pair(key: &str, value: &str) -> Result<(), LabelValidationError> {
    if key.is_empty() || key.len() > MAX_LABEL_KEY_LEN {
        return Err(LabelValidationError::KeyLength);
    }
    if !is_valid_label_segment(key) {
        return Err(LabelValidationError::KeyCharset);
    }
    if value.len() > MAX_LABEL_VALUE_LEN {
        return Err(LabelValidationError::ValueLength);
    }
    if !value.is_empty() && !is_valid_label_segment(value) {
        return Err(LabelValidationError::ValueCharset);
    }
    Ok(())
}

/// Validate a label set: count bound plus per-pair charset/length rules.
///
/// Generic over the pair iterator so both the domain [`BTreeMap`] and the proto
/// `HashMap` can be validated without an intermediate allocation. `count` is
/// the total number of labels (checked against [`MAX_LABELS`] before iterating).
///
/// # Errors
/// Returns a [`LabelValidationError`] describing the first violated rule.
pub fn validate_label_pairs<'a, I>(count: usize, pairs: I) -> Result<(), LabelValidationError>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    if count > MAX_LABELS {
        return Err(LabelValidationError::TooMany { count });
    }
    for (k, v) in pairs {
        validate_label_pair(k, v)?;
    }
    Ok(())
}

/// Validate a domain label map (the store/start-up boundary convenience).
///
/// # Errors
/// Returns a [`LabelValidationError`] describing the first violated rule.
pub fn validate_labels(labels: &BTreeMap<String, String>) -> Result<(), LabelValidationError> {
    validate_label_pairs(
        labels.len(),
        labels.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    )
}

/// Validate a [`LabelSelector`]: its `match_labels` must satisfy the same rules
/// as a stored label set (an over-long or malformed selector could never match
/// a valid label anyway, and echoing it back is unsafe for the same reason).
///
/// # Errors
/// Returns a [`LabelValidationError`] describing the first violated rule.
pub fn validate_selector(selector: &LabelSelector) -> Result<(), LabelValidationError> {
    validate_labels(&selector.match_labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn accepts_well_formed_labels_and_empty_values() {
        assert!(validate_labels(&map(&[("shard", "7"), ("role", "ingest")])).is_ok());
        // Empty value is permitted (Kubernetes convention).
        assert!(validate_labels(&map(&[("canary", "")])).is_ok());
        // Exactly at the length limits is accepted.
        let at_limit = map(&[(
            "k".repeat(MAX_LABEL_KEY_LEN).as_str(),
            "v".repeat(MAX_LABEL_VALUE_LEN).as_str(),
        )]);
        assert!(validate_labels(&at_limit).is_ok());
    }

    #[test]
    fn rejects_too_many_labels() {
        let many: BTreeMap<String, String> = (0..=MAX_LABELS)
            .map(|i| (format!("k{i}"), "v".to_owned()))
            .collect();
        assert_eq!(
            validate_labels(&many),
            Err(LabelValidationError::TooMany {
                count: MAX_LABELS + 1
            })
        );
    }

    #[test]
    fn rejects_bad_lengths() {
        assert_eq!(
            validate_labels(&map(&[("", "v")])),
            Err(LabelValidationError::KeyLength)
        );
        let long_key = "k".repeat(MAX_LABEL_KEY_LEN + 1);
        assert_eq!(
            validate_labels(&map(&[(long_key.as_str(), "v")])),
            Err(LabelValidationError::KeyLength)
        );
        let long_val = "v".repeat(MAX_LABEL_VALUE_LEN + 1);
        assert_eq!(
            validate_labels(&map(&[("shard", long_val.as_str())])),
            Err(LabelValidationError::ValueLength)
        );
    }

    #[test]
    // The `über` literal is intentional: it exercises rejection of a non-ASCII
    // key, which is exactly what this charset test asserts.
    #[allow(clippy::non_ascii_literal)]
    fn rejects_disallowed_charset_without_echoing_input() {
        for bad in ["bad key", "bad=key", "bad\nkey", "-lead", "trail-", "über"] {
            let err = validate_labels(&map(&[(bad, "v")])).unwrap_err();
            assert_eq!(err, LabelValidationError::KeyCharset);
            // The offending input must never be reflected in the message.
            assert!(!err.to_string().contains(bad));
        }
        let err = validate_labels(&map(&[("shard", "bad value")])).unwrap_err();
        assert_eq!(err, LabelValidationError::ValueCharset);
    }

    #[test]
    fn selector_reuses_label_rules() {
        let ok = LabelSelector::new().with("shard", "7");
        assert!(validate_selector(&ok).is_ok());

        let bad = LabelSelector::new().with("bad key", "7");
        assert_eq!(
            validate_selector(&bad),
            Err(LabelValidationError::KeyCharset)
        );
    }
}
