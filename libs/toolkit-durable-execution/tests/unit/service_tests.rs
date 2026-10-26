use super::*;
use crate::observation::{RunState, WorkflowQuery};
use crate::state::persisted::RunStatus;
use crate::{
    AuthorizationError, Authorizer, CancelOptions, ErrorKind, EventKind, ExecutionClient,
    ServiceAction,
};
use std::sync::Arc;
use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};

use std::sync::atomic::{AtomicBool, Ordering};

struct TenantAuthZ {
    tenant: Uuid,
    operator: Uuid,
    allow_operator: AtomicBool,
    deny_reads: AtomicBool,
    unavailable: AtomicBool,
    resource: parking_lot::Mutex<Option<Uuid>>,
}

#[async_trait]
impl Authorizer for TenantAuthZ {
    async fn caller_scope(
        &self,
        ctx: &SecurityContext,
        action: CallerAction,
    ) -> Result<AccessScope, AuthorizationError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(AuthorizationError::unavailable());
        }
        let operator_action = matches!(
            action,
            CallerAction::Manage | CallerAction::Inspect | CallerAction::List
        );
        let allowed = !(action == CallerAction::Get && self.deny_reads.load(Ordering::SeqCst))
            && (!operator_action
                || (ctx.subject_id() == self.operator
                    && self.allow_operator.load(Ordering::SeqCst)));
        if !allowed {
            return Err(AuthorizationError::denied());
        }
        let mut filters = vec![ScopeFilter::in_uuids(
            pep_properties::OWNER_TENANT_ID,
            vec![self.tenant],
        )];
        if operator_action && let Some(id) = *self.resource.lock() {
            filters.push(ScopeFilter::in_uuids(pep_properties::RESOURCE_ID, vec![id]));
        }
        Ok(AccessScope::single(ScopeConstraint::new(filters)))
    }
    async fn service_scope(&self, _: ServiceAction) -> Result<AccessScope, AuthorizationError> {
        Ok(AccessScope::allow_all())
    }
}

fn caller(subject: Uuid, tenant: Uuid, kind: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .subject_type(kind)
        .build()
        .unwrap()
}

fn service(
    store: crate::store::JournalStore,
    tenant: Uuid,
    operator: Uuid,
) -> (Service, Arc<TenantAuthZ>) {
    let policy = Arc::new(TenantAuthZ {
        tenant,
        operator,
        allow_operator: AtomicBool::new(true),
        deny_reads: AtomicBool::new(false),
        unavailable: AtomicBool::new(false),
        resource: parking_lot::Mutex::new(None),
    });
    (
        Service {
            store,
            authority: Authority::new(policy.clone(), std::time::Duration::from_secs(5)),
        },
        policy,
    )
}

async fn owned_run(tenant: Uuid, owner: Uuid) -> (crate::store::JournalStore, RunId) {
    let store = crate::store::repository::tests::store().await;
    let mut journal = crate::store::repository::tests::journal();
    journal.run.owner = ExecutionOwner {
        tenant_id: tenant,
        subject_id: owner,
    };
    let id = journal.run.id;
    store
        .insert(AccessScope::allow_all(), journal)
        .await
        .unwrap();
    (store, id)
}

#[tokio::test]
async fn pdp_authorized_service_stops_another_subjects_run_inside_the_tenant() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (store, id) = owned_run(tenant, owner).await;
    let operator = Uuid::new_v4();
    let (svc, _) = service(store.clone(), tenant, operator);
    let manager = caller(operator, tenant, "service");
    assert!(
        request_cancel(
            &svc,
            &manager,
            id,
            0,
            Some("Stopped by authorized operator".into())
        )
        .await
        .unwrap()
    );
    let saved = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.run.status, RunStatus::Cancelled);
    assert_eq!(
        saved.run.stop_reason.as_deref(),
        Some("Stopped by authorized operator")
    );
    assert!(
        !request_cancel(&svc, &manager, id, 0, Some("other".into()))
            .await
            .unwrap()
    );
    let again = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        again.run.stop_reason.as_deref(),
        Some("Stopped by authorized operator")
    );
}

