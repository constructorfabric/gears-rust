// Created: 2026-08-12 by Constructor Tech
//! Tests for the gear scaffold's startup contract.
//!
//! `Gear::init` needs a live `GearCtx` (database capability, config provider),
//! so its happy path belongs to the integration suite. What is pinned here is
//! the part that must hold before any of that: the gear must not hand out
//! resources it has not acquired.

use super::SettingsService;

#[test]
fn an_uninitialized_gear_refuses_to_hand_out_config() {
    // Returning a default here instead of an error is exactly the failure the
    // fail-closed bootstrap exists to prevent, one layer up.
    let gear = SettingsService::default();
    assert!(gear.config().is_err());
}

#[test]
fn an_uninitialized_gear_refuses_to_hand_out_the_database() {
    let gear = SettingsService::default();
    assert!(gear.db().is_err());
}

#[test]
fn the_accessor_errors_name_the_gear() {
    // Startup failures are read in aggregated logs where the message may be the
    // only clue which gear produced it.
    let gear = SettingsService::default();
    let err = gear.config().expect_err("uninitialized");
    assert!(err.to_string().contains("settings-service"), "got `{err}`");
}

#[test]
fn the_gear_hands_its_migrations_to_the_capability() {
    // The capability is the only route by which ToolKit learns there is a schema
    // to apply. A harness that existed but was never handed over would leave the
    // gear starting cleanly against a database it had never migrated.
    use sea_orm_migration::MigratorTrait;
    use toolkit::DatabaseCapability;

    let gear = SettingsService::default();
    let handed_over: Vec<String> = gear
        .migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let harness: Vec<String> = crate::infra::storage::migrations::Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();

    assert!(!handed_over.is_empty());
    assert_eq!(
        handed_over, harness,
        "the capability must expose the harness itself, not a separate list"
    );
}

#[test]
fn an_uninitialized_gear_refuses_to_hand_out_the_types_registry() {
    // The registry is the one client resolved at init -- the one `deps` entry
    // -- because a read that reached the database before discovering the
    // registry is missing has already spent the authorization and the query.
    let gear = SettingsService::default();
    assert!(gear.types().is_err());
}

#[test]
fn an_uninitialized_gear_refuses_to_hand_out_the_enforcer() {
    // Handing back a permissive default here would turn every unenforced
    // handler into an allow. There is no default to hand back.
    let gear = SettingsService::default();
    assert!(gear.enforcer().is_err());
}

#[test]
fn an_uninitialized_gear_refuses_to_hand_out_the_validator() {
    // A default validator that accepted everything would be the vacuous pass
    // the fail-closed rule exists to prevent.
    let gear = SettingsService::default();
    assert!(gear.validator().is_err());
}

#[test]
fn an_uninitialized_gear_refuses_to_hand_out_every_service_it_builds() {
    // Each of these is reached by a route handler on the first request. A
    // `OnceLock` that answered with a default would put a half-built service
    // behind a live endpoint; an error keeps the failure at startup, where the
    // operator is still watching.
    let gear = SettingsService::default();
    let refusals = [
        gear.resolver().err().map(|e| e.to_string()),
        gear.writes().err().map(|e| e.to_string()),
        gear.access().err().map(|e| e.to_string()),
        gear.hierarchy().err().map(|e| e.to_string()),
    ];
    for refusal in refusals {
        let message = refusal.expect("an uninitialized accessor refuses");
        assert!(
            message.contains("settings-service") && message.contains("not initialized"),
            "got `{message}`"
        );
    }
}

/// A gear holding nothing but the write coordinator, which is all the managed
/// lifecycle touches.
fn gear_with_writes(
    inner: &crate::test_support::ResolutionHarness,
    secrets: &std::sync::Arc<crate::test_support::RecordingSecrets>,
) -> std::sync::Arc<SettingsService> {
    let writes = crate::test_support::write_coordinator(
        inner,
        std::sync::Arc::clone(secrets),
        std::sync::Arc::new(crate::test_support::RecordingPublisher::default()),
        std::sync::Arc::new(crate::test_support::FixedStepUp::verified()),
    );
    let gear = SettingsService::default();
    gear.writes
        .set(writes)
        .map_err(|_| "already set")
        .expect("a fresh gear");
    std::sync::Arc::new(gear)
}

/// Stage an expired secret directly, the way an abandoned stage leaves one.
async fn seed_expired_stage(
    inner: &crate::test_support::ResolutionHarness,
    secrets: &crate::test_support::RecordingSecrets,
    declaration_id: uuid::Uuid,
) -> uuid::Uuid {
    use crate::domain::secrets::pending::{PendingSecretDraft, PendingSecretRepository as _};
    secrets.seed("abandoned-ref", "hunter2");
    let conn = inner.db.conn().expect("connection");
    crate::infra::storage::pending_secret_repo::PendingSecretRepo
        .insert(
            &conn,
            &toolkit_security::AccessScope::allow_all(),
            PendingSecretDraft {
                declaration_id,
                tenant_id: inner.tree.root,
                subject_id: "someone who walked away".to_owned(),
                secret_ref: "abandoned-ref".to_owned(),
                expires_at: time::OffsetDateTime::now_utc() - time::Duration::minutes(1),
            },
        )
        .await
        .expect("a staged secret")
        .id
}

