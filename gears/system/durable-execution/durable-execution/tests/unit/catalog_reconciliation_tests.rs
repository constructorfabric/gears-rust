//! Per-run cancellation races must not discard progress for other runs.
use super::super::repository::tests::{definition as execution_definition, journal, store};
use super::*;
use crate::domain::persisted::RunStatus;
use chrono::Utc;
use durable_execution_sdk::registration::{RegistrationState, UnregisterMode};
use std::time::Duration;

async fn stop_definition(store: &JournalStore) -> Result<Definition, DomainError> {
    store
        .unregister(
            "test.store.v1",
            UnregisterOptions {
                mode: UnregisterMode::CancelAndRelease,
                expected_revision: 0,
            },
        )
        .await
}

#[tokio::test]
async fn torn_run_is_deferred_without_blocking_other_cancellations() {
    let store = store().await;
    let scope = AccessScope::allow_all();
    let torn = journal();
    let torn_id = torn.run.id;
    let valid = journal();
    let valid_id = valid.run.id;
    store.insert(scope.clone(), torn).await.unwrap();
    store.insert(scope.clone(), valid).await.unwrap();
    // A mismatched checkpoint count represents the hydrate Conflict observed
    // when a READ COMMITTED scan crosses a concurrent checkpoint transaction.
    run::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(run::Column::Id.eq(torn_id.0)))
        .col_expr(run::Column::ActivityCount, Expr::value(3))
        .exec(&store.db.conn().unwrap())
        .await
        .unwrap();
    let stopping = stop_definition(&store).await.unwrap();
    store.reconcile_definition(&stopping).await.unwrap();
    assert_eq!(
        store
            .get(&scope, valid_id)
            .await
            .unwrap()
            .unwrap()
            .run
            .status,
        RunStatus::Cancelled
    );
    assert_eq!(
        store.definition("test.store.v1").await.unwrap().state,
        RegistrationState::Stopping
    );
    run::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(run::Column::Id.eq(torn_id.0)))
        .col_expr(run::Column::ActivityCount, Expr::value(2))
        .exec(&store.db.conn().unwrap())
        .await
        .unwrap();
    assert_eq!(
        store
            .get(&scope, torn_id)
            .await
            .unwrap()
            .unwrap()
            .run
            .status,
        RunStatus::Queued
    );
    store.reconcile_definition(&stopping).await.unwrap();
    assert_eq!(
        store
            .get(&scope, torn_id)
            .await
            .unwrap()
            .unwrap()
            .run
            .status,
        RunStatus::Cancelled
    );
    assert_eq!(
        store.definition("test.store.v1").await.unwrap().state,
        RegistrationState::Released
    );
}

#[tokio::test]
async fn cancelling_live_claim_is_not_rewritten_until_a_transition_occurs() {
    let store = store().await;
    let scope = AccessScope::allow_all();
    let mut run = journal();
    let id = run.run.id;
    run.claim(
        &execution_definition().contract(),
        0,
        Utc::now(),
        Duration::from_secs(3600),
    )
    .unwrap()
    .unwrap();
    store.insert(scope.clone(), run).await.unwrap();
    let stopping = stop_definition(&store).await.unwrap();
    store.reconcile_definition(&stopping).await.unwrap();
    let before = store.get(&scope, id).await.unwrap().unwrap();
    assert_eq!(before.run.status, RunStatus::Cancelling);
    assert!(before.cancellation_requested);
    store.reconcile_definition(&stopping).await.unwrap();
    let after = store.get(&scope, id).await.unwrap().unwrap();
    assert_eq!(
        after.revision, before.revision,
        "no-op reconciliation must not race the worker's CAS"
    );
    assert_eq!(after.lease_until, before.lease_until);
    assert_eq!(
        store.definition("test.store.v1").await.unwrap().state,
        RegistrationState::Stopping
    );
}

#[tokio::test]
async fn non_conflict_hydration_failure_is_propagated_and_keeps_stopping() {
    let store = store().await;
    let scope = AccessScope::allow_all();
    let run = journal();
    let id = run.run.id;
    store.insert(scope.clone(), run).await.unwrap();
    run::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(run::Column::Id.eq(id.0)))
        .col_expr(run::Column::Journal, Expr::value(b"invalid json".to_vec()))
        .exec(&store.db.conn().unwrap())
        .await
        .unwrap();
    let stopping = stop_definition(&store).await.unwrap();
    assert!(matches!(
        store.reconcile_definition(&stopping).await,
        Err(StoreError::Codec(_))
    ));
    assert_eq!(
        store.definition("test.store.v1").await.unwrap().state,
        RegistrationState::Stopping
    );
}

#[tokio::test]
async fn worker_definition_preserves_codec_errors_and_missing_definition_category() {
    let store = store().await;
    store
        .ensure_contract(execution_definition().contract())
        .await
        .unwrap();
    assert!(matches!(
        store.worker_definition("missing.definition.v1").await,
        Err(StoreError::Domain(DomainError::DefinitionNotFound(_)))
    ));
    definition::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(definition::Column::Name.eq("test.store.v1")))
        .col_expr(
            definition::Column::State,
            Expr::value(b"invalid json".to_vec()),
        )
        .exec(&store.db.conn().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        store.worker_definition("test.store.v1").await,
        Err(StoreError::Codec(_))
    ));
}
