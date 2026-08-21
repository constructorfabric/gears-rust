//! Version-family key derivation.
//!
//! A family groups every version of one logical entity — `v1~`, `v1.4~` and `v2~`
//! of the same type — because `database.sql`'s shape and contiguity rules are asked
//! of the family row, their only serialization point.
//!
//! The key is the identifier with **the last segment's version removed** and the
//! trailing `~` normalized away. A minor in a *preceding* segment survives verbatim:
//! it names which base was derived from, part of the entity's identity rather than
//! its own version. `family_key` is **not** a GTS Identifier and MUST NOT be parsed
//! as one (`database.sql`) — it is a byte key, hence the column's binary collation.
//!
//! The three non-stored family rules — kind, minor shape, contiguity — are T12's.
//! What lives here is the derivation the first admission needs to find or create the
//! row at all.

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
    // Everything up to the last segment is kept **verbatim** — including the
    // `gts.` prefix, which is not part of any segment and is configurable, and
    // including a preceding segment's own minor. Only the last segment is
    // rewritten. Taking the head by subtracting the last segment's raw length
    // means this never has to reconstruct a prefix it does not own.
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

#[cfg(test)]
#[path = "family_tests.rs"]
mod family_tests;
