//! Field-number lockfile for proto wire stability.
//!
//! Without a lockfile, [`generate_proto_file`] sorts fields alphabetically
//! and assigns numbers `1..N`. Adding a field shifts every lexically-larger
//! field's number — a wire-breaking change for any external consumer.
//!
//! The lockfile records the historic `(message, field) → number` mapping
//! and is consulted on every regeneration:
//! - Existing fields keep their assigned numbers.
//! - New fields get the smallest unused number > 0.
//! - Removed fields move to `reserved` so their numbers/names can never be
//!   reused (per proto3 evolution rules).
//!
//! Format on disk: TOML, conventionally named `proto.lock.toml` and committed
//! alongside the SDK's `proto/` tree.
//!
//! ```toml
//! version = 1
//!
//! [messages.ChargeRequest]
//! fields = { amount_cents = 1, currency = 2, description = 3 }
//! reserved_numbers = [4]
//! reserved_names = ["old_field"]
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Schema version of the lockfile format. Bumped when the on-disk shape
/// changes incompatibly. Currently always `1`.
pub const LOCKFILE_VERSION: u32 = 1;

/// On-disk record of historic field-number assignments. Caller is
/// responsible for persisting back to disk after [`crate::generate_proto_file`]
/// returns. The struct is `Default` so missing files are treated as empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtoLockfile {
    /// Schema version of the lockfile itself. Defaults to [`LOCKFILE_VERSION`].
    #[serde(default = "default_version")]
    pub version: u32,
    /// `message_name` → per-message lock entry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub messages: BTreeMap<String, MessageLock>,
    /// `enum_name` → per-enum lock entry. Number 0 is implicitly reserved
    /// for the synthetic `<ENUM_NAME>_UNSPECIFIED` sentinel; user variants
    /// always start at 1.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub enums: BTreeMap<String, EnumLock>,
}

fn default_version() -> u32 {
    LOCKFILE_VERSION
}

impl ProtoLockfile {
    /// Build an empty lockfile (no historic data — every regeneration will
    /// assign fresh numbers starting at 1).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            version: LOCKFILE_VERSION,
            messages: BTreeMap::new(),
            enums: BTreeMap::new(),
        }
    }

    /// Read a lockfile from disk. Returns an empty lockfile if `path` does
    /// not exist, propagates other I/O errors and TOML parse errors.
    ///
    /// # Errors
    /// I/O failures other than `NotFound`, or malformed TOML.
    pub fn load(path: &Path) -> Result<Self, LockfileError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(LockfileError::Parse),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(e) => Err(LockfileError::Io(e)),
        }
    }

    /// Persist a lockfile to disk. Creates parent directories as needed.
    ///
    /// # Errors
    /// I/O failures or TOML serialization failures.
    pub fn save(&self, path: &Path) -> Result<(), LockfileError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(LockfileError::Io)?;
        }
        let text = toml::to_string_pretty(self).map_err(LockfileError::Serialize)?;
        let mut tmp = path.to_path_buf();
        tmp.set_extension("tmp");
        std::fs::write(&tmp, text).map_err(LockfileError::Io)?;
        std::fs::rename(&tmp, path).map_err(LockfileError::Io)
    }
}

/// Per-message field-number assignments. `fields` is the live mapping;
/// `reserved_numbers` and `reserved_names` are tombstones for deleted fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageLock {
    /// `field_name` → `field_number`. Stable across regenerations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, u32>,
    /// Numbers that were once assigned but the field has since been deleted.
    /// Emitted as `reserved` in the `.proto` so they can never be re-used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserved_numbers: Vec<u32>,
    /// Names that were once used but the field has since been deleted.
    /// Emitted as `reserved` in the `.proto` so they can never be re-used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserved_names: Vec<String>,
}

/// Maximum legal proto3 field number per the language spec
/// (<https://protobuf.dev/programming-guides/proto3/#assigning>).
/// In practice protoc enforces this; we stop searching at this boundary so
/// an unbounded `(1u32..)` can't theoretically run out of integer space.
pub const PROTO3_MAX_FIELD_NUMBER: u32 = (1 << 29) - 1;

