//! Request identity: canonical bytes, the request fingerprint, and the
//! idempotency scope hash.
//!
//! # Why the canonical bytes are built here and not taken from `serde_json`
//!
//! `serde_json::Value`'s map is a `BTreeMap` only while the `preserve_order`
//! feature is off, and features unify across a workspace — any crate enabling it
//! would turn that map into an `IndexMap`, making the fingerprint depend on how the
//! caller happened to order the document and turning a re-submission into a
//! spurious `409`. Sorting explicitly costs one rebuild per candidate and removes
//! the dependency on a flag this crate does not control. (`gts-rust` has no
//! canonical-JSON function to defer to; it canonicalizes the *identifier*, via
//! `GtsId::try_new`.)
//!
//! **Numbers are not canonicalized.** `1` and `1.0` are distinct, because
//! `serde_json` preserves the literal form and RFC 8785 number canonicalization has
//! no implementation in this workspace. The consequence is in the safe direction: a
//! reformatted number yields a fresh fingerprint — a `409` on a reused key, never a
//! false replay.
//!
//! # Every field is length-prefixed
//!
//! A digest over concatenated fields cannot distinguish `("ab", "c")` from
//! `("a", "bc")`, so a candidate identifier ending in a digit could otherwise
//! collide with a different split of identifier and body.
//!
//! # Why SHA-256, and why `aws-lc-rs` rather than `sha2`
//!
//! Both digests here cover **client-controlled input** and their equality decides a
//! replay, so both need collision resistance against a chosen input:
//!
//! * `request_fingerprint` covers the submitted body — equality returns the stored
//!   operation, inequality means `409`. A caller able to collide with another body
//!   would be handed an operation it never named, on a hash match alone.
//! * `idempotency_scope_hash` is half of `UNIQUE (idempotency_scope_hash,
//!   idempotency_key)`. It digests constants in P0 (see `P0_PRINCIPAL_ID` below),
//!   but once P1 puts a real tenant in it a forgeable digest lets one tenant
//!   collide into another's `Idempotency-Key` namespace.
//!
//! That rules out the inline FNV-1a of [`crate::domain::artifacts`] — not on
//! collision *probability*, ~2⁻⁶⁴ per comparison either way, but because FNV-1a's
//! round is invertible (`h ← (h ^ b) · PRIME`, odd `PRIME`), so a target digest can
//! be solved for rather than searched. This layout hands an attacker the freedom to
//! do it: only the 1-byte `force` field follows the 8 arbitrary bytes of
//! `expected_resource_version`.
//!
//! The hasher is `aws-lc-rs` — the FIPS-validated provider the platform already
//! installs at bootstrap — and not `sha2`, which DE0708 bans outside an allow-list
//! (`SECURITY.md §9`), as in `bss/ledger`'s `payload_hash`.

use aws_lc_rs::digest::{Context, SHA256};
use serde_json::{Map, Value};
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::Precondition;
use crate::domain::enums::{OperationKind, OwnershipScope, Plane};

/// SHA-256 digest of the idempotency namespace.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScopeHash([u8; 32]);

impl ScopeHash {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Validate bytes read from the database before they enter the domain.
    pub fn from_stored(bytes: Vec<u8>) -> Result<Self, Vec<u8>> {
        <[u8; 32]>::try_from(bytes).map(Self)
    }
}

/// SHA-256 digest binding an idempotency key to one canonical request.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestFingerprint([u8; 32]);

impl RequestFingerprint {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Validate bytes read from the database before they enter the domain.
    pub fn from_stored(bytes: Vec<u8>) -> Result<Self, Vec<u8>> {
        <[u8; 32]>::try_from(bytes).map(Self)
    }
}

/// The principal recorded on every P0 operation.
///
/// ponytail: ceiling C2. P0 has no platform identity to name — no inbound
/// platform-identity validator exists (C8) — so this constant stands in. `nil` is
/// the value, decided at the Checkpoint 1 gate (SPEC §17, O2 closed): it is the
/// honest spelling of "no subject", where a deterministic `UUIDv5` would read as a
/// principal that could be resolved. Greppability comes from this constant's name,
/// not from its value.
/// Because `idempotency_scope_hash` then digests three constants, the
/// `Idempotency-Key` namespace is effectively **global**: two unrelated callers
/// reusing one key collide with `409` rather than being separated by scope. The
/// upgrade path needs no schema change, only a real subject: the column and the
/// digest already carry it.
///
/// TODO(P1): replace with the `SecurityContext` subject once platform identity
/// exists, at which point the namespace narrows to (plane, tenant, principal) as
/// `database.sql` describes.
pub const P0_PRINCIPAL_ID: Uuid = Uuid::nil();

