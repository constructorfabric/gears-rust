//! Exercise the async SDK against real storage and executor transitions.
use super::*;
use crate::client::RunService;
use crate::contracts::ExecutionDefinition;
use crate::registration::{Registration, RegistrationState, UnregisterMode, UnregisterOptions};
#[cfg(feature = "integration")]
use crate::runtime::{ActivityInput, ErasedActivity};
use crate::state::persisted::*;
use crate::*;
use crate::{
    config::Config,
    engine::executor::Executor,
    state::journal::Journal,
    store::repository::tests::{definition, journal, store},
};
use chrono::Utc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use toolkit_security::AccessScope;

fn registrar(store: JournalStore) -> Registrar {
    Registrar {
        store,
        registry: Arc::new(Registry::default()),
    }
}
fn candidate(definition: &ExecutionDefinition) -> Journal {
    let seed = journal();
    Journal::new(
        seed.run.id,
        seed.run.owner,
        definition.contract(),
        serde_json::Value::Null,
        Utc::now(),
    )
    .unwrap()
}
async fn current(store: &JournalStore, id: RunId) -> Journal {
    store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap()
}
fn executor(registrar: &Registrar) -> Executor {
    Executor {
        store: registrar.store.clone(),
        registry: registrar.registry.clone(),
        config: Config::default(),
    }
}
async fn stop(registrar: &Registrar, mode: UnregisterMode) -> Registration {
    let before = registrar.registration("test.store.v1").await.unwrap();
    registrar
        .unregister(&before.name, UnregisterOptions::new(mode, before.revision))
        .await
        .unwrap()
}
#[tokio::test]
async fn retain_preserves_checkpoints_retry_resume_and_explicit_reactivation() {
    let registrar = registrar(store().await);
    let d = definition();
    let first = registrar.register(d.clone()).await.unwrap();
    assert_eq!(first.state, RegistrationState::Active);
    assert!(matches!(
        registrar.register(d.clone()).await.map_err(|e| e.kind()),
        Err(crate::ErrorKind::AlreadyExists)
    ));
    let changed = crate::test_support::edit(d.clone(), |c| {
        c.activities[0].timeout += Duration::from_secs(1);
    });
    assert!(matches!(
        registrar
            .register_contract(changed.contract().clone())
            .await
            .map_err(|e| e.kind()),
        Err(crate::ErrorKind::AlreadyExists)
    ));
    let j = candidate(&d);
    let id = j.run.id;
    let options = StartOptions::new().idempotency_key("retain-request");
    registrar
        .store
        .start(AccessScope::allow_all(), j.clone(), options.clone())
        .await
        .unwrap();
    let retired = stop(&registrar, UnregisterMode::Retain).await;
    assert_eq!(retired.state, RegistrationState::Retired);
    assert!(registrar.registry.available(d.name(), 0));
    assert!(matches!(
        registrar
            .store
            .start(
                AccessScope::allow_all(),
                candidate(&d),
                StartOptions::default()
            )
            .await,
        Err(DomainError::DefinitionInactive)
    ));
    assert_eq!(
        registrar
            .store
            .start(AccessScope::allow_all(), j, options)
            .await
            .unwrap()
            .run_id,
        id
    );
    let exec = executor(&registrar);
    exec.execute(id, 0, CancellationToken::new()).await.unwrap();
    let mut j = current(&registrar.store, id).await;
    assert_eq!(j.run.activities[0].status, ActivityStatus::Succeeded);
    // Fail the next activity, then retry while Retired without repeating checkpoint one.
    let claim = j
        .claim(
            d.contract(),
            j.delivery_generation,
            Utc::now(),
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    registrar
        .store
        .save(AccessScope::allow_all(), j.revision, j, false)
        .await
        .unwrap();
    let mut j = current(&registrar.store, id).await;
    j.fail(claim, &ActivityError::permanent("fixture"), Utc::now(), 0)
        .unwrap();
    registrar
        .store
        .save(AccessScope::allow_all(), j.revision, j, false)
        .await
        .unwrap();
    let journal = current(&registrar.store, id).await;
    let user = toolkit_security::SecurityContext::builder()
        .subject_id(journal.run.owner.subject_id)
        .subject_tenant_id(journal.run.owner.tenant_id)
        .subject_type("user")
        .build()
        .unwrap();
    let service = crate::client::service::Service {
        store: registrar.store.clone(),
        authority: crate::test_support::tenant_authority(journal.run.owner.tenant_id),
    };
    service
        .retry(&user, id, ContinueOptions::new(0))
        .await
        .unwrap();
    service
        .cancel(&user, id, crate::CancelOptions::new(1))
        .await
        .unwrap();
    service
        .resume(&user, id, ContinueOptions::new(1))
        .await
        .unwrap();
    let j = current(&registrar.store, id).await;
    exec.execute(id, j.delivery_generation, CancellationToken::new())
        .await
        .unwrap();
    let j = current(&registrar.store, id).await;
    assert_eq!(j.run.status, RunStatus::Succeeded);
    assert_eq!(j.run.activities[0].attempts, 1);
    assert_eq!(j.run.execution_epoch, 2);
    assert!(matches!(
        registrar
            .activate(d.name(), first.revision)
            .await
            .map_err(|e| e.kind()),
        Err(crate::ErrorKind::Aborted)
    ));
    let active = registrar
        .activate(d.name(), retired.revision)
        .await
        .unwrap();
    assert_eq!(active.generation, 0);
    assert_eq!(active.state, RegistrationState::Active);
}
#[tokio::test]
async fn missing_bindings_leave_due_attempts_and_checkpoints_unchanged_until_late_registration() {
    let registrar = registrar(store().await);
    let d = definition();
    registrar
        .register_contract(d.contract().clone())
        .await
        .unwrap();
    let j = candidate(&d);
    let id = j.run.id;
    registrar
        .store
        .start(AccessScope::allow_all(), j, StartOptions::default())
        .await
        .unwrap();
    let exec = executor(&registrar);
    let before = current(&registrar.store, id).await;
    exec.execute(id, 0, CancellationToken::new()).await.unwrap();
    let after = current(&registrar.store, id).await;
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    registrar.register(d).await.unwrap();
    exec.execute(id, 0, CancellationToken::new()).await.unwrap();
    let j = current(&registrar.store, id).await;
    assert_eq!(j.run.activities[0].status, ActivityStatus::Succeeded);
    exec.execute(id, j.delivery_generation, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        current(&registrar.store, id).await.run.status,
        RunStatus::Succeeded
    );
}
#[tokio::test]
async fn release_cancels_queued_runs_and_rebinding_does_not_reactivate_or_resume_old_generations() {
    let registrar = registrar(store().await);
    let d = definition();
    registrar.register(d.clone()).await.unwrap();
    let j = candidate(&d);
    let id = j.run.id;
    let options = StartOptions::new().coalescing_key("slot");
    registrar
        .store
        .start(AccessScope::allow_all(), j.clone(), options.clone())
        .await
        .unwrap();
    let stopped = stop(&registrar, UnregisterMode::CancelAndRelease).await;
    assert_eq!(stopped.state, RegistrationState::Stopping);
    assert!(matches!(
        registrar
            .activate(d.name(), stopped.revision)
            .await
            .map_err(|e| e.kind()),
        Err(crate::ErrorKind::FailedPrecondition)
    ));
    assert!(matches!(
        registrar.register(d.clone()).await.map_err(|e| e.kind()),
        Err(crate::ErrorKind::FailedPrecondition)
    ));
    assert!(registrar.registry.available(d.name(), 0));
    let exec = executor(&registrar);
    exec.reconcile_definitions().await.unwrap();
    let released = registrar.registration(d.name()).await.unwrap();
    assert_eq!(released.state, RegistrationState::Released);
    assert!(!registrar.registry.available(d.name(), 0));
    let mut old = current(&registrar.store, id).await;
    assert_eq!(old.run.status, RunStatus::Cancelled);
    old.continue_execution(0, RunStatus::Cancelled, d.contract(), Utc::now())
        .unwrap();
    assert!(matches!(
        registrar
            .store
            .save(AccessScope::allow_all(), old.revision, old, false)
            .await,
        Err(crate::store::StoreError::DefinitionInactive)
    ));
    let prepared = registrar.register(d.clone()).await.unwrap();
    assert_eq!(prepared.state, RegistrationState::Released);
    assert!(registrar.registry.available(d.name(), 1));
    assert!(matches!(
        registrar
            .store
            .start(AccessScope::allow_all(), candidate(&d), options.clone())
            .await,
        Err(DomainError::DefinitionInactive)
    ));
    let active = registrar
        .activate(d.name(), prepared.revision)
        .await
        .unwrap();
    assert_eq!(active.generation, 1);
    let mut next = j;
    next.run.id = RunId(uuid::Uuid::new_v4());
    let new = registrar
        .store
        .start(AccessScope::allow_all(), next, options)
        .await
        .unwrap();
    assert_ne!(new.run_id, id);
    assert_eq!(
        current(&registrar.store, new.run_id)
            .await
            .registration_generation,
        1
    );
    exec.execute(new.run_id, 0, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        current(&registrar.store, new.run_id).await.run.activities[0].attempts,
        1
    );
}
#[tokio::test]
async fn revocation_fences_late_checkpoint_and_claim_but_allows_cancellation_cleanup() {
    let registrar = registrar(store().await);
    let d = definition();
    registrar.register(d.clone()).await.unwrap();
    let mut j = candidate(&d);
    let id = j.run.id;
    registrar
        .store
        .start(AccessScope::allow_all(), j.clone(), StartOptions::default())
        .await
        .unwrap();
    let claim = j
        .claim(d.contract(), 0, Utc::now(), Duration::from_secs(120))
        .unwrap()
        .unwrap();
    registrar
        .store
        .save(AccessScope::allow_all(), 0, j, false)
        .await
        .unwrap();
    let mut late = current(&registrar.store, id).await;
    late.complete(claim, serde_json::json!("must-not-commit"), Utc::now())
        .unwrap();
    stop(&registrar, UnregisterMode::CancelAndRelease).await;
    assert!(matches!(
        registrar
            .store
            .save(AccessScope::allow_all(), late.revision, late, true)
            .await,
        Err(crate::store::StoreError::DefinitionInactive)
    ));
    let mut heartbeat = current(&registrar.store, id).await;
    heartbeat
        .heartbeat(claim, Utc::now(), Duration::from_secs(120))
        .unwrap();
    assert!(
        registrar
            .store
            .save(
                AccessScope::allow_all(),
                heartbeat.revision,
                heartbeat,
                false
            )
            .await
            .is_err()
    );
    executor(&registrar).reconcile_definitions().await.unwrap();
    assert_eq!(
        registrar.registration(d.name()).await.unwrap().state,
        RegistrationState::Stopping
    );
    let mut j = current(&registrar.store, id).await;
    j.release(claim, Utc::now()).unwrap();
    registrar
        .store
        .save(AccessScope::allow_all(), j.revision, j, false)
        .await
        .unwrap();
    executor(&registrar).reconcile_definitions().await.unwrap();
    assert_eq!(
        registrar.registration(d.name()).await.unwrap().state,
        RegistrationState::Released
    );
    let j = current(&registrar.store, id).await;
    assert_eq!(j.run.status, RunStatus::Cancelled);
    assert!(j.run.activities[0].result.is_none());
}
#[tokio::test]
async fn parallel_cancel_waits_for_every_lease_and_retains_completed_checkpoint() {
    let registrar = registrar(store().await);
    let d = crate::test_support::edit(definition(), |d| {
        d.parallel_groups = vec![d.activities.iter().map(|a| a.id.clone()).collect()];
    });
    registrar.register(d.clone()).await.unwrap();
    let mut j = candidate(&d);
    let id = j.run.id;
    registrar
        .store
        .start(AccessScope::allow_all(), j.clone(), StartOptions::default())
        .await
        .unwrap();
    let a = j
        .claim(d.contract(), 0, Utc::now(), Duration::from_secs(120))
        .unwrap()
        .unwrap();
    let b = j
        .claim(
            d.contract(),
            j.delivery_generation,
            Utc::now(),
            Duration::from_secs(120),
        )
        .unwrap()
        .unwrap();
    j.complete(a, serde_json::json!("saved"), Utc::now())
        .unwrap();
    registrar
        .store
        .save(AccessScope::allow_all(), 0, j, false)
        .await
        .unwrap();
    stop(&registrar, UnregisterMode::CancelAndRelease).await;
    let exec = executor(&registrar);
    exec.reconcile_definitions().await.unwrap();
    assert_eq!(
        registrar.registration(d.name()).await.unwrap().state,
        RegistrationState::Stopping
    );
    let mut j = current(&registrar.store, id).await;
    j.release(b, Utc::now()).unwrap();
    registrar
        .store
        .save(AccessScope::allow_all(), j.revision, j, false)
        .await
        .unwrap();
    exec.reconcile_definitions().await.unwrap();
    assert_eq!(
        registrar.registration(d.name()).await.unwrap().state,
        RegistrationState::Released
    );
    assert_eq!(
        current(&registrar.store, id).await.run.activities[0].result,
        Some(serde_json::json!("saved"))
    );
}
#[cfg(feature = "integration")]
#[path = "../integration/registry_tests.rs"]
mod integration;

struct DenyAction(crate::ServiceAction);
#[async_trait::async_trait]
impl crate::authorization::worker::WorkerAuthorization for DenyAction {
    async fn scope(
        &self,
        action: crate::ServiceAction,
    ) -> Result<AccessScope, crate::store::StoreError> {
        if action == self.0 {
            return Err(crate::store::StoreError::Authorization(
                crate::AuthorizationError::denied(),
            ));
        }
        Ok(AccessScope::allow_all())
    }
}
fn assert_resource_error(
    result: Result<Registration, crate::Error>,
    expected: crate::ErrorKind,
    resource: crate::Resource,
) {
    let Err(error) = result else {
        panic!("operation unexpectedly succeeded");
    };
    assert_eq!(error.kind(), expected, "{error}");
    assert_eq!(error.resource(), Some(resource), "{error}");
}

#[tokio::test]
async fn registrar_denials_identify_the_permission_resource() -> Result<(), crate::Error> {
    let store = store().await;
    let initial = registrar(store.clone());
    let definition = definition();
    let registration = initial.register(definition.clone()).await?;
    for action in [
        crate::ServiceAction::DefinitionRegister,
        crate::ServiceAction::DefinitionGet,
        crate::ServiceAction::DefinitionUnregister,
        crate::ServiceAction::DefinitionActivate,
        crate::ServiceAction::CancelDefinition,
    ] {
        let denied = registrar(
            store
                .clone()
                .with_authorization(Arc::new(DenyAction(action))),
        );
        let result = match action {
            crate::ServiceAction::DefinitionRegister => {
                denied
                    .register_contract(definition.contract().clone())
                    .await
            }
            crate::ServiceAction::DefinitionGet => denied.registration(definition.name()).await,
            crate::ServiceAction::DefinitionActivate => {
                denied
                    .activate(definition.name(), registration.revision)
                    .await
            }
            _ => {
                denied
                    .unregister(
                        definition.name(),
                        UnregisterOptions::new(
                            if action == crate::ServiceAction::CancelDefinition {
                                UnregisterMode::CancelAndRelease
                            } else {
                                UnregisterMode::Retain
                            },
                            registration.revision,
                        ),
                    )
                    .await
            }
        };
        assert_resource_error(
            result,
            crate::ErrorKind::PermissionDenied,
            if action == crate::ServiceAction::CancelDefinition {
                crate::Resource::Run
            } else {
                crate::Resource::Definition
            },
        );
        assert_eq!(initial.registration(definition.name()).await?, registration);
    }
    Ok(())
}

#[tokio::test]
async fn registrar_revision_and_state_errors_identify_definitions() -> Result<(), crate::Error> {
    let registrar = registrar(store().await);
    let definition = definition();
    let active = registrar.register(definition.clone()).await?;
    let retained = registrar
        .unregister(
            definition.name(),
            UnregisterOptions::new(UnregisterMode::Retain, active.revision),
        )
        .await?;
    assert_resource_error(
        registrar.activate(definition.name(), active.revision).await,
        crate::ErrorKind::Aborted,
        crate::Resource::Definition,
    );
    assert_resource_error(
        registrar
            .unregister(
                definition.name(),
                UnregisterOptions::new(UnregisterMode::CancelAndRelease, active.revision),
            )
            .await,
        crate::ErrorKind::Aborted,
        crate::Resource::Definition,
    );
    assert_eq!(registrar.registration(definition.name()).await?, retained);
    let stopping = registrar
        .unregister(
            definition.name(),
            UnregisterOptions::new(UnregisterMode::CancelAndRelease, retained.revision),
        )
        .await?;
    assert_resource_error(
        registrar
            .activate(definition.name(), stopping.revision)
            .await,
        crate::ErrorKind::FailedPrecondition,
        crate::Resource::Definition,
    );
    assert_resource_error(
        registrar.register(definition.clone()).await,
        crate::ErrorKind::FailedPrecondition,
        crate::Resource::Definition,
    );
    assert_eq!(registrar.registration(definition.name()).await?, stopping);
    Ok(())
}

struct ConstrainedCancellation;
#[async_trait::async_trait]
impl crate::authorization::worker::WorkerAuthorization for ConstrainedCancellation {
    async fn scope(
        &self,
        action: crate::ServiceAction,
    ) -> Result<AccessScope, crate::store::StoreError> {
        if action == crate::ServiceAction::CancelDefinition {
            return Ok(AccessScope::single(toolkit_security::ScopeConstraint::new(
                vec![toolkit_security::ScopeFilter::r#in(
                    toolkit_security::pep_properties::OWNER_TENANT_ID,
                    vec![uuid::Uuid::new_v4().into()],
                )],
            )));
        }
        Ok(AccessScope::allow_all())
    }
}
#[tokio::test]
async fn registrar_constrained_global_cancellation_identifies_run_permission()
-> Result<(), crate::Error> {
    let store = store().await;
    let initial = registrar(store.clone());
    let definition = definition();
    let active = initial.register(definition.clone()).await?;
    let denied = registrar(store.with_authorization(Arc::new(ConstrainedCancellation)));
    assert_resource_error(
        denied
            .unregister(
                definition.name(),
                UnregisterOptions::new(UnregisterMode::CancelAndRelease, active.revision),
            )
            .await,
        crate::ErrorKind::PermissionDenied,
        crate::Resource::Run,
    );
    assert_eq!(initial.registration(definition.name()).await?, active);
    Ok(())
}

