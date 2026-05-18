//! Unit tests for [`ParsedSchemaId`] — AM-internal validation +
//! deterministic `UUIDv5` derivation.
//!
//! Mirrors the coverage previously held by the (now-retired)
//! `account_management_sdk::MetadataSchemaId` tests, but asserts
//! against [`DomainError::Validation`] surface rather than the
//! granular SDK error enum.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test helpers")]

use uuid::Uuid;

use super::ParsedSchemaId;
use crate::domain::error::DomainError;

/// Canonical valid chained schema id used across positive-path tests.
const VALID_SCHEMA_ID: &str = "gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.branding.v1~";

/// `UUIDv5` expected for [`VALID_SCHEMA_ID`] under the shared GTS
/// namespace (`Uuid::new_v5(&Uuid::NAMESPACE_URL, b"gts")`). Hardcoded
/// to pin the namespace + algorithm choice: any drift in either
/// makes this test fail immediately. Computed once via
/// `gts::GtsID::new(VALID_SCHEMA_ID).unwrap().to_uuid()`.
const VALID_SCHEMA_UUID: &str = "1908c97f-00d4-5e43-9c33-d3904e7bcfa6";

#[test]
fn parse_happy_path_yields_validated_id_and_uuid() {
    let parsed = ParsedSchemaId::parse(VALID_SCHEMA_ID).expect("valid chained id parses");
    assert_eq!(parsed.as_str(), VALID_SCHEMA_ID);
    assert_eq!(
        parsed.uuid(),
        Uuid::parse_str(VALID_SCHEMA_UUID).expect("hardcoded literal"),
        "UUID derivation must match upstream gts::GTS_NS namespace pin"
    );
}

#[test]
fn parse_normalises_leading_trailing_whitespace() {
    let with_ws = format!("   {VALID_SCHEMA_ID}   ");
    let parsed = ParsedSchemaId::parse(&with_ws).expect("ws-padded valid id");
    assert_eq!(parsed.as_str(), VALID_SCHEMA_ID, "trimmed string stored");
}

#[test]
fn parse_rejects_malformed_gts_syntax() {
    let err = ParsedSchemaId::parse("not a gts at all").expect_err("malformed");
    let detail = match err {
        DomainError::Validation { detail } => detail,
        other => panic!("expected Validation, got {other:?}"),
    };
    assert!(
        detail.contains("malformed metadata schema id"),
        "diagnostic should name the failure mode, got: {detail}"
    );
}

#[test]
fn parse_rejects_wrong_root_segment() {
    // Valid GTS shape (5 tokens per segment), wrong AM-namespace root.
    let alien = "gts.cf.core.other_module.dataset.v1~vendor.app.foo.bar.v1~";
    let err = ParsedSchemaId::parse(alien).expect_err("wrong root");
    let detail = match err {
        DomainError::Validation { detail } => detail,
        other => panic!("expected Validation, got {other:?}"),
    };
    assert!(
        detail.contains("must start with `gts.cf.core.am.tenant_metadata.v1`"),
        "diagnostic should name expected root, got: {detail}"
    );
}

#[test]
fn parse_rejects_root_only_chain() {
    // Valid root, no chained segment after it.
    let root_only = "gts.cf.core.am.tenant_metadata.v1~";
    let err = ParsedSchemaId::parse(root_only).expect_err("root only");
    let detail = match err {
        DomainError::Validation { detail } => detail,
        other => panic!("expected Validation, got {other:?}"),
    };
    assert!(
        detail.contains("missing a chained user-registered segment"),
        "diagnostic should call out missing chained segment, got: {detail}"
    );
}

#[test]
fn parse_rejects_instance_id_shape() {
    // Tail segment lacks `~` — instance id, not a schema chain.
    let instance = "gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.branding.v1";
    let err = ParsedSchemaId::parse(instance).expect_err("instance shape");
    let detail = match err {
        DomainError::Validation { detail } => detail,
        other => panic!("expected Validation, got {other:?}"),
    };
    assert!(
        detail.contains("instance id, not a schema chain"),
        "diagnostic should distinguish instance vs schema, got: {detail}"
    );
}

#[test]
fn uuid_matches_upstream_gts_to_uuid() {
    // Pin the equivalence-class contract: `ParsedSchemaId::uuid()`
    // MUST return the same UUID as the upstream `gts::GtsID::to_uuid()`
    // — that is the documented "shared namespace" guarantee. A drift
    // here means storage UUIDs derived in AM no longer match what
    // any sibling using `gts` directly would compute.
    let parsed = ParsedSchemaId::parse(VALID_SCHEMA_ID).expect("valid");
    let upstream = gts::GtsID::new(VALID_SCHEMA_ID)
        .expect("upstream parse")
        .to_uuid();
    assert_eq!(
        parsed.uuid(),
        upstream,
        "AM-side ParsedSchemaId UUID drifted from upstream gts::GtsID::to_uuid()"
    );
}