#[tokio::test]
async fn a_different_user_cannot_stop_the_run_and_another_tenant_stays_hidden() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (store, id) = owned_run(tenant, owner).await;
    let mut foreign = crate::store::repository::tests::journal();
    foreign.run.owner = ExecutionOwner {
        tenant_id: Uuid::new_v4(),
        subject_id: owner,
    };
    let foreign_id = foreign.run.id;
    store
        .insert(AccessScope::allow_all(), foreign)
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let (svc, _) = service(store.clone(), tenant, operator);
    let stranger = caller(Uuid::new_v4(), tenant, "user");
    assert!(matches!(
        request_cancel(&svc, &stranger, id, 0, None)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::PermissionDenied
    ));
    let manager = caller(operator, tenant, "user");
    assert!(matches!(
        request_cancel(&svc, &manager, foreign_id, 0, None)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    ));
    assert!(matches!(
        request_cancel(
            &svc,
            &manager,
            id,
            4,
            Some("Stopped by authorized operator".into())
        )
        .await
        .unwrap_err()
        .kind(),
        ErrorKind::Aborted
    ));
    let untouched = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched.run.status, RunStatus::Queued);
    assert_eq!(untouched.run.stop_reason, None);
    assert!(
        request_cancel(
            &svc,
            &caller(owner, tenant, "user"),
            id,
            0,
            Some("cancelled from the app".into()),
        )
        .await
        .unwrap()
    );
}

fn query() -> WorkflowQuery {
    WorkflowQuery::new(Utc::now() - chrono::Duration::days(1))
}

#[tokio::test]
async fn subject_type_does_not_grant_management_without_pdp_permission() {
    let tenant = Uuid::new_v4();
    let (store, id) = owned_run(tenant, Uuid::new_v4()).await;
    let (svc, _) = service(store.clone(), tenant, Uuid::new_v4());
    for kind in ["admin", "user", "service"] {
        let stranger = caller(Uuid::new_v4(), tenant, kind);
        assert!(matches!(
            svc.inspect(&stranger, id).await.unwrap_err().kind(),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            svc.list(&stranger, query()).await.unwrap_err().kind(),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            svc.cancel(&stranger, id, CancelOptions::new(0))
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        ));
    }
    let saved = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.run.status, RunStatus::Queued);
    assert_eq!(saved.revision, 0);
}

#[tokio::test]
async fn pdp_resource_constraints_limit_inspection_listing_and_cancellation() {
    let tenant = Uuid::new_v4();
    let (store, id) = owned_run(tenant, Uuid::new_v4()).await;
    let mut other = crate::store::repository::tests::journal();
    other.run.owner.tenant_id = tenant;
    let other_id = other.run.id;
    store.insert(AccessScope::allow_all(), other).await.unwrap();
    let operator = Uuid::new_v4();
    let (svc, policy) = service(store.clone(), tenant, operator);
    *policy.resource.lock() = Some(id.0);
    let manager = caller(operator, tenant, "user");
    assert_eq!(svc.inspect(&manager, id).await.unwrap().id, id);
    let scope = svc
        .authorize_scope(&manager, CallerAction::Manage)
        .await
        .unwrap();
    let definition = crate::store::repository::tests::definition()
        .contract()
        .clone();
    let mut journal = store.get(&scope, id).await.unwrap().unwrap();
    let claim = journal
        .claim(
            &definition,
            journal.delivery_generation,
            Utc::now(),
            std::time::Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    store
        .save(scope.clone(), journal.revision, journal, false)
        .await
        .unwrap();
    let mut journal = store.get(&scope, id).await.unwrap().unwrap();
    journal
        .complete(
            claim,
            serde_json::json!({"checkpoint": "saved"}),
            Utc::now(),
        )
        .unwrap();
    store
        .save(scope.clone(), journal.revision, journal, true)
        .await
        .unwrap();
    assert!(!store.events(&scope, id, 0, 100).await.unwrap().is_empty());
    svc.cancel(&manager, id, CancelOptions::new(0))
        .await
        .unwrap();
    let mut journal = store.get(&scope, id).await.unwrap().unwrap();
    assert!(
        journal
            .continue_execution(0, RunStatus::Cancelled, &definition, Utc::now())
            .unwrap()
    );
    store
        .save(scope.clone(), journal.revision, journal, true)
        .await
        .unwrap();
    let journal = store.get(&scope, id).await.unwrap().unwrap();
    assert_eq!(journal.run.previous_executions.len(), 1);
    assert_eq!(journal.run.activities[0].attempt_history.len(), 1);
    assert_eq!(
        journal.run.activities[0].result,
        Some(serde_json::json!({"checkpoint": "saved"}))
    );
    let page = svc.list(&manager, query()).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, id);
    assert!(matches!(
        svc.inspect(&manager, other_id).await.unwrap_err().kind(),
        ErrorKind::NotFound
    ));
    assert!(matches!(
        svc.cancel(&manager, other_id, CancelOptions::new(0))
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    ));
    svc.cancel(&manager, id, CancelOptions::new(1))
        .await
        .unwrap();
    assert_eq!(
        store
            .get(&AccessScope::allow_all(), other_id)
            .await
            .unwrap()
            .unwrap()
            .run
            .status,
        RunStatus::Queued
    );
}

