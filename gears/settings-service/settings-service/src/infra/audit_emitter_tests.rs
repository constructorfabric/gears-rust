// Created: 2026-08-13 by Constructor Tech
//! Tests for the tracing audit stand-in.
//!
//! What matters here is the masking boundary: a secret value must have nothing
//! to print, and that must hold by construction rather than by the emitter
//! remembering to check.

use settings_service_sdk::SettingKey;

use super::{TracingAuditEmitter, render};
use crate::audit::{AuditEmitter, AuditRecord, AuditScope, AuditValue};

fn key() -> SettingKey {
    SettingKey::parse("gts.cf.settings.types.bool_flag.v1~acme.settings.network.enable_proxy.v1")
        .expect("fixture key parses")
}

#[test]
fn a_secret_value_renders_as_masked() {
    // `AuditValue::Masked` is a unit variant, so the emitter has no payload to
    // print even if it tried.
    let masked = AuditValue::record(serde_json::json!("hunter2"), true);
    let rendered = render(Some(&masked));
    assert_eq!(rendered, "<masked>");
    assert!(!rendered.contains("hunter2"));
}

#[test]
fn a_clear_value_renders_in_full() {
    // Deliberate: seeing what changed is the point of the pre/post images, and
    // category fields carry nothing personal or secret.
    let clear = AuditValue::record(serde_json::json!({"name": "Networking"}), false);
    assert!(render(Some(&clear)).contains("Networking"));
}

#[test]
fn an_absent_image_is_distinguishable_from_a_masked_one() {
    // A create has no pre-image and a delete no post-image; neither is the same
    // as a value that existed but was withheld.
    assert_eq!(render(None), "<absent>");
    assert_ne!(render(None), render(Some(&AuditValue::Masked)));
}

#[tokio::test]
async fn recording_succeeds_and_keeps_the_fallible_signature() {
    // Infallible today, fallible by contract: when a real destination is bound,
    // a failed write must fail the mutation, and every call site already
    // propagates the error.
    let record = AuditRecord::new(
        key().as_str(),
        AuditScope::Platform,
        "admin",
        "category.create",
        "req-1",
    )
    .with_post_image(AuditValue::record(
        serde_json::json!({"key": "network"}),
        false,
    ));
    assert!(TracingAuditEmitter.audit(record).await.is_ok());
}
