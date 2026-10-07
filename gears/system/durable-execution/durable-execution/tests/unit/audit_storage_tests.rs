//! Regression coverage for persisted checkpoint reads and public admission.
use super::tests::{definition as execution_definition, journal, store};
use super::*;
use crate::domain::persisted::*;
use durable_execution_sdk::observation::{RunStatus as PublicRunStatus, WorkflowQuery};
use durable_execution_sdk::*;
use std::time::Duration;
use toolkit_db::secure::{SecureDeleteExt, secure_insert_many};
use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};

fn restricted(property: &'static str, id: Uuid) -> AccessScope {
    AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::r#in(
        property,
        vec![id.into()],
    )]))
}
fn all_query(since: DateTime<Utc>) -> WorkflowQuery {
    WorkflowQuery {
        since,
        status: None,
        limit: 100,
        offset: 0,
    }
}
fn header(j: &Journal, storage_version: i32) -> run::ActiveModel {
    run::ActiveModel {
        id: Set(j.run.id.0),
        tenant_id: Set(j.run.owner.tenant_id),
        owner_id: Set(j.run.owner.subject_id),
        definition: Set(j.run.definition.clone()),
        revision: Set(j.revision),
        registration_generation: Set(0),
        status: Set(status(j.run.status).into()),
        journal: Set(serde_json::to_vec(j).unwrap()),
        storage_version: Set(storage_version),
        activity_count: Set(0),
        lease_until: Set(j.lease_until),
        due_at: Set(j.run.next_attempt_at),
        created_at: Set(j.run.created_at),
        updated_at: Set(j.run.updated_at),
    }
}
async fn stored_header(store: &JournalStore, id: RunId) -> run::Model {
    run::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(run::Column::Id.eq(id.0)))
        .one(&store.db.conn().unwrap())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn runner_queries_observe_uncommitted_rows_preserve_scopes_and_rollback() {
    let store = store().await;
    let now = Utc::now();
    let mut candidate = journal();
    candidate.run.status = RunStatus::Running;
    candidate.run.activities[0].status = ActivityStatus::Running;
    candidate.lease_until = Some(now - chrono::Duration::seconds(1));
    candidate.run.next_attempt_at = None;
    let id = candidate.run.id;
    let intent = Uuid::new_v4();
    let definition =
        crate::domain::registration::Definition::new(execution_definition().contract());
    let name = definition.contract.name.clone();
    let definition_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes());
    let transaction_store = store.clone();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        store.db.db().transaction_ref_mapped(move |txn| {
            Box::pin(async move {
                let scope = AccessScope::allow_all();
                let denied = restricted(pep_properties::OWNER_ID, Uuid::new_v4());
                let denied_definition = restricted(pep_properties::RESOURCE_ID, Uuid::new_v4());
                secure_insert::<run::Entity>(header(&candidate, 0), &scope, txn).await?;
                secure_insert::<outbox::Entity>(
                    outbox::ActiveModel {
                        id: Set(intent),
                        tenant_id: Set(candidate.run.owner.tenant_id),
                        owner_id: Set(candidate.run.owner.subject_id),
                        run_id: Set(id.0),
                        generation: Set(0),
                        due_at: Set(now),
                        forwarded: Set(true),
                        delivered_at: Set(None),
                    },
                    &scope,
                    txn,
                )
                .await?;
                secure_insert::<super::super::definition::Entity>(
                    super::super::definition::ActiveModel {
                        id: Set(definition_id),
                        name: Set(name.clone()),
                        revision: Set(0),
                        state: Set(serde_json::to_vec(&definition).unwrap()),
                    },
                    &scope,
                    txn,
                )
                .await?;
                super::super::events::record(txn, &scope, None, &candidate).await?;
                assert_eq!(
                    transaction_store
                        .get_on(txn, &scope, id)
                        .await?
                        .unwrap()
                        .run
                        .status,
                    RunStatus::Running
                );
                assert!(transaction_store.get_on(txn, &denied, id).await?.is_none());
                let query = all_query(now - chrono::Duration::days(1));
                let listed = transaction_store
                    .list_progress_on(txn, &scope, &query)
                    .await?;
                assert_eq!(listed.total, 1);
                assert_eq!(listed.items[0].id, id);
                assert!(
                    transaction_store
                        .list_progress_on(txn, &denied, &query)
                        .await?
                        .items
                        .is_empty()
                );
                let expired = transaction_store
                    .expired_claims_on(txn, &scope, now)
                    .await?;
                assert_eq!(expired.len(), 1);
                assert_eq!(expired[0].run.id, id);
                assert!(
                    transaction_store
                        .expired_claims_on(txn, &denied, now)
                        .await?
                        .is_empty()
                );
                let events = transaction_store.events_on(txn, &scope, id, 0, 200).await?;
                assert!(events.iter().any(|event| event.kind == EventKind::Started));
                assert!(
                    transaction_store
                        .events_on(txn, &denied, id, 0, 200)
                        .await?
                        .is_empty()
                );
                assert_eq!(
                    transaction_store
                        .definition_on(txn, &scope, &name)
                        .await
                        .unwrap()
                        .contract
                        .name,
                    name
                );
                assert!(matches!(
                    transaction_store
                        .definition_on(txn, &denied_definition, &name)
                        .await,
                    Err(crate::domain::error::DomainError::DefinitionNotFound(_))
                ));
                assert_eq!(
                    transaction_store.definitions_on(txn, &scope).await?.len(),
                    1
                );
                assert!(
                    transaction_store
                        .definitions_on(txn, &denied_definition)
                        .await?
                        .is_empty()
                );
                assert!(matches!(
                    transaction_store
                        .mark_delivered_on(txn, &denied, intent, now)
                        .await,
                    Err(StoreError::Conflict)
                ));
                let untouched = outbox::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(Condition::all().add(outbox::Column::Id.eq(intent)))
                    .one(txn)
                    .await?
                    .unwrap();
                assert!(untouched.delivered_at.is_none());
                transaction_store
                    .mark_delivered_on(txn, &scope, intent, now)
                    .await?;
                let delivered = outbox::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(Condition::all().add(outbox::Column::Id.eq(intent)))
                    .one(txn)
                    .await?
                    .unwrap();
                assert!(delivered.delivered_at.is_some());
                Err::<(), StoreError>(StoreError::Conflict)
            })
        }),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(StoreError::Conflict)));
    let scope = AccessScope::allow_all();
    assert!(store.get(&scope, id).await.unwrap().is_none());
    assert!(
        store
            .list_progress(&scope, &all_query(now - chrono::Duration::days(1)))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(store.expired_claims(&scope, now).await.unwrap().is_empty());
    assert!(store.events(&scope, id, 0, 200).await.unwrap().is_empty());
    assert!(store.definitions().await.unwrap().is_empty());
    assert!(
        outbox::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(outbox::Column::Id.eq(intent)))
            .one(&store.db.conn().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn hydration_rejects_unsupported_storage_version() {
    let store = store().await;
    let j = journal();
    let id = j.run.id;
    store.insert(AccessScope::allow_all(), j).await.unwrap();
    run::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(run::Column::Id.eq(id.0)))
        .col_expr(run::Column::StorageVersion, Expr::value(2))
        .exec(&store.db.conn().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        store.get(&AccessScope::allow_all(), id).await,
        Err(StoreError::Invariant("unsupported storage version"))
    ));
}
#[tokio::test]
async fn hydration_rejects_missing_normalized_activity() {
    let store = store().await;
    let j = journal();
    let id = j.run.id;
    store.insert(AccessScope::allow_all(), j).await.unwrap();
    super::super::activity::Entity::delete_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(super::super::activity::Column::RunId.eq(id.0))
                .add(super::super::activity::Column::ActivityId.eq("one")),
        )
        .exec(&store.db.conn().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        store.get(&AccessScope::allow_all(), id).await,
        Err(StoreError::Conflict)
    ));
}
#[tokio::test]
async fn hydration_rejects_registration_generation_mismatch() {
    let store = store().await;
    let j = journal();
    let id = j.run.id;
    store.insert(AccessScope::allow_all(), j).await.unwrap();
    run::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(run::Column::Id.eq(id.0)))
        .col_expr(run::Column::RegistrationGeneration, Expr::value(1))
        .exec(&store.db.conn().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        store.get(&AccessScope::allow_all(), id).await,
        Err(StoreError::Invariant("registration generation mismatch"))
    ));
}
#[tokio::test]
async fn hydration_rejects_a_header_observed_before_a_committed_transition() {
    let store = store().await;
    let j = journal();
    let id = j.run.id;
    store.insert(AccessScope::allow_all(), j).await.unwrap();
    let stale = stored_header(&store, id).await;
    let mut next = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    next.request_cancel(Utc::now());
    store
        .save(AccessScope::allow_all(), next.revision, next, false)
        .await
        .unwrap();
    assert!(matches!(
        normalized::hydrate(&store.db.conn().unwrap(), &AccessScope::allow_all(), stale).await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(
        store
            .get(&AccessScope::allow_all(), id)
            .await
            .unwrap()
            .unwrap()
            .run
            .status,
        RunStatus::Cancelled
    );
}

#[tokio::test]
async fn progress_listing_filters_orders_pages_and_clamps() {
    let store = store().await;
    let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let owner = journal().run.owner;
    let mut rows = Vec::new();
    for (number, delta, state) in [
        (1u128, -1, RunStatus::Queued),
        (2, 0, RunStatus::Queued),
        (3, 1, RunStatus::Failed),
        (4, 1, RunStatus::Queued),
    ] {
        let mut j = journal();
        j.run.id = RunId(Uuid::from_u128(number));
        j.run.owner = owner.clone();
        j.run.created_at = at + chrono::Duration::seconds(delta);
        j.run.updated_at = j.run.created_at;
        j.run.status = state;
        rows.push(header(&j, 0));
    }
    secure_insert_many::<run::Entity>(rows, &AccessScope::allow_all(), &store.db.conn().unwrap())
        .await
        .unwrap();
    let scope = restricted(pep_properties::OWNER_ID, owner.subject_id);
    let mut query = all_query(at);
    let page = store.list_progress(&scope, &query).await.unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.id.0.as_u128())
            .collect::<Vec<_>>(),
        vec![4, 3, 2]
    );
    assert_eq!(page.items[1].state.status(), PublicRunStatus::Failed);
    query.status = Some(PublicRunStatus::Queued);
    let queued = store.list_progress(&scope, &query).await.unwrap();
    assert_eq!(queued.total, 2);
    assert_eq!(
        queued
            .items
            .iter()
            .map(|item| item.id.0.as_u128())
            .collect::<Vec<_>>(),
        vec![4, 2]
    );
    query.status = None;
    query.limit = 1;
    query.offset = 1;
    let second = store.list_progress(&scope, &query).await.unwrap();
    assert_eq!(second.total, 3);
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].id.0.as_u128(), 3);
    query.limit = 0;
    query.offset = 0;
    let minimum = store.list_progress(&scope, &query).await.unwrap();
    assert_eq!(minimum.items.len(), 1);
    assert_eq!(minimum.items[0].id.0.as_u128(), 4);
    query.offset = 3;
    let past_end = store.list_progress(&scope, &query).await.unwrap();
    assert_eq!(past_end.total, 3);
    assert!(past_end.items.is_empty());
    assert!(
        store
            .list_progress(
                &restricted(pep_properties::OWNER_ID, Uuid::new_v4()),
                &all_query(at)
            )
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let rows = (10u128..115)
        .map(|number| {
            let mut j = journal();
            j.run.id = RunId(Uuid::from_u128(number));
            j.run.owner = owner.clone();
            j.run.created_at = at;
            j.run.updated_at = at;
            header(&j, 0)
        })
        .collect();
    secure_insert_many::<run::Entity>(rows, &AccessScope::allow_all(), &store.db.conn().unwrap())
        .await
        .unwrap();
    query.status = None;
    query.offset = 0;
    query.limit = u32::MAX;
    let capped = store.list_progress(&scope, &query).await.unwrap();
    assert_eq!(capped.total, 108);
    assert_eq!(capped.items.len(), 100);
    query.offset = 100;
    let tail = store.list_progress(&scope, &query).await.unwrap();
    assert_eq!(tail.total, 108);
    assert_eq!(tail.items.len(), 8);
    assert!(
        !tail
            .items
            .iter()
            .any(|item| capped.items.iter().any(|first| first.id == item.id))
    );
}