#[tokio::test]
async fn management_reauthorizes_after_revocation_and_policy_outage() {
    let tenant = Uuid::new_v4();
    let (store, id) = owned_run(tenant, Uuid::new_v4()).await;
    let operator = Uuid::new_v4();
    let (svc, policy) = service(store.clone(), tenant, operator);
    let manager = caller(operator, tenant, "service");
    svc.inspect(&manager, id).await.unwrap();
    policy.allow_operator.store(false, Ordering::SeqCst);
    assert!(matches!(
        svc.cancel(&manager, id, CancelOptions::new(0))
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::PermissionDenied
    ));
    assert!(matches!(
        svc.list(&manager, query()).await.unwrap_err().kind(),
        ErrorKind::PermissionDenied
    ));
    policy.allow_operator.store(true, Ordering::SeqCst);
    policy.unavailable.store(true, Ordering::SeqCst);
    assert!(matches!(
        svc.inspect(&manager, id).await.unwrap_err().kind(),
        ErrorKind::Unavailable
    ));
    assert!(matches!(
        svc.cancel(&manager, id, CancelOptions::new(0))
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Unavailable
    ));
    let saved = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.run.status, RunStatus::Queued);
    assert_eq!(saved.revision, 0);
    policy.unavailable.store(false, Ordering::SeqCst);
    svc.cancel(&manager, id, CancelOptions::new(0))
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
        RunStatus::Cancelled
    );
}