#[tokio::test]
async fn late_binding_refreshes_only_its_definition_including_fresh_consumed_intents() {
    let registrar = registrar(store().await);
    let first = crate::test_support::edit(definition(), |d| d.name = "late.first.v1".into());
    let second = crate::test_support::edit(definition(), |d| d.name = "late.second.v1".into());
    registrar
        .register_contract(first.contract().clone())
        .await
        .unwrap();
    registrar
        .register_contract(second.contract().clone())
        .await
        .unwrap();
    let first_run = candidate(&first);
    let first_id = first_run.run.id;
    let second_run = candidate(&second);
    let second_id = second_run.run.id;
    let scope = AccessScope::allow_all();
    for run in [first_run, second_run] {
        let intent = uuid::Uuid::new_v5(&run.run.id.0, &run.delivery_generation.to_be_bytes());
        registrar.store.insert(scope.clone(), run).await.unwrap();
        registrar
            .store
            .mark_delivered(&scope, intent, Utc::now())
            .await
            .unwrap();
    }
    assert!(
        registrar
            .store
            .deliveries(&scope, Utc::now(), 100)
            .await
            .unwrap()
            .is_empty()
    );
    registrar.register(first).await.unwrap();
    let reopened = registrar
        .store
        .deliveries(&scope, Utc::now(), 100)
        .await
        .unwrap();
    assert_eq!(
        reopened.len(),
        1,
        "binding one definition must not duplicate other definitions"
    );
    assert_eq!(reopened[0].run_id, first_id);
    // The just-acknowledged second intent may already have been consumed by a
    // worker without handlers. Binding it must not wait for lease-based recovery.
    registrar.register(second).await.unwrap();
    let reopened = registrar
        .store
        .deliveries(&scope, Utc::now(), 100)
        .await
        .unwrap();
    assert_eq!(reopened.len(), 2);
    assert!(reopened.iter().any(|intent| intent.run_id == second_id));
}