#[tokio::test]
async fn public_sdk_admission_uses_persistent_registration_without_local_handlers() {
    let store = store().await;
    let tenant = Uuid::new_v4();
    let ctx = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap();
    let registry = Arc::new(crate::domain::registry::Registry::default());
    let registrar = crate::infra::registrar::Registrar {
        store: store.clone(),
        registry: registry.clone(),
    };
    let hub = toolkit::ClientHub::new();
    hub.register::<dyn DurableExecution>(Arc::new(crate::infra::service::Service {
        store: store.clone(),
        enforcer: authz_resolver_sdk::PolicyEnforcer::new(Arc::new(
            crate::gear::tests::TenantPolicy(tenant),
        )),
    }));
    let sdk = DurableExecutionClient::resolve(&hub).unwrap();
    assert!(matches!(
        sdk.raw()
            .start(
                &ctx,
                "missing.contract.v1",
                serde_json::Value::Null,
                StartOptions::default()
            )
            .await,
        Err(toolkit_canonical_errors::CanonicalError::NotFound { .. })
    ));
    let scope = AccessScope::allow_all();
    assert_eq!(
        store
            .list_progress(&scope, &all_query(DateTime::from_timestamp(0, 0).unwrap()))
            .await
            .unwrap()
            .total,
        0
    );
    assert!(
        store
            .deliveries(&scope, Utc::now(), 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(store.definitions().await.unwrap().is_empty());
    let definition = execution_definition();
    registrar
        .register_contract(definition.contract())
        .await
        .unwrap();
    assert!(!registry.available(&definition.name, 0));
    let accepted = sdk
        .raw()
        .start(
            &ctx,
            &definition.name,
            serde_json::json!({"value":42}),
            StartOptions::default(),
        )
        .await
        .unwrap();
    let saved = sdk.get(&ctx, accepted.run_id).await.unwrap();
    assert_eq!(
        saved.state.status(),
        durable_execution_sdk::observation::RunStatus::Queued
    );
    assert_eq!(saved.owner.subject_id, ctx.subject_id());
    assert_eq!(saved.steps.len(), 2);
    assert!(saved.steps.iter().all(|step| step.attempts == 0));
    assert_eq!(
        store
            .deliveries(&scope, Utc::now(), 100)
            .await
            .unwrap()
            .len(),
        1
    );
    let oversized = serde_json::Value::String("x".repeat(262_143));
    assert!(matches!(
        sdk.raw()
            .start(&ctx, &definition.name, oversized, StartOptions::default())
            .await,
        Err(toolkit_canonical_errors::CanonicalError::InvalidArgument { .. })
    ));
    let at_limit = serde_json::Value::String("x".repeat(262_142));
    sdk.raw()
        .start(&ctx, &definition.name, at_limit, StartOptions::default())
        .await
        .unwrap();
    assert_eq!(
        store
            .list_progress(&scope, &all_query(DateTime::from_timestamp(0, 0).unwrap()))
            .await
            .unwrap()
            .total,
        2
    );
}

#[tokio::test]
async fn recovery_reaches_stale_delivery_beyond_first_thousand_older_due_runs() {
    let store = store().await;
    let now = Utc::now();
    let scope = AccessScope::allow_all();
    let mut late = journal();
    late.run.created_at = now - chrono::Duration::hours(2);
    late.run.next_attempt_at = Some(now - chrono::Duration::minutes(1));
    let id = late.run.id;
    store.insert(scope.clone(), late).await.unwrap();
    let intent = Uuid::new_v5(&id.0, &0i64.to_be_bytes());
    store
        .mark_delivered(&scope, intent, now - chrono::Duration::minutes(10))
        .await
        .unwrap();
    let rows = (0..1001)
        .map(|_| {
            let mut old = journal();
            old.run.created_at = now - chrono::Duration::hours(3);
            old.run.next_attempt_at = Some(now - chrono::Duration::hours(1));
            let mut row = header(&old, 0);
            row.journal = Set(b"unrelated headers must not be hydrated".to_vec());
            row
        })
        .collect();
    secure_insert_many::<run::Entity>(rows, &scope, &store.db.conn().unwrap())
        .await
        .unwrap();
    store
        .recover_unclaimed_deliveries(now, now - chrono::Duration::minutes(5))
        .await
        .unwrap();
    let reopened = outbox::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(outbox::Column::Id.eq(intent)))
        .one(&store.db.conn().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(reopened.delivered_at.is_none());
    assert_eq!(
        store
            .deliveries(&scope, now, 100)
            .await
            .unwrap()
            .iter()
            .filter(|delivery| delivery.run_id == id)
            .count(),
        1
    );
    assert_eq!(
        store.get(&scope, id).await.unwrap().unwrap().run.activities[0].attempts,
        0
    );
}

#[cfg(feature = "integration")]
#[path = "../integration/audit_start_race_tests.rs"]
mod integration;

#[tokio::test]
async fn intent_ack_requires_visible_row_and_duplicate_ack_preserves_timestamp() {
    let store = store().await;
    let j = journal();
    let intent = Uuid::new_v5(&j.run.id.0, &j.delivery_generation.to_be_bytes());
    let denied = restricted(pep_properties::OWNER_TENANT_ID, Uuid::new_v4());
    let allowed = AccessScope::allow_all();
    store.insert(allowed.clone(), j).await.unwrap();
    let first = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    assert!(matches!(
        store.mark_delivered(&denied, intent, first).await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(
        store
            .deliveries(&allowed, Utc::now(), 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        store.mark_delivered(&allowed, Uuid::new_v4(), first).await,
        Err(StoreError::Conflict)
    ));
    store.mark_delivered(&allowed, intent, first).await.unwrap();
    store
        .mark_delivered(&allowed, intent, first + chrono::Duration::seconds(1))
        .await
        .unwrap();
    let row = outbox::Entity::find()
        .secure()
        .scope_with(&allowed)
        .filter(Condition::all().add(outbox::Column::Id.eq(intent)))
        .one(&store.db.conn().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.delivered_at, Some(first));
    assert!(matches!(
        store.mark_delivered(&denied, intent, first).await,
        Err(StoreError::Conflict)
    ));
}

#[tokio::test]
async fn progress_filters_legacy_partial_failure_before_pagination() {
    use durable_execution_sdk::observation::{
        RunStatus as PublicStatus, WorkflowQuery as PublicQuery,
    };
    let store = store().await;
    let mut failing = journal();
    failing.run.status = RunStatus::Running;
    failing.run.activities[0].status = ActivityStatus::Failed;
    failing.run.activities[0].error_code = Some("expected_failure".into());
    failing.run.activities[1].status = ActivityStatus::Running;
    let id = failing.run.id;
    let mut healthy = journal();
    healthy.run.owner = failing.run.owner.clone();
    healthy.run.status = RunStatus::Running;
    healthy.run.activities[0].status = ActivityStatus::Running;
    secure_insert_many::<run::Entity>(
        vec![header(&failing, 0), header(&healthy, 0)],
        &AccessScope::allow_all(),
        &store.db.conn().unwrap(),
    )
    .await
    .unwrap();
    let mut query = PublicQuery {
        since: DateTime::from_timestamp(0, 0).unwrap(),
        status: Some(PublicStatus::Failing),
        limit: 1,
        offset: 0,
    };
    let page = store
        .list_progress(&AccessScope::allow_all(), &query)
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, id);
    assert_eq!(page.items[0].state.status(), PublicStatus::Failing);
    query.status = Some(PublicStatus::Running);
    let page = store
        .list_progress(&AccessScope::allow_all(), &query)
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, healthy.run.id);
    let hidden = store
        .list_progress(
            &restricted(pep_properties::OWNER_ID, Uuid::new_v4()),
            &query,
        )
        .await
        .unwrap();
    assert_eq!(hidden.total, 0);
}