#[tokio::test]
async fn invalid_request_filters_fail_before_policy_or_database_access() {
    let (svc, policy) = service(
        crate::store::repository::tests::store().await,
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    policy.unavailable.store(true, Ordering::SeqCst);
    let ctx = caller(Uuid::new_v4(), Uuid::new_v4(), "user");
    for name in ["x".repeat(161), "sync.v0".into(), "Sync.v1".into()] {
        assert!(matches!(
            svc.start(&ctx, &name, Value::Null, StartOptions::default())
                .await
                .map_err(|e| e.kind()),
            Err(ErrorKind::InvalidArgument)
        ));
    }
    for status in ["x".repeat(10000), "unknown".into()] {
        let mut request = serde_json::to_value(query()).unwrap();
        request["status"] = Value::String(status);
        assert!(serde_json::from_value::<WorkflowQuery>(request).is_err());
    }
}

fn sdk(svc: Service) -> ExecutionClient {
    ExecutionClient::new(Arc::new(svc))
}

#[tokio::test]
async fn sdk_snapshot_cursor_bounds_saved_events_and_paged_replay() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (store, id) = owned_run(tenant, owner).await;
    let (svc, _) = service(store, tenant, Uuid::new_v4());
    let sdk = sdk(svc);
    let ctx = caller(owner, tenant, "user");
    let before = sdk.get(&ctx, id).await.unwrap();
    assert_eq!(before.id, id);
    assert_eq!(before.owner.subject_id, owner);
    assert_eq!(before.state, RunState::Queued);
    let initial = sdk.events(&ctx, id, 0, 100).await.unwrap();
    assert!(!initial.is_empty());
    assert!(initial.iter().all(|event| event.sequence <= before.cursor));
    assert!(
        sdk.events(&ctx, id, before.cursor, 100)
            .await
            .unwrap()
            .is_empty()
    );

    assert!(
        request_cancel(sdk.raw(), &ctx, id, 0, Some("Stopped by owner".into()))
            .await
            .unwrap()
    );
    let after = sdk.get(&ctx, id).await.unwrap();
    assert_eq!(after.id, id);
    assert_eq!(
        after.state.status(),
        crate::observation::RunStatus::Cancelled
    );
    assert!(
        matches!(&after.state, RunState::Cancelled { reason: Some(reason), .. } if reason == "Stopped by owner")
    );
    assert!(after.cursor > before.cursor);
    let replay = sdk.events(&ctx, id, before.cursor, 100).await.unwrap();
    assert!(
        replay
            .iter()
            .any(|event| event.kind == EventKind::StopRequested)
    );
    assert!(replay.iter().any(|event| event.kind == EventKind::Stopped));
    assert!(replay.iter().all(|event| {
        event.run_id == id && event.sequence > before.cursor && event.sequence <= after.cursor
    }));
    assert!(
        replay
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    // A zero limit is clamped to one; the next page must neither skip nor repeat it.
    let first = sdk.events(&ctx, id, before.cursor, 0).await.unwrap();
    assert_eq!(first.len(), 1);
    let mut paged = sdk.events(&ctx, id, first[0].sequence, 100).await.unwrap();
    paged.insert(0, first[0].clone());
    assert_eq!(
        serde_json::to_value(paged).unwrap(),
        serde_json::to_value(replay).unwrap()
    );
    assert!(
        sdk.events(&ctx, id, after.cursor, 100)
            .await
            .unwrap()
            .is_empty()
    );
    let all = sdk.events(&ctx, id, 0, 100).await.unwrap();
    assert_eq!(
        all.iter()
            .filter(|event| event.sequence <= before.cursor)
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        initial
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn sdk_snapshot_and_events_hide_unknown_foreign_owner_and_foreign_tenant_runs() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (store, journal) = owned_run(tenant, owner).await;
    let mut hidden = vec![RunId(Uuid::new_v4())];
    for run_owner in [
        ExecutionOwner {
            tenant_id: tenant,
            subject_id: Uuid::new_v4(),
        },
        ExecutionOwner {
            tenant_id: Uuid::new_v4(),
            subject_id: owner,
        },
    ] {
        let mut run = crate::store::repository::tests::journal();
        run.run.owner = run_owner;
        hidden.push(run.run.id);
        store.insert(AccessScope::allow_all(), run).await.unwrap();
    }
    let (svc, _) = service(store, tenant, Uuid::new_v4());
    let sdk = sdk(svc);
    let ctx = caller(owner, tenant, "user");
    assert_eq!(sdk.get(&ctx, journal).await.unwrap().id, journal);
    assert!(!sdk.events(&ctx, journal, 0, 100).await.unwrap().is_empty());
    for id in hidden {
        for error in [
            sdk.get(&ctx, id).await.unwrap_err(),
            sdk.events(&ctx, id, 0, 100).await.unwrap_err(),
        ] {
            assert_eq!(error.kind(), ErrorKind::NotFound);
            assert_eq!(error.run_id(), Some(id));
        }
    }
}

#[tokio::test]
async fn sdk_snapshot_and_events_refresh_pdp_and_reject_out_of_range_cursors() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (store, id) = owned_run(tenant, owner).await;
    let (svc, policy) = service(store, tenant, Uuid::new_v4());
    let sdk = sdk(svc);
    let ctx = caller(owner, tenant, "user");
    let before = sdk.get(&ctx, id).await.unwrap();
    assert!(!sdk.events(&ctx, id, 0, 100).await.unwrap().is_empty());
    policy.deny_reads.store(true, Ordering::SeqCst);
    assert!(matches!(
        sdk.get(&ctx, id).await.map_err(|e| e.kind()),
        Err(ErrorKind::PermissionDenied)
    ));
    assert!(matches!(
        sdk.events(&ctx, id, 0, 100).await.map_err(|e| e.kind()),
        Err(ErrorKind::PermissionDenied)
    ));
    policy.deny_reads.store(false, Ordering::SeqCst);
    policy.unavailable.store(true, Ordering::SeqCst);
    assert!(matches!(
        sdk.get(&ctx, id).await.map_err(|e| e.kind()),
        Err(ErrorKind::Unavailable)
    ));
    assert!(matches!(
        sdk.events(&ctx, id, 0, 100).await.map_err(|e| e.kind()),
        Err(ErrorKind::Unavailable)
    ));
    policy.unavailable.store(false, Ordering::SeqCst);
    let restored = sdk.get(&ctx, id).await.unwrap();
    assert_eq!(restored.cursor, before.cursor);
    assert_eq!(restored.state, RunState::Queued);
    assert!(!sdk.events(&ctx, id, 0, 100).await.unwrap().is_empty());
    assert!(
        sdk.events(&ctx, id, u64::try_from(i64::MAX).unwrap(), 100)
            .await
            .unwrap()
            .is_empty()
    );
    let error = sdk.events(&ctx, id, u64::MAX, 100).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    assert_eq!(error.field().as_deref(), Some("after"));
    assert_eq!(error.code(), reason::OUT_OF_RANGE);
}

async fn request_cancel(
    client: &dyn RunService,
    ctx: &SecurityContext,
    id: RunId,
    expected_epoch: u64,
    reason: Option<String>,
) -> Result<bool, Error> {
    client
        .cancel(ctx, id, {
            let mut options = CancelOptions::new(expected_epoch);
            options.reason = reason;
            options
        })
        .await
        .map(|r| r == crate::CancelResult::Requested)
}

#[tokio::test]
async fn result_and_history_require_fresh_owner_access_independent_of_inspection() {
    use crate::contracts::InputSource;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let operator = Uuid::new_v4();
    let (store, id) = owned_run(tenant, owner).await;
    let contract = crate::store::repository::tests::definition()
        .contract()
        .clone();
    let (svc, policy) = service(store.clone(), tenant, operator);
    let user = caller(owner, tenant, "user");
    assert!(matches!(
        svc.result(
            &user,
            id,
            &contract.name,
            &contract.fingerprint().unwrap(),
            crate::contracts::ResultSelector::Workflow
        )
        .await
        .unwrap_err()
        .kind(),
        ErrorKind::FailedPrecondition
    ));
    let mut journal = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    let now = Utc::now();
    for value in [
        serde_json::json!(17),
        serde_json::json!({"private_result":42}),
    ] {
        let claim = journal
            .claim(
                &contract,
                journal.delivery_generation,
                now,
                std::time::Duration::from_secs(120),
            )
            .unwrap()
            .unwrap();
        journal.complete(claim, value, now).unwrap();
    }
    store
        .save(AccessScope::allow_all(), journal.revision, journal, false)
        .await
        .unwrap();
    assert_eq!(
        svc.result(
            &user,
            id,
            &contract.name,
            &contract.fingerprint().unwrap(),
            crate::contracts::ResultSelector::Checkpoint(InputSource::Checkpoint("one".into()))
        )
        .await
        .unwrap(),
        serde_json::json!(17)
    );
    assert_eq!(
        svc.result(
            &user,
            id,
            &contract.name,
            &contract.fingerprint().unwrap(),
            crate::contracts::ResultSelector::Workflow
        )
        .await
        .unwrap(),
        serde_json::json!({"private_result":42})
    );
    assert!(
        !serde_json::to_string(&svc.history(&user, id).await.unwrap())
            .unwrap()
            .contains("private_result")
    );
    // Compatible JSON must not authorize an unrelated contract or checkpoint.
    assert!(matches!(
        svc.result(
            &user,
            id,
            &contract.name,
            "unrelated",
            crate::contracts::ResultSelector::Workflow
        )
        .await
        .unwrap_err()
        .kind(),
        ErrorKind::FailedPrecondition
    ));
    assert!(matches!(
        svc.result(
            &user,
            id,
            &contract.name,
            &contract.fingerprint().unwrap(),
            crate::contracts::ResultSelector::Checkpoint(InputSource::Checkpoint(
                "foreign_step".into()
            ))
        )
        .await
        .unwrap_err()
        .kind(),
        ErrorKind::FailedPrecondition
    ));
    let manager = caller(operator, tenant, "service");
    assert_eq!(svc.inspect(&manager, id).await.unwrap().id, id);
    assert!(matches!(
        svc.result(
            &manager,
            id,
            &contract.name,
            &contract.fingerprint().unwrap(),
            crate::contracts::ResultSelector::Workflow
        )
        .await
        .unwrap_err()
        .kind(),
        ErrorKind::NotFound
    ));
    assert!(matches!(
        svc.history(&manager, id).await.unwrap_err().kind(),
        ErrorKind::NotFound
    ));
    let foreign = caller(owner, Uuid::new_v4(), "user");
    assert!(matches!(
        svc.result(
            &foreign,
            id,
            &contract.name,
            &contract.fingerprint().unwrap(),
            crate::contracts::ResultSelector::Workflow
        )
        .await
        .unwrap_err()
        .kind(),
        ErrorKind::NotFound
    ));
    policy.deny_reads.store(true, Ordering::SeqCst);
    assert!(matches!(
        svc.result(
            &user,
            id,
            &contract.name,
            &contract.fingerprint().unwrap(),
            crate::contracts::ResultSelector::Workflow
        )
        .await
        .unwrap_err()
        .kind(),
        ErrorKind::PermissionDenied
    ));
    assert!(matches!(
        svc.history(&user, id).await.unwrap_err().kind(),
        ErrorKind::PermissionDenied
    ));
    policy.unavailable.store(true, Ordering::SeqCst);
    assert!(matches!(
        svc.result(
            &user,
            id,
            &contract.name,
            &contract.fingerprint().unwrap(),
            crate::contracts::ResultSelector::Workflow
        )
        .await
        .unwrap_err()
        .kind(),
        ErrorKind::Unavailable
    ));
    policy.unavailable.store(false, Ordering::SeqCst);
    policy.deny_reads.store(false, Ordering::SeqCst);
    assert_eq!(
        svc.result(
            &user,
            id,
            &contract.name,
            &contract.fingerprint().unwrap(),
            crate::contracts::ResultSelector::Workflow
        )
        .await
        .unwrap(),
        serde_json::json!({"private_result":42})
    );
}

#[tokio::test]
async fn foreign_contract_is_rejected_before_corrupt_payload_hydration() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (store, id) = owned_run(tenant, owner).await;
    let (service, _) = service(store.clone(), tenant, Uuid::new_v4());
    crate::store::repository::tests::corrupt_header(&store, id, b"unreadable payload".to_vec())
        .await;
    let result = service
        .result(
            &caller(owner, tenant, "user"),
            id,
            "test.store.v1",
            "unrelated-contract",
            crate::contracts::ResultSelector::Workflow,
        )
        .await;
    assert!(matches!(
        result.map_err(|e| e.kind()),
        Err(ErrorKind::FailedPrecondition)
    ));
}

#[tokio::test]
async fn abort_raw_contract_is_definitive_and_cannot_resume_forward_work() {
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (store, id) = owned_run(tenant, owner).await;
    let (service, _) = service(store, tenant, Uuid::new_v4());
    let user = caller(owner, tenant, "user");
    assert_eq!(
        service
            .abort(&user, id, crate::AbortOptions::new(0).reason("abandoned"))
            .await
            .unwrap(),
        crate::CancelResult::Requested
    );
    assert!(matches!(
        service.get(&user, id).await.unwrap().state,
        RunState::Compensated {
            cause: crate::observation::CompensationCause::Abort { .. },
            ..
        }
    ));
    assert!(matches!(
        service
            .resume(&user, id, crate::ContinueOptions::new(0))
            .await
            .map_err(|e| e.kind()),
        Err(ErrorKind::FailedPrecondition)
    ));
}
