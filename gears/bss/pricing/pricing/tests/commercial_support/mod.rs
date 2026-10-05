//! Shared immutable receipt fixtures for `SQLite` and `PostgreSQL` repositories.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_pricing::infra::{
    commercial_terms::wire,
    storage::{
        entity::{acceptance, commercial_command, hold},
        repo::{acceptance_repo, hold_repo},
    },
};
use bss_pricing_sdk::acceptance::HeldBindings;
use uuid::Uuid;
pub fn row() -> acceptance::Model {
    acceptance_repo::from_receipt(
        &wire::decode_acceptance(include_str!("../commercial_receipts/acceptance-v1.json"))
            .unwrap(),
        Uuid::from_u128(99),
    )
    .unwrap()
}
pub fn command(a: &acceptance::Model) -> commercial_command::Model {
    commercial_command::Model {
        id: Uuid::new_v4(),
        tenant_id: a.tenant_id,
        caller_tenant_id: Uuid::from_u128(90),
        caller_id: Uuid::from_u128(91),
        operation: "check".into(),
        idempotency_key: "accept-1".into(),
        request_digest: a.request_digest.clone(),
        receipt_kind: "acceptance".into(),
        receipt_id: a.id,
        acceptance_id: Some(a.id),
        hold_id: None,
    }
}
pub fn held(a: &acceptance::Model) -> hold::Model {
    let receipt = wire::decode_acceptance(&a.receipt_json).unwrap();
    hold_repo::from_receipt(
        a.tenant_id,
        &HeldBindings {
            hold_id: Uuid::new_v4(),
            acceptance_id: a.id,
            terms_digest: receipt.terms_digest,
            activation_at: receipt.query.start_at,
            bindings: receipt.bindings,
        },
        Uuid::from_u128(99),
        receipt.accepted_at,
    )
    .unwrap()
}