/// Canonical UTF-8 bytes of a JSON document: object keys sorted, arrays in order,
/// no insignificant whitespace.
#[must_use]
pub fn canonical_text(value: &Value) -> String {
    canonicalize(value).to_string()
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<(&String, &Value)> = map.iter().collect();
            sorted.sort_by_key(|(key, _)| *key);
            let mut out = Map::with_capacity(sorted.len());
            for (key, child) in sorted {
                out.insert(key.clone(), canonicalize(child));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

/// One candidate's contribution to the fingerprint.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct FingerprintCandidate<'a> {
    pub gts_id: &'a str,
    /// The canonical body text, as [`canonical_text`] produced it.
    pub canonical_body: &'a str,
    pub precondition: Precondition,
    pub force: bool,
}

/// Everything the fingerprint covers.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct FingerprintInput<'a> {
    pub kind: OperationKind,
    /// Orthogonal to `kind` and part of the fingerprint, so reusing one
    /// `Idempotency-Key` for a dry run and then a commit is a **mismatch**, not a
    /// replay of the dry-run result (`database.sql`).
    pub dry_run: bool,
    pub plane: Plane,
    pub tenant_id: Option<Uuid>,
    pub principal_id: Uuid,
    /// The owner side of the request. Constant in P0 (`Global` / `None`), and
    /// included so a tenant-owned request cannot ever fingerprint-match a global
    /// one under the same key.
    pub ownership_scope: OwnershipScope,
    pub candidates: &'a [FingerprintCandidate<'a>],
}

/// A digest over the canonical body, operation kind and mode, owner,
/// preconditions and each `force` flag.
#[must_use]
pub fn request_fingerprint(input: &FingerprintInput<'_>) -> RequestFingerprint {
    let mut hasher = Context::new(&SHA256);
    // A version tag, so a future change to what the fingerprint covers cannot
    // read as a matching replay of a request accepted under the old rules.
    write_field(&mut hasher, b"tr-fingerprint-v1");
    write_field(&mut hasher, &[kind_tag(input.kind)]);
    write_field(&mut hasher, &[u8::from(input.dry_run)]);
    write_field(&mut hasher, &[plane_tag(input.plane)]);
    write_field(&mut hasher, &[ownership_tag(input.ownership_scope)]);
    write_field(
        &mut hasher,
        input.tenant_id.as_ref().map_or(&[][..], |t| t.as_bytes()),
    );
    write_field(&mut hasher, input.principal_id.as_bytes());
    write_field(&mut hasher, &field_len(input.candidates.len()));
    for candidate in input.candidates {
        write_field(&mut hasher, candidate.gts_id.as_bytes());
        write_field(&mut hasher, candidate.canonical_body.as_bytes());
        write_field(
            &mut hasher,
            &candidate.precondition.stored_value().to_be_bytes(),
        );
        write_field(&mut hasher, &[u8::from(candidate.force)]);
    }
    let digest = hasher.finish();
    let mut bytes = [0; 32];
    bytes.copy_from_slice(digest.as_ref());
    RequestFingerprint(bytes)
}

/// A digest of (plane, `tenant_id`, `principal_id`).
///
/// Digested rather than stored as three columns because all three backends permit
/// multiple `NULL`s in a unique key, so a nullable `tenant_id` in
/// `uq_tr_operation_idem` would stop the constraint from serializing anything
/// (`database.sql`).
#[must_use]
pub fn idempotency_scope_hash(
    plane: Plane,
    tenant_id: Option<Uuid>,
    principal_id: Uuid,
) -> ScopeHash {
    let mut hasher = Context::new(&SHA256);
    write_field(&mut hasher, b"tr-idem-scope-v1");
    write_field(&mut hasher, &[plane_tag(plane)]);
    write_field(
        &mut hasher,
        tenant_id.as_ref().map_or(&[][..], |t| t.as_bytes()),
    );
    write_field(&mut hasher, principal_id.as_bytes());
    let digest = hasher.finish();
    let mut bytes = [0; 32];
    bytes.copy_from_slice(digest.as_ref());
    ScopeHash(bytes)
}

fn write_field(hasher: &mut Context, bytes: &[u8]) {
    hasher.update(&field_len(bytes.len()));
    hasher.update(bytes);
}

/// A length as a fixed 8 bytes, big-endian.
///
/// Not `usize::to_be_bytes`: that is 4 bytes on a 32-bit target and 8 on a 64-bit
/// one, so the same request would fingerprint differently on the two — and a stored
/// `request_fingerprint` would stop matching its own replay after a rebuild for a
/// different target. Every other input to this digest is already a fixed width for
/// exactly that reason. The saturation is unreachable: no field is 16 exabytes long.
fn field_len(len: usize) -> [u8; 8] {
    u64::try_from(len).unwrap_or(u64::MAX).to_be_bytes()
}

// The storage enums derive no `Serialize`, deliberately (T3), so their wire
// numbering cannot leak. These tags are the fingerprint's own encoding and are
// pinned by tests rather than borrowed from the column values.
const fn kind_tag(kind: OperationKind) -> u8 {
    match kind {
        OperationKind::Registration => 1,
        OperationKind::Deletion => 2,
    }
}

const fn plane_tag(plane: Plane) -> u8 {
    match plane {
        Plane::Platform => 1,
        Plane::Tenant => 2,
    }
}

const fn ownership_tag(scope: OwnershipScope) -> u8 {
    match scope {
        OwnershipScope::Global => 1,
        OwnershipScope::Tenant => 2,
    }
}

#[cfg(test)]
#[path = "fingerprint_tests.rs"]
mod fingerprint_tests;