#[tokio::test]
async fn the_managed_lifecycle_sweeps_abandoned_stages_and_stops_when_cancelled() {
    // The gear's only long-running work. It has to announce readiness, release
    // what nobody claimed, and return when the runtime cancels it — a sweep
    // that outlived cancellation would hold the shutdown open past its timeout.
    use crate::domain::secrets::pending::PendingSecretRepository as _;

    let inner = crate::test_support::ResolutionHarness::new().await;
    let id = inner
        .declare_typed(
            "api_token",
            "cascading",
            serde_json::json!(""),
            crate::test_support::SECRET,
            "secret",
        )
        .await;
    let secrets = std::sync::Arc::new(crate::test_support::RecordingSecrets::default());
    let pending_id = seed_expired_stage(&inner, &secrets, id).await;
    let gear = gear_with_writes(&inner, &secrets);

    let cancel = tokio_util::sync::CancellationToken::new();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(SettingsService::serve(
        std::sync::Arc::clone(&gear),
        cancel.clone(),
        toolkit::lifecycle::ReadySignal::from_sender(tx),
    ));

    rx.await.expect("the sweep announces readiness");
    // The store release is the pass's last step, so waiting for it waits for
    // the whole of it.
    let mut released = false;
    for _ in 0..1000u32 {
        if secrets.held().is_empty() {
            released = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(released, "the first tick releases what has expired");

    let conn = inner.db.conn().expect("connection");
    let scope = toolkit_security::AccessScope::allow_all();
    assert!(
        crate::infra::storage::pending_secret_repo::PendingSecretRepo
            .find(&conn, &scope, pending_id)
            .await
            .expect("a readable table")
            .is_none(),
        "and the row goes with the entry"
    );

    cancel.cancel();
    serving
        .await
        .expect("the task joins")
        .expect("and returns cleanly");
}

#[tokio::test]
async fn the_managed_lifecycle_refuses_to_start_before_init() {
    // `serve` is the runtime's entry point, so it runs whether or not init
    // reached the write coordinator. Announcing readiness first and failing
    // afterwards would flip the gear to Running with nothing behind it.
    let gear = std::sync::Arc::new(SettingsService::default());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let outcome = SettingsService::serve(
        gear,
        tokio_util::sync::CancellationToken::new(),
        toolkit::lifecycle::ReadySignal::from_sender(tx),
    )
    .await;
    assert!(outcome.is_err(), "an uninitialized gear has nothing to run");
    assert!(rx.await.is_err(), "and it never announced readiness");
}

#[tokio::test]
async fn a_sweep_pass_that_finds_nothing_is_not_an_event() {
    // The pass runs once a minute forever; only a release is worth a line.
    let inner = crate::test_support::ResolutionHarness::new().await;
    let secrets = std::sync::Arc::new(crate::test_support::RecordingSecrets::default());
    let writes = crate::test_support::write_coordinator(
        &inner,
        std::sync::Arc::clone(&secrets),
        std::sync::Arc::new(crate::test_support::RecordingPublisher::default()),
        std::sync::Arc::new(crate::test_support::FixedStepUp::verified()),
    );
    SettingsService::sweep_once(&writes).await;
    assert!(secrets.held().is_empty());
}

#[tokio::test]
async fn a_store_that_cannot_release_leaves_the_row_gone_and_the_pass_reporting_success() {
    // The row and its entry are released together, but the store is a separate
    // system: one that cannot answer orphans the entry. That is a warning and
    // the pass goes on — a sweep that stopped on it would let the expired rows
    // behind it pile up forever.
    use crate::domain::secrets::pending::PendingSecretRepository as _;

    let inner = crate::test_support::ResolutionHarness::new().await;
    let id = inner
        .declare_typed(
            "api_token",
            "cascading",
            serde_json::json!(""),
            crate::test_support::SECRET,
            "secret",
        )
        .await;
    let secrets = std::sync::Arc::new(crate::test_support::RecordingSecrets::default());
    let pending_id = seed_expired_stage(&inner, &secrets, id).await;
    let writes = crate::test_support::write_coordinator(
        &inner,
        std::sync::Arc::clone(&secrets),
        std::sync::Arc::new(crate::test_support::RecordingPublisher::default()),
        std::sync::Arc::new(crate::test_support::FixedStepUp::verified()),
    );
    secrets.go_down();

    SettingsService::sweep_once(&writes).await;

    let conn = inner.db.conn().expect("connection");
    let scope = toolkit_security::AccessScope::allow_all();
    assert!(
        crate::infra::storage::pending_secret_repo::PendingSecretRepo
            .find(&conn, &scope, pending_id)
            .await
            .expect("a readable table")
            .is_none(),
        "the row goes whatever the store does: nothing points at the entry any more"
    );
    assert_eq!(
        secrets.held(),
        vec!["abandoned-ref".to_owned()],
        "and the entry is orphaned rather than silently forgotten"
    );

    // The next tick still runs, and finds nothing left to do.
    SettingsService::sweep_once(&writes).await;
}
