//! Version-family key derivation, and the sibling identifiers the rules look up.
//!
//! The key is the identifier with **the last segment's version removed** and the
//! trailing `~` normalized away. A minor in a *preceding* segment survives verbatim:
//! it names which base was derived from, part of the entity's identity rather than
//! its own version. `family_key` is **not** a GTS Identifier and MUST NOT be parsed
//! as one (`database.sql`) — it is a byte key, hence the column's binary collation.
//!
//! [`sibling_id`] is [`family_key`] run backwards, and lives here for that reason:
//! split apart, they would be two places that both know how a version is spelled,
//! and a rule looking up an identifier the registry spells differently is a rule
//! that silently never fires.

use gts::{GtsId, GtsIdSegment};
use toolkit_macros::domain_model;

/// A version-family lookup key.
///
/// This is deliberately distinct from a GTS identifier: the last segment has no
/// version and the value MUST NOT be parsed through [`GtsId`]. Keeping the bytes
/// private prevents a family key from being passed to a GTS-id keyed port by
/// accident while leaving the storage adapter free to persist it as text.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FamilyKey(String);

impl FamilyKey {
    /// Rehydrate the domain value from the storage column.
    #[must_use]
    pub fn from_stored(value: String) -> Self {
        Self(value)
    }

    /// The exact bytes persisted in `version_family.family_key`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FamilyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Derive the version-family key of a candidate identifier.
///
/// Built from the parsed segments rather than by string surgery on the
/// identifier: the version suffix is `vMAJOR[.MINOR]`, and a hand-rolled "strip
/// everything after the last `.v`" would also strip a type token that happened to
/// begin with `v` followed by digits.
#[must_use]
pub fn family_key(id: &GtsId) -> FamilyKey {
    let Some(last) = id.segments().last() else {
        return FamilyKey(String::new());
    };
    // Everything up to the last segment is kept **verbatim** — the configurable
    // `gts.` prefix included, and a preceding segment's own minor. Taking the head
    // by subtracting the last segment's raw length means this never reconstructs a
    // prefix it does not own.
    let raw = last.raw();
    let head = id.id().len().saturating_sub(raw.len());
    let mut key = String::with_capacity(id.id().len());
    key.push_str(&id.id()[..head]);
    match last {
        GtsIdSegment::Concrete(_) => {
            key.push_str(last.vendor());
            key.push('.');
            key.push_str(last.package());
            key.push('.');
            key.push_str(last.namespace());
            key.push('.');
            key.push_str(last.type_name());
        }
        // A UUID tail is refused at acceptance (§8.1 step 4), so this is
        // unreachable from the write path. Falling back to the raw form keeps the
        // function total rather than panicking on an input it cannot see.
        GtsIdSegment::UuidTail(_) => key.push_str(raw),
    }
    FamilyKey(key)
}

/// The identifier of the member of this candidate's family at the named version.
///
/// The kind marker is the **candidate's own**: a family holds one kind
/// (`super::rules`), so a rule asking about a sibling is always asking about a
/// sibling of the same kind, and a Type Schema's `~` must survive into the lookup or
/// it would probe an Instance identifier that can never exist.
///
/// Returns the exact bytes `entity.gts_id` holds, so the caller's lookup goes
/// through `uq_tr_entity_gts_id` rather than scanning the family.
#[must_use]
pub fn sibling_id(id: &GtsId, major: u32, minor: Option<u32>) -> String {
    let mut out = family_key(id).0;
    out.push_str(".v");
    out.push_str(&major.to_string());
    if let Some(minor) = minor {
        out.push('.');
        out.push_str(&minor.to_string());
    }
    if id.is_type() {
        out.push('~');
    }
    out
}

/// Canonical order for acquiring family locks: sorted and deduplicated.
///
/// A batch admission touches several families, and two batches taking the same two
/// in opposite orders would deadlock. Sorting is enough to make that impossible, and
/// `FamilyKey`'s byte order is the one the `family_key` column already uses.
///
/// P0 admits one candidate per transaction, so today this is the identity. It is on
/// the path anyway, so the rule lives at the call site rather than in a comment.
#[must_use]
pub fn lock_order(family_keys: &[FamilyKey]) -> Vec<FamilyKey> {
    let mut ordered = family_keys.to_vec();
    ordered.sort();
    ordered.dedup();
    ordered
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod key_tests;
