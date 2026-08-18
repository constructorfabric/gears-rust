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
//! `entity.resource_version` — reserved for optimistic writes.
//!
//! The canonical form is the one from [`crate::domain::admission::fingerprint`],
//! for the same reason stated there: `serde_json`'s map ordering depends on a
//! feature this crate does not control, and a fingerprint that flapped with map
//! iteration order would make every read look changed.
//!
//! # Why FNV-1a and not SHA-256
//!
//! Both digests here answer *"is this byte-identical to what I had?"* and nothing
//! else. Neither input is client-controlled: `content_hash` covers a document that
//! has already been validated, and `resolution_fingerprint` covers artifacts this
//! server computed. So collision *resistance against a chosen input* buys nothing,
//! and `SECURITY.md §9` puts direct `sha2` imports behind the DE0708 allow-list
//! precisely to keep unreviewed hashing out — the two sites it names as migrated
//! (`toolkit-odata` cursor consistency, `oidc-authn-plugin` cache key) moved to
//! inline FNV-1a for the same reason.
//!
//! Neither use is weakened by the narrower digest:
//!
//! * `content_hash` is a **prefilter** (ADR-0012). A collision proposes
//!   `unchanged` and T11's exact byte comparison then rejects the proposal, so a
//!   collision costs one wasted comparison and cannot produce a wrong answer.
//! * `resolution_fingerprint` is compared old-versus-new **for one entity**, not
//!   across a population, so this is not a birthday problem: a missed refresh
//!   needs the new artifacts to collide with that entity's specific previous
//!   digest, at ~2⁻⁶⁴ per comparison.
//!
//! [`crate::domain::admission::fingerprint`] keeps SHA-256, because its inputs
//! *are* client-controlled and its equality decides a replay. It takes it from
//! `aws-lc-rs` rather than `sha2`, so no allow-list entry is spent there either —
//! see that module's header.

use gts::ResolvedType;

use crate::domain::admission::fingerprint::canonical_text;

/// FNV-1a 64-bit — deterministic, non-cryptographic fingerprint.
///
/// Algorithm is a public specification (Fowler–Noll–Vo) with fixed constants,
/// guaranteeing identical output across all Rust versions and platforms. That
/// stability is a storage requirement here, not a nicety: these digests are
/// persisted and compared against digests written by an earlier process.
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
    // prefix is a `u64` and not a `usize`: `usize::to_be_bytes` is 4 bytes on a
    // 32-bit target and 8 on a 64-bit one, which would make the digest a property of
    // the platform and stop every persisted `content_hash` from matching after a
    // rebuild — the opposite of what this function's docstring promises. The
    // saturation cannot happen: no field here is 16 exabytes long.
    for field in fields {
        absorb(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        absorb(field);
    }
    hash.to_be_bytes().to_vec()
}

/// The three artifacts D3 materializes, plus their digest.
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
    fnv1a_64(&[
        b"tr-resolution-v1",
        resolved_schema.as_bytes(),
        effective_traits.as_bytes(),
        effective_traits_schema.as_bytes(),
    ])
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
