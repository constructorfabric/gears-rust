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
//! `resolution_fingerprint` is the persisted equality identity of the effective
//! artifacts and has no exact-comparison fallback, so it uses SHA-256. Authored
//! content has no digest: `unchanged` compares the canonical bytes (ADR-0012).

use aws_lc_rs::digest::{Context, SHA256};
use gts::ResolvedType;
use toolkit_macros::domain_model;

use crate::domain::admission::fingerprint::canonical_text;

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

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod artifacts_tests;
