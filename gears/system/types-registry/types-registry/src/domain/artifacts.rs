//! Materialized effective artifacts and the resolution fingerprint (SPEC D3).
//!
//! D3 puts the resolved artifacts on the `type_schema` current-state row at
//! admission, so a read is a `SELECT` and no consumer recomputes them. The three
//! artifacts come from `gts-rust`'s `validate_schema`, which is the **only**
//! public route to them — `effective_traits` is `pub(crate)` in the library, and
//! `GtsOps::validate_schema` discards the `ResolvedType` it built.
//!
//! `resolution_fingerprint` digests the canonical bytes of all three. It supports
//! **equality only, never ordering** (`database.sql`): a digest, unlike a counter,
//! stays stable when recomputation yields identical artifacts, which is how a
//! dependency-driven read change is detected without moving
//! `entity.resource_version` — reserved for optimistic writes. The canonical form
//! is [`crate::domain::admission::fingerprint`]'s, for the reason stated there.
//!
//! # Digest algorithms
//!
//! `content_hash` uses FNV-1a because it is only a **prefilter** (ADR-0012): a
//! collision proposes `unchanged`, and T11's exact byte comparison rejects it.
//! `resolution_fingerprint`, however, is the persisted equality identity of the
//! effective artifacts and has no exact-comparison fallback, so it uses SHA-256.

use aws_lc_rs::digest::{Context, SHA256};
use gts::ResolvedType;
use toolkit_macros::domain_model;

use crate::domain::admission::fingerprint::canonical_text;

/// FNV-1a 64-bit — deterministic, non-cryptographic fingerprint.
///
/// Fixed public constants (Fowler–Noll–Vo) give identical output across all Rust
/// versions and platforms. That stability is a storage requirement here: these
/// digests are persisted and compared against digests written by an earlier
/// process.
fn fnv1a_64(fields: &[&[u8]]) -> Vec<u8> {
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut hash = BASIS;
    let mut absorb = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    // Length-prefixed per field, so no two splits of adjacent fields collide. The
    // prefix is a `u64`, not a `usize`, whose `to_be_bytes` is 4 bytes on a 32-bit
    // target and 8 on a 64-bit one — that would make persisted digests a property
    // of the platform. The saturation cannot happen: no field here is 16 exabytes.
    for field in fields {
        absorb(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        absorb(field);
    }
    hash.to_be_bytes().to_vec()
}

/// The three artifacts D3 materializes, plus their digest.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializedArtifacts {
    pub resolved_schema: String,
    pub effective_traits: String,
    pub effective_traits_schema: String,
    pub resolution_fingerprint: Vec<u8>,
}

/// Materialize a validated type's artifacts in canonical form.
#[must_use]
pub fn materialize(resolved: &ResolvedType) -> MaterializedArtifacts {
    let resolved_schema = canonical_text(&resolved.schema);
    let effective_traits = canonical_text(&resolved.effective_traits);
    let effective_traits_schema = canonical_text(&resolved.effective_traits_schema);
    let resolution_fingerprint = resolution_fingerprint(
        &resolved_schema,
        &effective_traits,
        &effective_traits_schema,
    );
    MaterializedArtifacts {
        resolved_schema,
        effective_traits,
        effective_traits_schema,
        resolution_fingerprint,
    }
}

/// Digest the three artifacts. Length-prefixed and version-tagged for the same
/// reasons as the request fingerprint: no two field splits can collide, and a
/// future change to the inputs cannot read as an unchanged resolution.
#[must_use]
pub fn resolution_fingerprint(
    resolved_schema: &str,
    effective_traits: &str,
    effective_traits_schema: &str,
) -> Vec<u8> {
    let mut hasher = Context::new(&SHA256);
    for field in [
        b"tr-resolution-v1".as_slice(),
        resolved_schema.as_bytes(),
        effective_traits.as_bytes(),
        effective_traits_schema.as_bytes(),
    ] {
        hasher.update(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(field);
    }
    hasher.finish().as_ref().to_vec()
}

/// Digest one authored document's canonical bytes — `type_schema_revision.content_hash`.
///
/// A **prefilter only** (ADR-0012): equality of content hashes proposes that a
/// revision is `unchanged`, and T11 confirms it by comparing the canonical bytes
/// themselves. Effective artifacts are deliberately not inputs, because they move
/// when a dependency moves while the authored content stands still.
#[must_use]
pub fn content_hash(canonical_body: &str) -> Vec<u8> {
    fnv1a_64(&[b"tr-content-v1", canonical_body.as_bytes()])
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod artifacts_tests;