/// Answers every service action with a fixed authorization failure.
struct FailingAuthorizer(crate::AuthorizationErrorKind);
#[async_trait::async_trait]
impl crate::Authorizer for FailingAuthorizer {
    async fn caller_scope(
        &self,
        _: &toolkit_security::SecurityContext,
        _: crate::CallerAction,
    ) -> Result<AccessScope, crate::AuthorizationError> {
        Err(crate::AuthorizationError::denied())
    }
    async fn service_scope(
        &self,
        _: crate::ServiceAction,
    ) -> Result<AccessScope, crate::AuthorizationError> {
        Err(match self.0 {
            crate::AuthorizationErrorKind::Denied => crate::AuthorizationError::denied(),
            _ => crate::AuthorizationError::unavailable(),
        })
    }
}

#[tokio::test]
async fn authorizer_failures_reach_registry_callers_with_their_kind() {
    let store = store().await;
    for (failure, expected) in [
        (
            crate::AuthorizationErrorKind::Denied,
            crate::ErrorKind::PermissionDenied,
        ),
        (
            crate::AuthorizationErrorKind::Unavailable,
            crate::ErrorKind::Unavailable,
        ),
    ] {
        let authority = crate::authorization::Authority::new(
            Arc::new(FailingAuthorizer(failure)),
            Duration::from_secs(5),
        );
        let denied = registrar(store.clone().with_authorization(Arc::new(authority)));
        let error = denied.register(definition()).await.unwrap_err();
        assert_eq!(error.kind(), expected, "{error}");
        assert!(std::error::Error::source(&error).is_some());
    }
}
