use super::*;
use crate::domain::persisted::RunStatus;
use crate::{
    domain::registry::Registry,
    infra::{
        executor::Executor,
        registrar::Registrar,
        storage::repository::tests::{definition, journal, store},
    },
};
use durable_execution_sdk::WorkflowRegistry;
use durable_execution_sdk::registration::{RegistrationState, UnregisterMode, UnregisterOptions};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn reordered_nested_json_replays_both_request_keys() {
    for coalesce in [false, true] {
        let store = store().await;
        let mut first = journal();
        first.input =
            serde_json::from_str(r#"{"nested":{"b":2,"a":1},"array":[{"d":4,"c":3}]}"#).unwrap();
        let mut replay = first.clone();
        replay.run.id = RunId(Uuid::new_v4());
        replay.input =
            serde_json::from_str(r#"{"array":[{"c":3,"d":4}],"nested":{"a":1,"b":2}}"#).unwrap();
        let options = if coalesce {
            StartOptions {
                coalescing_key: Some("replay".into()),
                ..Default::default()
            }
        } else {
            StartOptions {
                idempotency_key: Some("replay".into()),
                ..Default::default()
            }
        };
        let saved = store
            .start(AccessScope::allow_all(), first.clone(), options.clone())
            .await
            .unwrap();
        assert_eq!(
            store
                .start(AccessScope::allow_all(), replay.clone(), options.clone())
                .await
                .unwrap()
                .run_id,
            saved.run_id
        );
        // Simulate an input hash persisted by an older preserve_order host.
        let legacy = hex::encode(
            digest(
                &SHA256,
                br#"{"nested":{"b":2,"a":1},"array":[{"d":4,"c":3}]}"#,
            )
            .as_ref(),
        );
        let scope = AccessScope::allow_all();
        let conn = store.db.conn().unwrap();
        if coalesce {
            coalescing::Entity::update_many()
                .secure()
                .scope_with(&scope)
                .filter(Condition::all().add(coalescing::Column::ActiveRunId.eq(saved.run_id.0)))
                .col_expr(coalescing::Column::InputHash, Expr::value(legacy))
                .exec(&conn)
                .await
                .unwrap();
        } else {
            start_key::Entity::update_many()
                .secure()
                .scope_with(&scope)
                .filter(Condition::all().add(start_key::Column::RunId.eq(saved.run_id.0)))
                .col_expr(start_key::Column::InputHash, Expr::value(legacy))
                .exec(&conn)
                .await
                .unwrap();
        }
        assert_eq!(
            store
                .start(scope.clone(), replay.clone(), options.clone())
                .await
                .unwrap()
                .run_id,
            saved.run_id
        );
        replay.input["nested"]["a"] = serde_json::json!(999);
        assert!(matches!(
            store.start(scope, replay, options).await,
            Err(crate::domain::error::DomainError::IdempotencyConflict(_))
        ));
    }
}

#[test]
fn generation_zero_suffix_cannot_address_a_new_registration_slot() {
    let mut candidate = journal();
    let original = scoped_id(&candidate, "coalesce", "foo:registration:1");
    candidate.registration_generation = 1;
    assert_ne!(original, scoped_id(&candidate, "coalesce", "foo"));
}

#[tokio::test]
async fn released_registration_does_not_reuse_a_cancelled_successor() {
    let store = store().await;
    let registrar = Registrar {
        store: store.clone(),
        registry: Arc::new(Registry::default()),
    };
    let definition = definition();
    registrar.register(definition.clone()).await.unwrap();
    let candidate = journal();
    let old_options = StartOptions {
        coalescing_key: Some("foo:registration:1".into()),
        ..Default::default()
    };
    let active = store
        .start(
            AccessScope::allow_all(),
            candidate.clone(),
            old_options.clone(),
        )
        .await
        .unwrap();
    let mut running = store
        .get(&AccessScope::allow_all(), active.run_id)
        .await
        .unwrap()
        .unwrap();
    let claim = running
        .claim(
            &definition.contract(),
            0,
            Utc::now(),
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    store
        .save(AccessScope::allow_all(), running.revision, running, false)
        .await
        .unwrap();
    let mut successor = candidate.clone();
    successor.run.id = RunId(Uuid::new_v4());
    let parked = store
        .start(AccessScope::allow_all(), successor, old_options)
        .await
        .unwrap();
    assert_ne!(active.run_id, parked.run_id);
    let registration = registrar.registration(&definition.name).await.unwrap();
    registrar
        .unregister(
            &definition.name,
            UnregisterOptions {
                mode: UnregisterMode::CancelAndRelease,
                expected_revision: registration.revision,
            },
        )
        .await
        .unwrap();
    let executor = Executor {
        store: store.clone(),
        registry: registrar.registry.clone(),
        config: crate::config::Config::default(),
    };
    executor.reconcile_definitions().await.unwrap();
    let mut running = store
        .get(&AccessScope::allow_all(), active.run_id)
        .await
        .unwrap()
        .unwrap();
    running.release(claim, Utc::now()).unwrap();
    store
        .save(AccessScope::allow_all(), running.revision, running, false)
        .await
        .unwrap();
    executor.reconcile_definitions().await.unwrap();
    let released = registrar.registration(&definition.name).await.unwrap();
    assert_eq!(released.state, RegistrationState::Released);
    assert_eq!(
        store
            .get(&AccessScope::allow_all(), parked.run_id)
            .await
            .unwrap()
            .unwrap()
            .run
            .status,
        RunStatus::Cancelled
    );
    let active = registrar
        .activate(&definition.name, released.revision)
        .await
        .unwrap();
    assert_eq!(active.generation, 1);
    let mut next = candidate;
    next.run.id = RunId(Uuid::new_v4());
    let options = StartOptions {
        coalescing_key: Some("foo".into()),
        ..Default::default()
    };
    let started = store
        .start(AccessScope::allow_all(), next.clone(), options.clone())
        .await
        .unwrap();
    assert_ne!(started.run_id, parked.run_id);
    let loaded = store
        .get(&AccessScope::allow_all(), started.run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.registration_generation, 1);
    assert_eq!(loaded.run.status, RunStatus::Queued);
    // Existing nonzero-generation slots used the original namespace. Their
    // unique logical key must remain reusable after upgrading the slot ID format.
    let options = StartOptions {
        coalescing_key: Some("legacy".into()),
        ..Default::default()
    };
    next.run.id = RunId(Uuid::new_v4());
    let legacy = store
        .start(AccessScope::allow_all(), next.clone(), options.clone())
        .await
        .unwrap();
    let old_id = Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!(
            "{}:{}:{}:coalesce:legacy:registration:1",
            next.run.owner.tenant_id, next.run.owner.subject_id, next.run.definition
        )
        .as_bytes(),
    );
    let scope = AccessScope::allow_all();
    let conn = store.db.conn().unwrap();
    coalescing::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(coalescing::Column::ActiveRunId.eq(legacy.run_id.0)))
        .col_expr(coalescing::Column::Id, Expr::value(old_id))
        .exec(&conn)
        .await
        .unwrap();
    next.run.id = RunId(Uuid::new_v4());
    assert_eq!(
        store.start(scope, next, options).await.unwrap().run_id,
        legacy.run_id
    );
}
