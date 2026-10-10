use super::*;
use crate::domain::persisted::*;
use async_trait::async_trait;
use durable_execution_sdk::contracts::{
    ActivityDefinition, ActivityInput, ErasedActivity, ExecutionDefinition,
};
use durable_execution_sdk::*;
use std::time::Duration;
use toolkit_db::{ConnectOpts, connect_db, migration_runner::run_migrations_for_testing};
use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};
struct FixtureAuthorization;
#[async_trait]
impl crate::infra::authorization::WorkerAuthorization for FixtureAuthorization {
    async fn scope(&self, _: &str) -> Result<AccessScope, StoreError> {
        Ok(AccessScope::allow_all())
    }
}
struct Step;
#[async_trait]
impl ErasedActivity for Step {
    async fn execute(
        &self,
        _: ActivityContext,
        _: ActivityInput,
    ) -> Result<serde_json::Value, ActivityError> {
        Ok(serde_json::Value::Null)
    }
}
pub fn definition() -> ExecutionDefinition {
    ExecutionDefinition {
        name: "test.store.v1".into(),
        parallel_groups: Vec::new(),
        activities: ["one", "two"]
            .into_iter()
            .map(|id| ActivityDefinition {
                id: ActivityId(id.into()),
                handler: Arc::new(Step),
                timeout: Duration::from_secs(90),
                retry: RetryPolicy::default(),
            })
            .collect(),

        flow: None,
    }
}