/// Proto3 reserves field numbers in this inclusive range for the framework
/// itself — generated `.proto` files must never assign numbers in this range.
pub const PROTO3_RESERVED_RANGE: std::ops::RangeInclusive<u32> = 19000..=19999;

impl MessageLock {
    /// Assign or recall a stable number for `field_name`. Existing names
    /// return their historic number; new names get the smallest unused
    /// number > 0 (skipping numbers in `fields` and `reserved_numbers`).
    ///
    /// If [`PROTO3_MAX_FIELD_NUMBER`] is reached without finding a free
    /// slot, the value saturates at the max — protoc itself will then
    /// reject the generated `.proto` with a clearer diagnostic than a
    /// panic in our codegen.
    pub fn assign(&mut self, field_name: &str) -> u32 {
        if let Some(&n) = self.fields.get(field_name) {
            return n;
        }
        let used: BTreeSet<u32> = self
            .fields
            .values()
            .copied()
            .chain(self.reserved_numbers.iter().copied())
            .collect();
        // Field number 0 is invalid in proto3; start at 1. Bounded by the
        // proto3 spec maximum so the search is finite even in pathological
        // inputs (a message with > 2^29 fields). Saturating fallback makes
        // the failure mode "protoc rejects oversized field numbers" rather
        // than "rust panics in the generator".
        let next = (1u32..=PROTO3_MAX_FIELD_NUMBER)
            .filter(|n| !PROTO3_RESERVED_RANGE.contains(n))
            .find(|n| !used.contains(n))
            .unwrap_or(PROTO3_MAX_FIELD_NUMBER);
        self.fields.insert(field_name.to_owned(), next);
        next
    }

    /// Move fields that are present in the lock but absent from the current
    /// schema into the reserved tombstones. Idempotent — re-running with
    /// the same `current_names` set is a no-op.
    pub fn reap_removed(&mut self, current_names: &BTreeSet<String>) {
        let to_remove: Vec<String> = self
            .fields
            .keys()
            .filter(|n| !current_names.contains(n.as_str()))
            .cloned()
            .collect();
        for name in to_remove {
            if let Some(num) = self.fields.remove(&name) {
                self.reserved_numbers.push(num);
                self.reserved_names.push(name);
            }
        }
        self.reserved_numbers.sort_unstable();
        self.reserved_numbers.dedup();
        self.reserved_names.sort();
        self.reserved_names.dedup();
    }
}

/// Per-enum variant-number assignments. Mirrors [`MessageLock`] but starts
/// numbering at 1 — number 0 is reserved by proto3 codegen for the synthetic
/// `<ENUM_NAME>_UNSPECIFIED` sentinel that every emitted enum carries.
///
/// Why a sentinel: proto3 zero-valued defaults are indistinguishable from
/// "field not set on the wire". Without an explicit `_UNSPECIFIED = 0`, the
/// first user variant becomes the silent default — adding new fields to
/// surrounding messages then has wire-level meaning ("missing" decodes as
/// the first variant). Per Google proto3 style guide.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnumLock {
    /// `variant_name` → `variant_number`. Stable across regenerations.
    /// Numbers start at 1; 0 is the implicit `_UNSPECIFIED` slot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, u32>,
    /// Numbers that were once assigned but the variant has since been
    /// deleted. Emitted as `reserved` so they can never be re-used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserved_numbers: Vec<u32>,
    /// Names that were once used but the variant has since been deleted.
    /// Emitted as `reserved` so they can never be re-used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserved_names: Vec<String>,
}

impl EnumLock {
    /// Assign or recall a stable number for `variant_name`. Existing names
    /// return their historic number; new names get the smallest unused
    /// number > 0 (skipping numbers in `variants` and `reserved_numbers`).
    /// Number 0 is permanently reserved for the `_UNSPECIFIED` sentinel.
    pub fn assign(&mut self, variant_name: &str) -> u32 {
        if let Some(&n) = self.variants.get(variant_name) {
            return n;
        }
        let used: BTreeSet<u32> = self
            .variants
            .values()
            .copied()
            .chain(self.reserved_numbers.iter().copied())
            .collect();
        // Start at 1: zero is the sentinel slot and is never assigned to a
        // user variant. Same upper bound as message fields.
        let next = (1u32..=PROTO3_MAX_FIELD_NUMBER)
            .filter(|n| !PROTO3_RESERVED_RANGE.contains(n))
            .find(|n| !used.contains(n))
            .unwrap_or(PROTO3_MAX_FIELD_NUMBER);
        self.variants.insert(variant_name.to_owned(), next);
        next
    }

