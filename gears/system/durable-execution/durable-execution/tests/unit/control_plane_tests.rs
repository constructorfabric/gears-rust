//! Recovery progress and authorization cost of catalog reconciliation.
use super::*;
use crate::domain::persisted::*;
use crate::infra::storage::{
    definition_migration, delivery_migration, events_migration, migration, normalized_migration,
    repository::tests::definition, run,
};
use durable_execution_sdk::contracts::ExecutionContract;
use durable_execution_sdk::registration::{RegistrationState, UnregisterMode, UnregisterOptions};
use durable_execution_sdk::*;
use parking_lot::Mutex;
use sea_orm::ActiveValue::Set;
use std::sync::atomic::{AtomicBool, Ordering};
use toolkit_db::{
    ConnectOpts, DBProvider, DbError, connect_db, migration_runner::run_migrations_for_testing,
    secure::secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

#[derive(Default)]
struct Authorization {
    actions: Mutex<Vec<String>>,
    deny_release: AtomicBool,
}
#[async_trait::async_trait]
impl crate::infra::authorization::WorkerAuthorization for Authorization {
    async fn scope(&self, action: &str) -> Result<AccessScope, StoreError> {
        self.actions.lock().push(action.to_owned());
        if action == "definition:release" && self.deny_release.load(Ordering::SeqCst) {
            return Err(StoreError::AuthorizationUnavailable);
        }
        Ok(AccessScope::allow_all())
    }
}
impl Authorization {
    fn take_actions(&self) -> Vec<String> {
        std::mem::take(&mut *self.actions.lock())
    }
}
struct Parked;
#[async_trait::async_trait]
impl toolkit_db::outbox::LeasedMessageHandler for Parked {
    async fn handle(
        &self,
        _: &toolkit_db::outbox::OutboxMessage,
    ) -> toolkit_db::outbox::MessageResult {
        toolkit_db::outbox::MessageResult::Retry
    }
}

async fn fixture() -> (Executor, Arc<DBProvider<DbError>>, Arc<Authorization>) {
    let db = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            min_conns: Some(1),
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    run_migrations_for_testing(
        &db,
        vec![
            Box::new(migration::Migration),
            Box::new(delivery_migration::Migration),
            Box::new(normalized_migration::Migration),
            Box::new(events_migration::Migration),
            Box::new(definition_migration::Migration),
        ],
    )
    .await
    .unwrap();
    let provider = Arc::new(DBProvider::new(db));
    let authorization = Arc::new(Authorization::default());
    let store = JournalStore::new(provider.clone()).with_authorization(authorization.clone());
    let handle = store.start_outbox(Parked).await.unwrap();
    handle.stop().await;
    (
        Executor {
            store,
            registry: Arc::new(Registry::default()),
            config: Config::default(),
        },
        provider,
        authorization,
    )
}
fn expired_legacy(contract: &ExecutionContract, at: DateTime<Utc>) -> Journal {
    let mut journal = Journal::new(
        RunId(Uuid::new_v4()),
        ExecutionOwner {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        },
        contract,
        serde_json::Value::Null,
        at,
    )
    .unwrap();
    let claim = journal
        .claim(contract, 0, at, Duration::from_secs(30))
        .unwrap()
        .unwrap();
    journal
        .complete(claim, serde_json::json!("kept-checkpoint"), at)
        .unwrap();
    journal
        .claim(
            contract,
            journal.delivery_generation,
            at,
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
    journal.registration_contract = None;
    journal
}
async fn insert_legacy(provider: &DBProvider<DbError>, journal: &Journal) {
    secure_insert::<run::Entity>(
        run::ActiveModel {
            id: Set(journal.run.id.0),
            tenant_id: Set(journal.run.owner.tenant_id),
            owner_id: Set(journal.run.owner.subject_id),
            definition: Set(journal.run.definition.clone()),
            revision: Set(journal.revision),
            registration_generation: Set(0),
            status: Set("running".into()),
            journal: Set(serde_json::to_vec(journal).unwrap()),
            storage_version: Set(0),
            activity_count: Set(0),
            lease_until: Set(journal.lease_until),
            due_at: Set(journal.run.next_attempt_at),
            created_at: Set(journal.run.created_at),
            updated_at: Set(journal.run.updated_at),
        },
        &AccessScope::allow_all(),
        &provider.conn().unwrap(),
    )
    .await
    .unwrap();
}
fn assert_recovered(before: &Journal, after: &Journal) {
    assert_eq!(after.run.status, RunStatus::Queued);
    assert_eq!(after.lease_until, None);
    assert!(after.run.next_attempt_at.is_some());
    assert!(after.delivery_generation > before.delivery_generation);
    assert_eq!(after.run.activities[0].status, ActivityStatus::Succeeded);
    assert_eq!(
        after.run.activities[0].result,
        Some(serde_json::json!("kept-checkpoint"))
    );
    assert_eq!(after.run.activities[1].status, ActivityStatus::Pending);
    assert_eq!(after.run.activities[1].attempts, 1);
}

#[tokio::test]
async fn recovery_skips_missing_catalog_beyond_first_page_and_retries_after_registration() {
    let (executor, provider, _) = fixture().await;
    let mut absent = definition().contract();
    absent.name = "recovery.absent.v1".into();
    let mut registered = definition().contract();
    registered.name = "recovery.registered.v1".into();
    executor
        .store
        .ensure_contract(registered.clone())
        .await
        .unwrap();
    let old = Utc::now() - chrono::Duration::hours(2);
    let mut deferred = Vec::new();
    // The first page and part of the second share one expired lease; paging
    // must use the ID tie breaker and still reach the later registered run.
    for _ in 0..101 {
        let journal = expired_legacy(&absent, old);
        insert_legacy(&provider, &journal).await;
        deferred.push(journal);
    }
    let valid = expired_legacy(&registered, old + chrono::Duration::minutes(1));
    insert_legacy(&provider, &valid).await;
    executor.recover().await.unwrap();
    let scope = AccessScope::allow_all();
    let recovered = executor
        .store
        .get(&scope, valid.run.id)
        .await
        .unwrap()
        .unwrap();
    assert_recovered(&valid, &recovered);
    for before in &deferred {
        let after = executor
            .store
            .get(&scope, before.run.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(before).unwrap(),
            "a missing catalog must defer without altering attempts, checkpoint or lease"
        );
    }
    executor.store.ensure_contract(absent).await.unwrap();
    executor.recover().await.unwrap();
    for before in &deferred {
        let after = executor
            .store
            .get(&scope, before.run.id)
            .await
            .unwrap()
            .unwrap();
        assert_recovered(before, &after);
    }
}

#[tokio::test]
async fn reconciliation_uses_one_authorized_snapshot_but_refreshes_mutation_permission() {
    let (executor, _, authorization) = fixture().await;
    for index in 0..40 {
        let mut contract = definition().contract();
        contract.name = format!("catalog.definition{index}.v1");
        executor.store.ensure_contract(contract).await.unwrap();
    }
    authorization.take_actions();
    executor.reconcile_definitions().await.unwrap();
    assert_eq!(
        authorization.take_actions(),
        vec!["definition:list"],
        "unchanged definitions must not each exchange credentials and reload the catalog"
    );

    let stopping = executor
        .store
        .unregister(
            "catalog.definition0.v1",
            UnregisterOptions {
                mode: UnregisterMode::CancelAndRelease,
                expected_revision: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(stopping.state, RegistrationState::Stopping);
    authorization.take_actions();
    authorization.deny_release.store(true, Ordering::SeqCst);
    assert!(matches!(
        executor.reconcile_definitions().await,
        Err(StoreError::AuthorizationUnavailable)
    ));
    let denied_actions = authorization.take_actions();
    assert!(
        denied_actions
            .iter()
            .any(|action| action == "cancel_definition")
    );
    assert!(
        denied_actions
            .iter()
            .any(|action| action == "definition:release")
    );
    assert_eq!(
        executor
            .store
            .definition("catalog.definition0.v1")
            .await
            .unwrap()
            .state,
        RegistrationState::Stopping,
        "a fresh denial must preserve the operation for retry"
    );

    authorization.deny_release.store(false, Ordering::SeqCst);
    authorization.take_actions();
    executor.reconcile_definitions().await.unwrap();
    let retry_actions = authorization.take_actions();
    assert_eq!(
        retry_actions
            .iter()
            .filter(|action| *action == "definition:list")
            .count(),
        1
    );
    assert_eq!(
        retry_actions
            .iter()
            .filter(|action| *action == "definition:get")
            .count(),
        1,
        "only the stopping row needs a reload after mutation"
    );
    assert!(
        retry_actions
            .iter()
            .any(|action| action == "definition:release")
    );
    assert_eq!(
        executor
            .store
            .definition("catalog.definition0.v1")
            .await
            .unwrap()
            .state,
        RegistrationState::Released
    );
}