#[tokio::test]
async fn parallel_deliveries_checkpoint_before_next_stage() {
    struct Gated {
        started: tokio::sync::mpsc::Sender<String>,
        gate: Arc<tokio::sync::Semaphore>,
    }
    #[async_trait]
    impl ErasedActivity for Gated {
        async fn execute(
            &self,
            ctx: ActivityContext,
            _: ActivityInput,
        ) -> Result<serde_json::Value, ActivityError> {
            self.started.send(ctx.activity_id.0.clone()).await.unwrap();
            self.gate.acquire().await.unwrap().forget();
            Ok(serde_json::Value::String(ctx.activity_id.0))
        }
    }
    let store = store().await;
    let (started, mut observed) = tokio::sync::mpsc::channel(2);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut d = definition();
    d.parallel_groups = vec![d.activities.iter().map(|a| a.id.clone()).collect()];
    for step in &mut d.activities {
        step.handler = Arc::new(Gated {
            started: started.clone(),
            gate: gate.clone(),
        });
    }
    let mut next_stage = d.activities[0].clone();
    next_stage.id = ActivityId("next-stage".into());
    next_stage.handler = Arc::new(Step);
    d.activities.push(next_stage);
    let seed = journal();
    let j = Journal::new(
        seed.run.id,
        seed.run.owner,
        &d.contract(),
        serde_json::Value::Null,
        Utc::now(),
    )
    .unwrap();
    let id = j.run.id;
    store.insert(AccessScope::allow_all(), j).await.unwrap();
    let registry = Arc::new(crate::domain::registry::Registry::default());
    registry.register(d).unwrap();
    let executor = Arc::new(crate::infra::executor::Executor {
        store: store.clone(),
        registry,
        config: crate::config::Config::default(),
    });
    let first = {
        let executor = executor.clone();
        tokio::spawn(async move {
            executor
                .execute(id, 0, tokio_util::sync::CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    let current = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.run.status, RunStatus::Running);
    let deliveries = store
        .deliveries(&AccessScope::allow_all(), Utc::now(), 10)
        .await
        .unwrap();
    assert!(
        deliveries
            .iter()
            .any(|d| d.generation == current.delivery_generation)
    );
    let second = {
        let executor = executor.clone();
        tokio::spawn(async move {
            executor
                .execute(
                    id,
                    current.delivery_generation,
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    let current = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        current
            .run
            .activities
            .iter()
            .filter(|a| a.status == ActivityStatus::Running)
            .count(),
        2
    );
    assert_eq!(current.run.activities[2].status, ActivityStatus::Pending);
    gate.add_permits(2);
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    let current = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.run.status, RunStatus::Queued);
    assert!(
        current.run.activities[..2]
            .iter()
            .all(|a| a.result.is_some())
    );
    executor
        .execute(
            id,
            current.delivery_generation,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .get(&AccessScope::allow_all(), id)
            .await
            .unwrap()
            .unwrap()
            .run
            .status,
        RunStatus::Succeeded
    );
}
struct ParkedDelivery;
#[async_trait::async_trait]
impl toolkit_db::outbox::LeasedMessageHandler for ParkedDelivery {
    async fn handle(
        &self,
        _: &toolkit_db::outbox::OutboxMessage,
    ) -> toolkit_db::outbox::MessageResult {
        toolkit_db::outbox::MessageResult::Retry
    }
}
#[tokio::test]
async fn normalized_rows_preserve_attempts_and_keep_results_out_of_run_header() {
    let store = store().await;
    let scope = AccessScope::allow_all();
    let mut j = journal();
    let id = j.run.id;
    store.insert(scope.clone(), j.clone()).await.unwrap();
    let first = j
        .claim(
            &definition().contract(),
            0,
            Utc::now(),
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    store.save(scope.clone(), 0, j, false).await.unwrap();
    let mut j = store.get(&scope, id).await.unwrap().unwrap();
    j.fail(
        first,
        &ActivityError::permanent("invalid_result"),
        &definition().contract(),
        Utc::now(),
        0,
    )
    .unwrap();
    store
        .save(scope.clone(), j.revision, j, false)
        .await
        .unwrap();
    let mut j = store.get(&scope, id).await.unwrap().unwrap();
    assert_eq!(
        j.run.activities[0].attempt_history[0].error_code.as_deref(),
        Some("invalid_result")
    );
    j.continue_execution(0, RunStatus::Failed, &definition().contract(), Utc::now())
        .unwrap();
    store
        .save(scope.clone(), j.revision, j, true)
        .await
        .unwrap();
    let mut j = store.get(&scope, id).await.unwrap().unwrap();
    let next = j
        .claim(
            &definition().contract(),
            j.delivery_generation,
            Utc::now(),
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    store
        .save(scope.clone(), j.revision, j, false)
        .await
        .unwrap();
    let mut j = store.get(&scope, id).await.unwrap().unwrap();
    j.complete(
        next,
        serde_json::json!({"saved": "checkpoint-result"}),
        Utc::now(),
    )
    .unwrap();
    store
        .save(scope.clone(), j.revision, j, true)
        .await
        .unwrap();
    let restored = store.get(&scope, id).await.unwrap().unwrap();
    assert_eq!(restored.run.activities[0].attempt_history.len(), 2);
    assert_eq!(restored.run.activities[0].attempt_history[0].epoch, 0);
    assert_eq!(restored.run.activities[0].attempt_history[1].epoch, 1);
    assert_eq!(
        restored.run.activities[0].attempt_history[1].status,
        ActivityStatus::Succeeded
    );
    assert_eq!(restored.run.previous_executions.len(), 1);
    let conn = store.db.conn().unwrap();
    let row = run::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(run::Column::Id.eq(id.0)))
        .one(&conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.storage_version, 1);
    let header: Journal = serde_json::from_slice(&row.journal).unwrap();
    assert!(header.run.activities.is_empty());
    assert!(header.run.previous_executions.is_empty());
    assert!(
        !String::from_utf8(row.journal)
            .unwrap()
            .contains("checkpoint-result")
    );
    // A later transition cannot overwrite the completed attempt record.
    let mut tampered = restored;
    tampered.run.activities[0].attempt_history[0].error_code = Some("changed".into());
    assert!(
        store
            .save(scope, tampered.revision, tampered, false)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn legacy_blob_is_readable_and_converted_on_next_fenced_write() {
    let store = store().await;
    let scope = AccessScope::allow_all();
    let mut j = journal();
    let id = j.run.id;
    let conn = store.db.conn().unwrap();
    secure_insert::<run::Entity>(
        run::ActiveModel {
            id: Set(id.0),
            tenant_id: Set(j.run.owner.tenant_id),
            owner_id: Set(j.run.owner.subject_id),
            definition: Set(j.run.definition.clone()),
            revision: Set(0),
            registration_generation: Set(0),
            status: Set("queued".into()),
            journal: Set(serde_json::to_vec(&j).unwrap()),
            storage_version: Set(0),
            activity_count: Set(0),
            lease_until: Set(None),
            due_at: Set(j.run.next_attempt_at),
            created_at: Set(j.run.created_at),
            updated_at: Set(j.run.updated_at),
        },
        &scope,
        &conn,
    )
    .await
    .unwrap();
    assert_eq!(
        store
            .get(&scope, id)
            .await
            .unwrap()
            .unwrap()
            .run
            .activities
            .len(),
        2
    );
    j.claim(
        &definition().contract(),
        0,
        Utc::now(),
        Duration::from_mins(2),
    )
    .unwrap()
    .unwrap();
    store.save(scope.clone(), 0, j, false).await.unwrap();
    let restored = store.get(&scope, id).await.unwrap().unwrap();
    assert_eq!(restored.run.activities.len(), 2);
    assert_eq!(restored.run.activities[0].attempt_history.len(), 1);
}
#[tokio::test]
async fn events_are_fenced_replayable_and_private_with_consistent_snapshot_cursor() {
    let store = store().await;
    let scope = AccessScope::allow_all();
    let mut j = journal();
    j.input = serde_json::json!({"secret": "private input"});
    let id = j.run.id;
    store.insert(scope.clone(), j.clone()).await.unwrap();
    let snapshot = store.get(&scope, id).await.unwrap().unwrap();
    let cursor = crate::domain::view::cursor(snapshot.revision).unwrap();
    assert!(
        store
            .events(&scope, id, 0, 200)
            .await
            .unwrap()
            .iter()
            .all(|e| e.sequence <= cursor)
    );
    let claim = j
        .claim(
            &definition().contract(),
            0,
            Utc::now(),
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    store.save(scope.clone(), 0, j, false).await.unwrap();
    let replay = store.events(&scope, id, cursor, 200).await.unwrap();
    assert_eq!(
        replay
            .iter()
            .filter(|e| e.activity_id.is_some() && e.kind == EventKind::Started)
            .count(),
        1
    );
    let mut j = store.get(&scope, id).await.unwrap().unwrap();
    j.complete(
        claim,
        serde_json::json!({"secret": "private checkpoint"}),
        Utc::now(),
    )
    .unwrap();
    assert!(store.save(scope.clone(), 0, j.clone(), true).await.is_err());
    assert_eq!(
        store.events(&scope, id, cursor, 200).await.unwrap().len(),
        replay.len()
    );
    store.save(scope.clone(), 1, j, true).await.unwrap();
    let first = store.events(&scope, id, cursor, 1).await.unwrap();
    let next = store
        .events(&scope, id, first[0].sequence, 200)
        .await
        .unwrap();
    assert!(next.iter().all(|e| e.sequence > first[0].sequence));
    let encoded = serde_json::to_string(&next).unwrap();
    assert!(!encoded.contains("private"));
    let other = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::r#in(
        pep_properties::OWNER_ID,
        vec![Uuid::new_v4().into()],
    )]));
    assert!(store.events(&other, id, 0, 200).await.unwrap().is_empty());
}
struct CaptureDelivery(tokio::sync::mpsc::UnboundedSender<crate::infra::apalis::Delivery>);
#[async_trait::async_trait]
impl toolkit_db::outbox::LeasedMessageHandler for CaptureDelivery {
    async fn handle(
        &self,
        message: &toolkit_db::outbox::OutboxMessage,
    ) -> toolkit_db::outbox::MessageResult {
        let delivery = serde_json::from_slice(&message.payload).unwrap();
        self.0.send(delivery).unwrap();
        toolkit_db::outbox::MessageResult::Ok
    }
}
#[tokio::test]
async fn framework_delivery_is_atomic_with_checkpoint_and_has_no_input() {
    let store = store().await;
    let scope = AccessScope::allow_all();
    let mut j = journal();
    j.input = serde_json::json!({"private": "must-not-be-in-queue"});
    let id = j.run.id;
    store.insert(scope.clone(), j.clone()).await.unwrap();
    let claim = j
        .claim(
            &definition().contract(),
            0,
            Utc::now(),
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    store.save(scope.clone(), 0, j, false).await.unwrap();
    let mut completed = store.get(&scope, id).await.unwrap().unwrap();
    completed
        .complete(claim, serde_json::json!("checkpoint"), Utc::now())
        .unwrap();
    assert!(
        store
            .save(scope.clone(), 0, completed.clone(), true)
            .await
            .is_err()
    );
    store.save(scope.clone(), 1, completed, true).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = store.start_outbox(CaptureDelivery(tx)).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.run_id, id.0.to_string());
    assert_eq!(first.activity_id, "one");
    assert_eq!(second.activity_id, "two");
    assert!(second.generation > first.generation);
    assert_eq!(
        serde_json::to_value(&first)
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        3
    );
    handle.stop().await;
    assert!(rx.try_recv().is_err());
}
#[tokio::test]
async fn unavailable_publisher_rolls_back_run_and_intent() {
    let fixture = store().await;
    let store = JournalStore::new(fixture.db.clone());
    let j = journal();
    let id = j.run.id;
    let scope = AccessScope::allow_all();
    assert!(matches!(
        store.insert(scope.clone(), j).await,
        Err(StoreError::DeliveryUnavailable)
    ));
    assert!(store.get(&scope, id).await.unwrap().is_none());
    assert!(
        store
            .deliveries(&scope, Utc::now(), 100)
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn legacy_intent_handoff_is_idempotent() {
    let store = store().await;
    let scope = AccessScope::allow_all();
    let j = journal();
    store.insert(scope.clone(), j.clone()).await.unwrap();
    // Simulate an intent written by the old binary after the additive migration.
    // Its original framework command is a harmless duplicate for this fixture.
    let conn = store.db.conn().unwrap();
    outbox::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(outbox::Column::RunId.eq(j.run.id.0)))
        .col_expr(outbox::Column::Forwarded, Expr::value(false))
        .exec(&conn)
        .await
        .unwrap();
    store.forward_legacy_deliveries().await.unwrap();
    store.forward_legacy_deliveries().await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = store.start_outbox(CaptureDelivery(tx)).await.unwrap();
    for _ in 0..2 {
        let d = tokio::time::timeout(Duration::from_secs(15), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(d.run_id, j.run.id.0.to_string());
        assert_eq!(d.generation, 0);
    }
    handle.stop().await;
    assert!(rx.try_recv().is_err());
}
#[cfg(not(feature = "integration"))]
pub async fn isolated_url() -> String {
    "sqlite::memory:".into()
}
#[cfg(feature = "integration")]
pub async fn isolated_url() -> crate::test_postgres::Database {
    use sea_orm::ConnectionTrait;
    let mut database = crate::test_postgres::database().await;
    let schema = format!("durable_test_{}", Uuid::new_v4().simple());
    let admin = sea_orm::Database::connect(&database.url).await.unwrap();
    admin
        .execute_unprepared(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    admin.close().await.unwrap();
    database.url = format!(
        "{}{}options=-csearch_path%3D{schema}%2Cpublic",
        database.url,
        if database.url.contains('?') { "&" } else { "?" }
    );
    database
}
#[tokio::test]
async fn pinned_start_rejects_changed_definition_before_persisting_any_run() {
    let store = store().await;
    let candidate = journal();
    let id = candidate.run.id;
    let result = store
        .start(
            AccessScope::allow_all(),
            candidate.clone(),
            StartOptions {
                expected_fingerprint: Some("different-deployment".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(
        result,
        Err(crate::domain::error::DomainError::DefinitionMismatch)
    ));
    assert!(
        store
            .get(&AccessScope::allow_all(), id)
            .await
            .unwrap()
            .is_none()
    );
    let expected = candidate.fingerprint.clone();
    assert!(
        store
            .start(
                AccessScope::allow_all(),
                candidate,
                StartOptions {
                    expected_fingerprint: Some(expected),
                    ..Default::default()
                }
            )
            .await
            .is_ok()
    );
}

pub async fn store() -> JournalStore {
    let database = isolated_url().await;
    let store = store_at(&database).await;
    #[cfg(feature = "integration")]
    let store = {
        let mut store = store;
        store.test_database = Some(Arc::new(database));
        store
    };
    store
}
pub async fn store_at(url: &str) -> JournalStore {
    let db = connect_db(
        url,
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    run_migrations_for_testing(
        &db,
        vec![
            Box::new(super::super::migration::Migration),
            Box::new(super::super::delivery_migration::Migration),
            Box::new(super::super::normalized_migration::Migration),
            Box::new(super::super::events_migration::Migration),
            Box::new(super::super::definition_migration::Migration),
        ],
    )
    .await
    .unwrap();
    let store = JournalStore::new(Arc::new(DBProvider::new(db)))
        .with_authorization(Arc::new(FixtureAuthorization));
    let handle = store.start_outbox(ParkedDelivery).await.unwrap();
    handle.stop().await;
    store
}
pub fn journal() -> Journal {
    Journal::new(
        RunId(Uuid::new_v4()),
        ExecutionOwner {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        },
        &definition().contract(),
        serde_json::Value::Null,
        Utc::now(),
    )
    .unwrap()
}

#[tokio::test]
async fn stop_reason_survives_reload_and_stays_on_its_epoch() {
    let store = store().await;
    let mut journal = journal();
    let id = journal.run.id;
    let now = Utc::now();
    assert!(
        journal
            .request_cancel_at(0, now, Some("Stop requested by operator"))
            .unwrap()
    );
    store
        .insert(AccessScope::allow_all(), journal)
        .await
        .unwrap();
    let mut loaded = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.run.stop_reason.as_deref(),
        Some("Stop requested by operator")
    );
    assert_eq!(loaded.run.status, RunStatus::Cancelled);
    let definition = definition().contract();
    assert!(
        loaded
            .continue_execution(0, RunStatus::Cancelled, &definition, now)
            .unwrap()
    );
    assert_eq!(loaded.run.stop_reason, None);
    assert_eq!(
        loaded.run.previous_executions[0].stop_reason.as_deref(),
        Some("Stop requested by operator")
    );
    store
        .save(AccessScope::allow_all(), loaded.revision, loaded, false)
        .await
        .unwrap();
    let resumed = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.run.execution_epoch, 1);
    assert_eq!(resumed.run.stop_reason, None);
    assert_eq!(
        resumed.run.previous_executions[0].stop_reason.as_deref(),
        Some("Stop requested by operator")
    );
}

#[tokio::test]
async fn lost_delivery_is_reopened_only_while_current_generation_is_unclaimed() {
    let store = store().await;
    let mut j = journal();
    let scope = AccessScope::allow_all();
    let now = Utc::now();
    store.insert(scope.clone(), j.clone()).await.unwrap();
    let delivery = store.deliveries(&scope, now, 10).await.unwrap().remove(0);
    store
        .mark_delivered(&scope, delivery.id, now)
        .await
        .unwrap();
    store
        .recover_unclaimed_deliveries(now, now - chrono::Duration::seconds(1))
        .await
        .unwrap();
    assert!(store.deliveries(&scope, now, 10).await.unwrap().is_empty());
    store.recover_unclaimed_deliveries(now, now).await.unwrap();
    assert_eq!(store.deliveries(&scope, now, 10).await.unwrap().len(), 1);
    store
        .mark_delivered(&scope, delivery.id, now)
        .await
        .unwrap();
    j.claim(&definition().contract(), 0, now, Duration::from_mins(2))
        .unwrap()
        .unwrap();
    store
        .save(scope.clone(), j.revision, j, false)
        .await
        .unwrap();
    store.recover_unclaimed_deliveries(now, now).await.unwrap();
    assert!(store.deliveries(&scope, now, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn checkpoint_and_next_delivery_commit_together_and_cas_rejects_stale_writer() {
    let store = store().await;
    let mut j = journal();
    let scope = AccessScope::allow_all();
    store.insert(scope.clone(), j.clone()).await.unwrap();
    assert_eq!(
        store
            .deliveries(&scope, Utc::now(), 10)
            .await
            .unwrap()
            .len(),
        1
    );
    let c = j
        .claim(
            &definition().contract(),
            0,
            Utc::now(),
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    store
        .save(scope.clone(), 0, j.clone(), false)
        .await
        .unwrap();
    assert!(matches!(
        store.save(scope.clone(), 0, j.clone(), false).await,
        Err(StoreError::Conflict)
    ));
    j = store.get(&scope, j.run.id).await.unwrap().unwrap();
    j.complete(c, serde_json::json!("saved"), Utc::now())
        .unwrap();
    store.save(scope.clone(), 1, j.clone(), true).await.unwrap();
    let restored = store.get(&scope, j.run.id).await.unwrap().unwrap();
    assert_eq!(restored.revision, 2);
    assert_eq!(restored.run.activities[0].status, ActivityStatus::Succeeded);
    assert_eq!(
        store
            .deliveries(&scope, Utc::now(), 10)
            .await
            .unwrap()
            .len(),
        2
    );
    // Inserting the same outbox generation fails and rolls back its CAS.
    assert!(
        store
            .save(scope.clone(), 2, restored.clone(), true)
            .await
            .is_err()
    );
    assert_eq!(
        store.get(&scope, j.run.id).await.unwrap().unwrap().revision,
        2
    );
}
#[tokio::test]
async fn scope_prevents_cross_owner_reads_and_writes() {
    let store = store().await;
    let j = journal();
    store
        .insert(AccessScope::allow_all(), j.clone())
        .await
        .unwrap();
    let other = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::r#in(
        pep_properties::OWNER_ID,
        vec![Uuid::new_v4().into()],
    )]));
    assert!(store.get(&other, j.run.id).await.unwrap().is_none());
    assert!(store.save(other, 0, j, false).await.is_err());
}
#[tokio::test]
async fn idempotency_replays_result_and_rejects_changed_input() {
    let store = store().await;
    let j = journal();
    let scope = AccessScope::allow_all();
    let opts = StartOptions {
        expected_fingerprint: None,
        idempotency_key: Some("one-request".into()),
        coalescing_key: None,
    };
    let first = store
        .start(scope.clone(), j.clone(), opts.clone())
        .await
        .unwrap();
    let mut other = j.clone();
    other.run.id = RunId(Uuid::new_v4());
    let again = store
        .start(scope.clone(), other.clone(), opts.clone())
        .await
        .unwrap();
    assert_eq!(first, again);
    other.input = serde_json::json!({"changed": true});
    assert_eq!(
        store.start(scope, other, opts).await.unwrap_err(),
        crate::domain::error::DomainError::IdempotencyConflict("idempotency_key")
    );
}
#[tokio::test]
async fn coalescing_creates_one_parked_successor_and_promotes_after_completion() {
    let store = store().await;
    let mut j = journal();
    let scope = AccessScope::allow_all();
    let opts = StartOptions {
        expected_fingerprint: None,
        idempotency_key: None,
        coalescing_key: Some("pr-1".into()),
    };
    let first = store
        .start(scope.clone(), j.clone(), opts.clone())
        .await
        .unwrap();
    let mut candidate = j.clone();
    candidate.run.id = RunId(Uuid::new_v4());
    assert_eq!(
        store
            .start(scope.clone(), candidate.clone(), opts.clone())
            .await
            .unwrap()
            .run_id,
        first.run_id
    );
    j = store.get(&scope, first.run_id).await.unwrap().unwrap();
    let claim = j
        .claim(
            &definition().contract(),
            0,
            Utc::now(),
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    store
        .save(scope.clone(), j.revision, j, false)
        .await
        .unwrap();
    let successor = store
        .start(scope.clone(), candidate.clone(), opts.clone())
        .await
        .unwrap();
    assert_ne!(successor.run_id, first.run_id);
    assert_eq!(successor.requested_generation, 2);
    candidate.run.id = RunId(Uuid::new_v4());
    assert_eq!(
        store
            .start(scope.clone(), candidate, opts)
            .await
            .unwrap()
            .run_id,
        successor.run_id
    );
    assert!(
        store
            .get(&scope, successor.run_id)
            .await
            .unwrap()
            .unwrap()
            .run
            .next_attempt_at
            .is_none()
    );
    store.promote_successors().await.unwrap();
    assert!(
        store
            .get(&scope, successor.run_id)
            .await
            .unwrap()
            .unwrap()
            .run
            .next_attempt_at
            .is_none()
    );
    let mut active = store.get(&scope, first.run_id).await.unwrap().unwrap();
    active
        .fail(
            claim,
            &ActivityError::permanent("failed"),
            &definition().contract(),
            Utc::now(),
            0,
        )
        .unwrap();
    store
        .save(scope.clone(), active.revision, active, false)
        .await
        .unwrap();
    store.promote_successors().await.unwrap();
    assert!(
        store
            .get(&scope, successor.run_id)
            .await
            .unwrap()
            .unwrap()
            .run
            .next_attempt_at
            .is_some()
    );
    let deliveries = store.deliveries(&scope, Utc::now(), 100).await.unwrap();
    assert_eq!(
        deliveries
            .iter()
            .filter(|d| d.run_id == successor.run_id)
            .count(),
        1
    );
    store.promote_successors().await.unwrap();
    assert_eq!(
        store
            .deliveries(&scope, Utc::now(), 100)
            .await
            .unwrap()
            .len(),
        deliveries.len()
    );
}
#[tokio::test]
async fn concurrent_starts_converge_on_one_idempotent_run() {
    let store = store().await;
    let j = journal();
    let scope = AccessScope::allow_all();
    let mut other = j.clone();
    other.run.id = RunId(Uuid::new_v4());
    let opts = StartOptions {
        expected_fingerprint: None,
        idempotency_key: Some("concurrent".into()),
        coalescing_key: None,
    };
    let (a, b) = tokio::join!(
        store.start(scope.clone(), j, opts.clone()),
        store.start(scope.clone(), other, opts)
    );
    assert_eq!(a.unwrap().run_id, b.unwrap().run_id);
    assert_eq!(
        store
            .deliveries(&scope, Utc::now(), 100)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn queued_refresh_invalidates_a_stale_worker_claim() {
    let store = store().await;
    let mut stale = journal();
    let scope = AccessScope::allow_all();
    let opts = StartOptions {
        expected_fingerprint: None,
        idempotency_key: None,
        coalescing_key: Some("claim-race".into()),
    };
    store
        .start(scope.clone(), stale.clone(), opts.clone())
        .await
        .unwrap();
    let mut refresh = stale.clone();
    refresh.run.id = RunId(Uuid::new_v4());
    store.start(scope.clone(), refresh, opts).await.unwrap();
    stale
        .claim(
            &definition().contract(),
            0,
            Utc::now(),
            Duration::from_mins(2),
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .save(scope.clone(), stale.revision, stale.clone(), false)
            .await,
        Err(StoreError::Conflict)
    ));
    let current = store.get(&scope, stale.run.id).await.unwrap().unwrap();
    assert_eq!(current.run.status, RunStatus::Queued);
    assert_eq!(current.run.activities[0].attempts, 0);
}

struct CheckpointStep {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    second: bool,
}
#[async_trait]
impl ErasedActivity for CheckpointStep {
    async fn execute(
        &self,
        _: ActivityContext,
        input: ActivityInput,
    ) -> Result<serde_json::Value, ActivityError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.second {
            assert_eq!(
                input.previous_results.get("one"),
                Some(&serde_json::json!({"checkpoint": 42}))
            );
        }
        Ok(serde_json::json!({"checkpoint": 42}))
    }
}
#[tokio::test]
async fn recreated_executor_skips_saved_step_and_ignores_duplicate_delivery() {
    use crate::{config::Config, domain::registry::Registry, infra::executor::Executor};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio_util::sync::CancellationToken;
    let store = store().await;
    let scope = AccessScope::allow_all();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut d = definition();
    for (i, step) in d.activities.iter_mut().enumerate() {
        step.handler = Arc::new(CheckpointStep {
            calls: calls.clone(),
            second: i == 1,
        });
    }
    let registry = Arc::new(Registry::default());
    registry.register(d).unwrap();
    registry.validate_bindings().unwrap();
    let j = journal();
    let id = j.run.id;
    store.insert(scope.clone(), j).await.unwrap();
    let executor = Executor {
        store: store.clone(),
        registry: registry.clone(),
        config: Config::default(),
    };
    executor
        .execute(id, 0, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(executor);
    let executor = Executor {
        store: store.clone(),
        registry,
        config: Config::default(),
    };
    executor
        .execute(id, 0, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let checkpoint = store.get(&scope, id).await.unwrap().unwrap();
    let (a, b) = tokio::join!(
        executor.execute(id, checkpoint.delivery_generation, CancellationToken::new()),
        executor.execute(id, checkpoint.delivery_generation, CancellationToken::new())
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store.get(&scope, id).await.unwrap().unwrap().run.status,
        RunStatus::Succeeded
    );
}

#[cfg(feature = "integration")]
#[path = "../integration/runtime_delivery_tests.rs"]
mod integration;