    /// Move variants that are present in the lock but absent from the
    /// current schema into the reserved tombstones. Idempotent.
    pub fn reap_removed(&mut self, current_names: &BTreeSet<String>) {
        let to_remove: Vec<String> = self
            .variants
            .keys()
            .filter(|n| !current_names.contains(n.as_str()))
            .cloned()
            .collect();
        for name in to_remove {
            if let Some(num) = self.variants.remove(&name) {
                self.reserved_numbers.push(num);
                self.reserved_names.push(name);
            }
        }
        self.reserved_numbers.sort_unstable();
        self.reserved_numbers.dedup();
        self.reserved_names.sort();
        self.reserved_names.dedup();
    }
}

/// Errors produced by lockfile load/save.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LockfileError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(toml::de::Error),
    #[error("TOML serialize error: {0}")]
    Serialize(toml::ser::Error),
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn assign_starts_at_one_for_empty_lock() {
        let mut lock = MessageLock::default();
        assert_eq!(lock.assign("a"), 1);
        assert_eq!(lock.assign("b"), 2);
        assert_eq!(lock.assign("c"), 3);
    }

    #[test]
    fn assign_returns_existing_number_for_known_field() {
        let mut lock = MessageLock::default();
        let first = lock.assign("currency");
        let again = lock.assign("currency");
        assert_eq!(first, again);
    }

    #[test]
    fn assign_skips_reserved_numbers() {
        let mut lock = MessageLock {
            fields: BTreeMap::new(),
            reserved_numbers: vec![1, 2, 4],
            reserved_names: vec![],
        };
        assert_eq!(lock.assign("first"), 3);
        assert_eq!(lock.assign("second"), 5);
    }

    #[test]
    fn assign_fills_gaps_in_existing_fields() {
        let mut lock = MessageLock::default();
        lock.fields.insert("a".into(), 1);
        lock.fields.insert("c".into(), 3);
        assert_eq!(lock.assign("b"), 2);
        assert_eq!(lock.assign("d"), 4);
    }

    #[test]
    fn reap_removed_moves_to_reserved() {
        let mut lock = MessageLock::default();
        lock.assign("amount");
        lock.assign("currency");
        lock.assign("description");
        let mut current = BTreeSet::new();
        current.insert("amount".into());
        current.insert("currency".into());
        lock.reap_removed(&current);
        assert!(!lock.fields.contains_key("description"));
        assert!(lock.reserved_numbers.contains(&3));
        assert!(lock.reserved_names.contains(&"description".to_owned()));
    }

    #[test]
    fn reap_removed_idempotent_on_repeat() {
        let mut lock = MessageLock::default();
        lock.assign("amount");
        lock.assign("description");
        let mut current = BTreeSet::new();
        current.insert("amount".into());
        lock.reap_removed(&current);
        let after_first = lock.clone();
        lock.reap_removed(&current);
        assert_eq!(lock, after_first);
    }

    #[test]
    fn round_trips_via_toml() {
        let mut lock = ProtoLockfile::empty();
        let entry = lock.messages.entry("Foo".into()).or_default();
        entry.assign("a");
        entry.assign("b");
        entry.reserved_numbers = vec![5];
        entry.reserved_names = vec!["old".into()];

        let text = toml::to_string_pretty(&lock).unwrap();
        let back: ProtoLockfile = toml::from_str(&text).unwrap();
        assert_eq!(lock, back);
    }

    #[test]
    fn assign_skips_proto3_reserved_range() {
        let mut lock = MessageLock::default();
        for n in 1u32..=18999 {
            lock.fields.insert(format!("f{n}"), n);
        }
        let next = lock.assign("post_reserved");
        assert_eq!(next, 20000, "must skip 19000..=19999");
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let path = std::path::Path::new("/tmp/__definitely_does_not_exist_proto.lock.toml");
        let lock = ProtoLockfile::load(path).unwrap();
        assert_eq!(lock, ProtoLockfile::empty());
    }
}
