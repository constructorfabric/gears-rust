// Created: 2026-08-13 by Constructor Tech
//! Tests for the audit record contract.
//!
//! Acceptance criterion: FEATURE `gear-foundation.md` §6 — *the Audit Emitter
//! records a mutation with both pre-image and post-image available to the
//! caller*.

use settings_service_sdk::SettingKey;

use super::{AuditOutcome, AuditRecord, AuditScope, AuditValue};

fn key() -> SettingKey {
    SettingKey::parse("gts.cf.settings.types.bool_flag.v1~acme.settings.network.enable_proxy.v1")
        .expect("fixture key parses")
}

fn record() -> AuditRecord {
    AuditRecord::new(
        key().as_str(),
        AuditScope::Platform,
        "admin@acme",
        "change",
        "req-1",
    )
}

#[test]
fn a_mutation_carries_both_images() {
    let rec = record()
        .with_pre_image(AuditValue::record(serde_json::json!(false), false))
        .with_post_image(AuditValue::record(serde_json::json!(true), false));
    assert_eq!(
        rec.pre_image,
        Some(AuditValue::Clear(serde_json::json!(false)))
    );
    assert_eq!(
        rec.post_image,
        Some(AuditValue::Clear(serde_json::json!(true)))
    );
}

#[test]
fn a_create_has_no_pre_image_and_a_remove_no_post_image() {
    // The images are Option because a create has no before and a remove no
    // after — not because recording them is optional.
    let created = record().with_post_image(AuditValue::record(serde_json::json!(1), false));
    assert!(created.pre_image.is_none());
    let removed = record().with_pre_image(AuditValue::record(serde_json::json!(1), false));
    assert!(removed.post_image.is_none());
}

#[test]
fn a_secret_value_carries_no_payload_at_all() {
    // Masked is a unit variant on purpose: there is no field to leak into the
    // trail even by mistake, and no way to reconstruct the value from a record.
    let masked = AuditValue::record(serde_json::json!("hunter2"), true);
    assert_eq!(masked, AuditValue::Masked);
    let rendered = serde_json::to_string(&masked).expect("serializes");
    assert!(!rendered.contains("hunter2"), "got `{rendered}`");
}

#[test]
fn masking_a_secret_leaves_the_resource_id_intact() {
    // DESIGN.md §4.2: only the pre/post values are masked, never the resource
    // id — a secret setting's history stays as queryable as any other.
    let rec = record()
        .with_pre_image(AuditValue::record(serde_json::json!("old-secret"), true))
        .with_post_image(AuditValue::record(serde_json::json!("new-secret"), true));
    let rendered = serde_json::to_string(&rec).expect("serializes");
    assert!(rec.resource.contains("enable_proxy"));
    assert!(!rendered.contains("old-secret"));
    assert!(!rendered.contains("new-secret"));
}

#[test]
fn a_record_cannot_be_built_without_stating_secrecy() {
    // `AuditValue::record` takes the classification as an argument, so there is
    // no constructor that records a value without answering the question.
    let clear = AuditValue::record(serde_json::json!("visible"), false);
    assert_eq!(clear, AuditValue::Clear(serde_json::json!("visible")));
}

#[test]
fn a_failed_mutation_is_recorded_as_failed() {
    // A mutation that did not commit still leaves a trail; the outcome is what
    // distinguishes it, not the absence of a record.
    assert_eq!(record().outcome, AuditOutcome::Success);
    assert_eq!(record().failed().outcome, AuditOutcome::Failure);
}

#[test]
fn the_record_uses_the_shared_resource_formatter() {
    // Not a second spelling: the record's id must be byte-identical to what the
    // history read path computes for the same setting and scope.
    assert_eq!(
        record().resource,
        super::resource_id::format(&key(), AuditScope::Platform)
    );
}
