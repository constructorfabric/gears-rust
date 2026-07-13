//! Unit tests for [`ParsedTypeId`] — AM-internal validation +
//! deterministic `UUIDv5` derivation.
//!
//! Mirrors the coverage previously held by the (now-retired)
//! `account_management_sdk::MetadataSchemaId` tests, but asserts
//! against the [`DomainError::MetadataValidation`] surface rather
//! than the granular SDK error enum — the metadata-content variant
//! routes the canonical envelope to `gts.cf.core.am.tenant_metadata.v1~`
//! instead of the tenant default.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test helpers")]

use toolkit_gts::{GTS_ID_PREFIX, gts_id};
use uuid::Uuid;

use super::{METADATA_ROOT_SEGMENT, ParsedTypeId};
use crate::domain::error::DomainError;

/// Canonical valid chained schema id used across positive-path tests.
const VALID_TYPE_ID: &str =
    gts_id!("cf.core.am.tenant_metadata.v1~vendor.app.metadata.branding.v1~");

/// `UUIDv5` expected for [`VALID_TYPE_ID`] under the shared GTS
/// namespace (`Uuid::new_v5(&Uuid::NAMESPACE_URL, b"gts")`). Hardcoded
/// to pin the namespace + algorithm choice: any drift in either
/// makes this test fail immediately. Computed once via
/// `gts::GtsId::try_new(VALID_TYPE_ID).unwrap().to_uuid()`.
const VALID_SCHEMA_UUID: &str = "1908c97f-00d4-5e43-9c33-d3904e7bcfa6";

#[test]
fn parse_happy_path_yields_validated_id_and_uuid() {
    let parsed = ParsedTypeId::parse(VALID_TYPE_ID).expect("valid chained id parses");
    assert_eq!(parsed.as_str(), VALID_TYPE_ID);
    assert_eq!(
        parsed.uuid(),
        Uuid::parse_str(VALID_SCHEMA_UUID).expect("hardcoded literal"),
        "UUID derivation must match upstream gts::GTS_NS namespace pin"
    );
}

#[test]
fn parse_normalises_leading_trailing_whitespace() {
    let with_ws = format!("   {VALID_TYPE_ID}   ");
    let parsed = ParsedTypeId::parse(&with_ws).expect("ws-padded valid id");
    assert_eq!(parsed.as_str(), VALID_TYPE_ID, "trimmed string stored");
}

#[test]
fn parse_rejects_malformed_gts_syntax() {
    let err = ParsedTypeId::parse("not a gts at all").expect_err("malformed");
    let detail = match err {
        DomainError::MetadataValidation { detail } => detail,
        other => panic!("expected MetadataValidation, got {other:?}"),
    };
    assert!(
        detail.contains("malformed metadata schema id"),
        "diagnostic should name the failure mode, got: {detail}"
    );
}

#[test]
fn parse_rejects_wrong_root_segment() {
    // Valid GTS shape (5 tokens per segment), wrong AM-namespace root.
    let alien = gts_id!("cf.core.other_gear.dataset.v1~vendor.app.foo.bar.v1~");
    let err = ParsedTypeId::parse(alien).expect_err("wrong root");
    let detail = match err {
        DomainError::MetadataValidation { detail } => detail,
        other => panic!("expected MetadataValidation, got {other:?}"),
    };
    assert!(
        detail.contains(&format!(
            "must start with `{GTS_ID_PREFIX}{METADATA_ROOT_SEGMENT}`"
        )),
        "diagnostic should name expected root, got: {detail}"
    );
}

#[test]
fn parse_rejects_root_only_chain() {
    // Valid root, no chained segment after it.
    let root_only = gts_id!("cf.core.am.tenant_metadata.v1~");
    let err = ParsedTypeId::parse(root_only).expect_err("root only");
    let detail = match err {
        DomainError::MetadataValidation { detail } => detail,
        other => panic!("expected MetadataValidation, got {other:?}"),
    };
    assert!(
        detail.contains("missing a chained user-registered segment"),
        "diagnostic should call out missing chained segment, got: {detail}"
    );
}

#[test]
fn parse_rejects_instance_id_shape() {
    // Tail segment lacks `~` — instance id, not a schema chain.
    let instance = gts_id!("cf.core.am.tenant_metadata.v1~vendor.app.metadata.branding.v1");
    let err = ParsedTypeId::parse(instance).expect_err("instance shape");
    let detail = match err {
        DomainError::MetadataValidation { detail } => detail,
        other => panic!("expected MetadataValidation, got {other:?}"),
    };
    assert!(
        detail.contains("instance id, not a schema chain"),
        "diagnostic should distinguish instance vs schema, got: {detail}"
    );
}

#[test]
fn uuid_matches_upstream_gts_to_uuid() {
    // Pin the equivalence-class contract: `ParsedTypeId::uuid()`
    // MUST return the same UUID as the upstream `gts::GtsId::to_uuid()`
    // — that is the documented "shared namespace" guarantee. A drift
    // here means storage UUIDs derived in AM no longer match what
    // any sibling using `gts` directly would compute.
    let parsed = ParsedTypeId::parse(VALID_TYPE_ID).expect("valid");
    let upstream = gts::GtsId::try_new(VALID_TYPE_ID)
        .expect("upstream parse")
        .to_uuid();
    assert_eq!(
        parsed.uuid(),
        upstream,
        "AM-side ParsedTypeId UUID drifted from upstream gts::GtsId::to_uuid()"
    );
}
