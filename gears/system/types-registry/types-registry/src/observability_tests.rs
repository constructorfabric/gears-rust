//! The span constructors' contract: fields, their spellings, and what is absent before the facts
//! are known.

use crate::domain::enums::OperationKind;
use crate::domain::key::EntityKey;

#[test]
#[tracing_test::traced_test]
fn the_operation_span_carries_the_operation_id_kind_and_dry_run_mode() {
    let operation_id = uuid::Uuid::from_u128(0x1234);
    let span = super::operation_span(operation_id);
    super::record_operation_facts(&span, OperationKind::Registration, false);
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(logs_contain(&operation_id.to_string()));
    assert!(logs_contain(r#"kind="registration""#));
    assert!(logs_contain("dry_run=false"));
}

#[test]
#[tracing_test::traced_test]
fn the_operation_span_shows_no_kind_before_the_operation_is_read() {
    let span = super::operation_span(uuid::Uuid::from_u128(0x99));
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(!logs_contain("kind="));
}

#[test]
#[tracing_test::traced_test]
fn the_unit_span_carries_the_candidate_identifier_beside_the_operation_facts() {
    let operation_id = uuid::Uuid::from_u128(0x5678);
    let span = super::unit_span(
        operation_id,
        &EntityKey::GtsId("cf.core.example.type.v1~".to_owned()),
        OperationKind::Registration,
        true,
        42,
    );
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(logs_contain("entity_key=cf.core.example.type.v1~"));
    assert!(logs_contain(r#"gts_id="cf.core.example.type.v1~""#));
    assert!(logs_contain(&operation_id.to_string()));
    assert!(logs_contain(r#"kind="registration""#));
    assert!(logs_contain("dry_run=true"));
    assert!(logs_contain("operation_item_id=42"));
}

#[test]
#[tracing_test::traced_test]
fn a_deletion_operation_is_labelled_deletion() {
    let span = super::operation_span(uuid::Uuid::from_u128(0x7));
    super::record_operation_facts(&span, OperationKind::Deletion, false);
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(logs_contain(r#"kind="deletion""#));
}

fn reference_unit_span() -> (tracing::Span, uuid::Uuid) {
    let reference = uuid::Uuid::from_u128(0x0f5c);
    let span = super::unit_span(
        uuid::Uuid::from_u128(0x1),
        &EntityKey::Uuid(reference),
        OperationKind::Deletion,
        false,
        7,
    );
    (span, reference)
}

/// A Registry Reference is the key, never the identifier: `gts_id` stays empty
/// until the deletion resolves it, so a trace filtered by identifier finds only
/// units that name one.
#[test]
#[tracing_test::traced_test]
fn a_reference_keyed_unit_names_no_identifier_before_resolution() {
    let (span, reference) = reference_unit_span();
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(logs_contain(&format!("entity_key={reference}")));
    assert!(!logs_contain("gts_id="));
}

#[test]
#[tracing_test::traced_test]
fn a_reference_keyed_unit_records_the_identifier_it_resolved_to() {
    let (span, _) = reference_unit_span();
    super::record_resolved_gts_id(&span, "cf.core.example.type.v1~");
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(logs_contain(r#"gts_id="cf.core.example.type.v1~""#));
}

/// What a client generated: logged as is, the bridge from a lost acknowledgement.
#[test]
#[tracing_test::traced_test]
fn an_accepted_operation_names_a_canonical_uuid_key() {
    let key = "0b9f4c2e-7d1a-4e8b-9c3f-5a6b7c8d9e0f";
    super::operation_accepted(
        uuid::Uuid::from_u128(0xab),
        OperationKind::Registration,
        false,
        3,
        true,
        key,
    );

    assert!(logs_contain(&format!("idempotency_key={key}")));
    assert!(logs_contain(r#"idempotency_key_kind="uuid""#));
    assert!(logs_contain("replayed=true"));
    assert!(logs_contain(&uuid::Uuid::from_u128(0xab).to_string()));
}

/// Any other key may carry a caller's data, and a reformatted UUID could merge keys that
/// differ only in case: neither is logged.
#[test]
#[tracing_test::traced_test]
fn an_accepted_operation_never_logs_another_key() {
    for key in [
        "customer-4711-order-SECRET",
        "0B9F4C2E-7D1A-4E8B-9C3F-5A6B7C8D9E0F",
        "{0b9f4c2e-7d1a-4e8b-9c3f-5a6b7c8d9e0f}",
        "0b9f4c2e7d1a4e8b9c3f5a6b7c8d9e0f",
    ] {
        super::operation_accepted(
            uuid::Uuid::from_u128(0xcd),
            OperationKind::Deletion,
            false,
            1,
            false,
            key,
        );
        assert!(!logs_contain(key), "{key}");
    }
    assert!(
        !logs_contain("0b9f4c2e-7d1a-4e8b-9c3f-5a6b7c8d9e0f"),
        "never reformatted"
    );
    assert!(logs_contain(r#"idempotency_key_kind="opaque""#));
    assert!(logs_contain("idempotency_key_len=26"));
}
