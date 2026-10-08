// Created: 2026-08-13 by Virtuozzo International GmbH
//! The record: images, masking, the shared resource id and the horizon.

use settings_service_sdk::SettingKey;
use time::{Duration, OffsetDateTime};

use super::{
    ActorClassification, AuditOperation, AuditOutcome, AuditRecord, AuditValue, retention_horizon,
};

fn key() -> SettingKey {
    SettingKey::parse("gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~")
        .expect("fixture key parses")
}

fn record() -> AuditRecord {
    AuditRecord::new(
        key().as_str(),
        Some(uuid::Uuid::nil()),
        "admin@acme",
        AuditOperation::Change,
        "req-1",
    )
}

#[test]
fn a_mutation_carries_both_images() {
    let rec = record()
        .with_pre_image(AuditValue::record(
            serde_json::json!(false),
            crate::audit::ImageClass::Public,
        ))
        .with_post_image(AuditValue::record(
            serde_json::json!(true),
            crate::audit::ImageClass::Public,
        ));
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
    let created = record().with_post_image(AuditValue::record(
        serde_json::json!(1),
        crate::audit::ImageClass::Public,
    ));
    assert!(created.pre_image.is_none());
    let removed = record().with_pre_image(AuditValue::record(
        serde_json::json!(1),
        crate::audit::ImageClass::Public,
    ));
    assert!(removed.post_image.is_none());
}

#[test]
fn a_secret_value_carries_no_payload_at_all() {
    let masked = AuditValue::record(
        serde_json::json!("hunter2"),
        crate::audit::ImageClass::Secret,
    );
    assert_eq!(masked, AuditValue::Masked);
    let rendered = serde_json::to_string(&masked).expect("serializes");
    assert!(!rendered.contains("hunter2"), "got `{rendered}`");
}

#[test]
fn masking_a_secret_leaves_the_resource_id_intact() {
    let rec = record()
        .with_pre_image(AuditValue::record(
            serde_json::json!("old-secret"),
            crate::audit::ImageClass::Secret,
        ))
        .with_post_image(AuditValue::record(
            serde_json::json!("new-secret"),
            crate::audit::ImageClass::Secret,
        ));
    let rendered = serde_json::to_string(&rec).expect("serializes");
    assert!(rec.resource.contains("enable_proxy"));
    assert!(!rendered.contains("old-secret"));
    assert!(!rendered.contains("new-secret"));
}

#[test]
fn a_failed_mutation_is_recorded_as_failed() {
    assert_eq!(record().outcome, AuditOutcome::Success);
    assert_eq!(record().failed().outcome, AuditOutcome::Failure);
}

#[test]
fn the_record_uses_the_shared_resource_formatter_and_the_indexed_pair() {
    let rec = record();
    assert_eq!(
        rec.resource,
        super::resource_id::format(&key(), Some(uuid::Uuid::nil()))
    );
    assert_eq!(rec.declaration_key, key().to_string());
    assert_eq!(rec.tenant_id, Some(uuid::Uuid::nil()));
}

#[test]
fn an_administrator_is_pii_and_a_module_is_public() {
    assert_eq!(record().actor_classification, ActorClassification::Pii);
    assert_eq!(
        record().by_module().actor_classification,
        ActorClassification::Public
    );
}

#[test]
fn the_vocabularies_round_trip_through_their_stored_spelling() {
    for op in [
        AuditOperation::Create,
        AuditOperation::Change,
        AuditOperation::Revert,
        AuditOperation::Remove,
        AuditOperation::Clone,
        AuditOperation::SecretUse,
    ] {
        assert_eq!(AuditOperation::parse(op.as_str()), Some(op));
    }
    assert_eq!(AuditOperation::parse("delete"), None);
    assert_eq!(AuditOutcome::parse("failure"), Some(AuditOutcome::Failure));
    assert_eq!(
        ActorClassification::parse("pii"),
        Some(ActorClassification::Pii)
    );
}

#[test]
fn the_horizon_is_the_explicit_instant_or_the_default_from_when_it_happened() {
    let at = OffsetDateTime::from_unix_timestamp(1_000).expect("in range");
    let default = Duration::days(365);
    assert_eq!(retention_horizon(None, at, default), at + default);
    let explicit = at + Duration::days(30);
    assert_eq!(retention_horizon(Some(explicit), at, default), explicit);
}

#[test]
fn an_image_is_recorded_under_the_class_its_caller_states() {
    use crate::audit::ImageClass;
    let value = serde_json::json!("alice@example.com");
    assert_eq!(
        AuditValue::record(value.clone(), ImageClass::Public),
        AuditValue::Clear(value.clone())
    );
    assert_eq!(
        AuditValue::record(value.clone(), ImageClass::Pii),
        AuditValue::Pii(value.clone())
    );
    assert_eq!(
        AuditValue::record(value.clone(), ImageClass::Secret),
        AuditValue::Masked
    );
    // The class travels in the stored image itself.
    assert_eq!(
        serde_json::to_value(AuditValue::Pii(value.clone())).expect("serializes"),
        serde_json::json!({ "kind": "pii", "value": value })
    );
}

#[test]
fn a_value_is_classed_by_its_setting_and_an_unknown_class_is_not_recorded() {
    use crate::audit::ImageClass;
    assert_eq!(ImageClass::of_value("public"), ImageClass::Public);
    assert_eq!(ImageClass::of_value("pii"), ImageClass::Pii);
    assert_eq!(ImageClass::of_value("secret"), ImageClass::Secret);
    assert_eq!(ImageClass::of_value("confidential"), ImageClass::Secret);
}

#[test]
fn a_definition_is_classed_by_the_classification_its_snapshot_carries() {
    // The Schema Default of a `pii` setting is a value of it; a secret
    // setting's default is only the empty placeholder.
    let snapshot =
        |class: &str| serde_json::json!({ "default_value": "x", "data_classification": class });
    assert!(AuditValue::definition(snapshot("pii")).is_pii());
    assert!(matches!(
        AuditValue::definition(snapshot("public")),
        AuditValue::Clear(_)
    ));
    assert!(matches!(
        AuditValue::definition(snapshot("secret")),
        AuditValue::Clear(_)
    ));
    // Without a classification it cannot tell, and treats the image as personal.
    assert!(AuditValue::definition(serde_json::json!({ "default_value": "x" })).is_pii());
}
