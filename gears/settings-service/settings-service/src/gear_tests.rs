// Created: 2026-08-12 by Virtuozzo International GmbH
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
    // The lifecycle also runs the audit retention pass, over the database
    // with the configured default.
    gear.db
        .set(std::sync::Arc::clone(&inner.db))
        .map_err(|_| "already set")
        .expect("a fresh gear");
    gear.config
        .set(std::sync::Arc::new(
            serde_json::from_value(serde_json::json!({})).expect("the defaults parse"),
        ))
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
    // The sweep's database work runs on the blocking pool, so the release
    // lands on another thread at a time this task cannot observe directly.
    // The wait is therefore on the clock and not on scheduler turns: a
    // fixed number of `yield_now`s is a race, and a slow machine loses it.
    // The store release is the pass's last step, so waiting for it waits for
    // the whole of it.
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !secrets.held().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first tick releases what has expired");

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
async fn a_store_that_cannot_release_keeps_the_row_for_the_next_tick_which_releases_it() {
    // The entry is released first and the row removed after: a store that
    // cannot answer leaves the row as the durable handle on the entry, and
    // the pass goes on — a sweep that stopped on it would let the expired
    // rows behind it pile up forever. The next tick tries again.
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
            .is_some(),
        "the row stays while the entry is unreleased: it is what the next pass retries by"
    );
    assert_eq!(secrets.held(), vec!["abandoned-ref".to_owned()]);

    // The store is back: the next tick releases the entry and drops the row.
    secrets
        .unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    SettingsService::sweep_once(&writes).await;
    assert!(
        crate::infra::storage::pending_secret_repo::PendingSecretRepo
            .find(&conn, &scope, pending_id)
            .await
            .expect("a readable table")
            .is_none()
    );
    assert!(secrets.held().is_empty());
    assert_eq!(
        secrets.deleted.lock().expect("lock").as_slice(),
        ["abandoned-ref".to_owned()]
    );
}

// ── Init, over a real context ────────────────────────────────────────────────

use std::sync::Arc;

use serde_json::{Value, json};
use toolkit::{ClientHub, ConfigProvider, Gear as _, GearCtx, RestApiCapability as _};
use tower::ServiceExt as _;
use uuid::Uuid;

/// A config provider answering one gear from one document.
struct FixedConfig(Value);

impl ConfigProvider for FixedConfig {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        (gear == SettingsService::MODULE_NAME).then_some(&self.0)
    }
}

/// A hub holding the two clients init resolves.
fn wired_hub() -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(
        types_registry_sdk::testing::MockTypesRegistryClient::new(),
    ));
    hub.register::<dyn credstore_sdk::CredStoreClientV1>(Arc::new(
        credstore_sdk::test_util::MockCredStoreClient::empty(),
    ));
    hub
}

/// A context over a migrated in-memory database, a wired hub, and the given
/// `config` section.
async fn context_with(section: Value, hub: Arc<ClientHub>) -> GearCtx {
    let db = crate::test_support::sqlite_provider().await;
    GearCtx::new(
        SettingsService::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(json!({ "config": section }))),
        hub,
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db((*db).clone())
}

/// The ordinary deployment: nothing configured, every default taken.
async fn default_context() -> GearCtx {
    context_with(json!({}), wired_hub()).await
}

#[tokio::test]
async fn init_fills_every_lock_and_binds_both_sdk_traits_into_the_hub() {
    // The two traits are how every other gear reaches settings: one to read an
    // effective value, one to contribute a declaration from its own init. A
    // gear that came up without binding them would leave its consumers
    // resolving nothing, and they resolve *after* us by construction.
    let hub = wired_hub();
    let ctx = context_with(json!({}), Arc::clone(&hub)).await;
    let gear = SettingsService::default();
    gear.init(&ctx).await.expect("init over a live context");

    for accessor in [
        gear.config().is_ok(),
        gear.db().is_ok(),
        gear.enforcer().is_ok(),
        gear.types().is_ok(),
        gear.validator().is_ok(),
        gear.resolver().is_ok(),
        gear.writes().is_ok(),
        gear.access().is_ok(),
        gear.hierarchy().is_ok(),
    ] {
        assert!(accessor, "init leaves no accessor empty");
    }
    assert!(
        hub.get::<dyn settings_service_sdk::api::SettingsReaderClient>()
            .is_ok(),
        "the reader is bound"
    );
    assert!(
        hub.get::<dyn settings_service_sdk::api::SettingsContributionClient>()
            .is_ok(),
        "and the contribution door with it"
    );
}

#[tokio::test]
async fn init_takes_the_design_fixed_defaults_when_nothing_is_configured() {
    let ctx = default_context().await;
    let gear = SettingsService::default();
    gear.init(&ctx).await.expect("init");
    let config = gear.config().expect("config");
    assert_eq!(config.cache_ttl_seconds, 30);
    assert_eq!(config.cache_max_entries, 500_000);
    assert_eq!(config.audit_retention_days, 365);
    assert_eq!(config.step_up.max_age_seconds, 300);
}

#[tokio::test]
async fn a_second_init_is_refused_rather_than_quietly_rebuilding() {
    // `OnceLock::set` failing is the only signal that the runtime called init
    // twice; swallowing it would leave two resolvers over one database, each
    // with its own cache.
    let gear = SettingsService::default();
    gear.init(&default_context().await).await.expect("first");
    let err = gear
        .init(&default_context().await)
        .await
        .expect_err("second");
    assert!(
        err.to_string().contains("already initialized"),
        "got `{err}`"
    );
}

#[tokio::test]
async fn init_refuses_a_retention_shorter_than_the_platform_keeps() {
    // Below twelve months the store would prune what the platform is required
    // to hold, and the pruning would be invisible.
    let ctx = context_with(json!({ "audit_retention_days": 30 }), wired_hub()).await;
    let err = SettingsService::default()
        .init(&ctx)
        .await
        .expect_err("a short retention");
    let message = err.to_string();
    assert!(
        message.contains("audit_retention_days") && message.contains("365"),
        "the message names the value and the floor: `{message}`"
    );
}

#[tokio::test]
async fn init_refuses_a_remote_binding_for_a_trait_bound_in_process() {
    // R1 publishes no remote contract for either SDK trait, so a deployment
    // asking for one would resolve to nothing. Better a boot failure.
    let ctx = context_with(
        json!({ "client_wiring": { "settings_reader_client": { "transport": "rest" } } }),
        wired_hub(),
    )
    .await;
    let err = SettingsService::default()
        .init(&ctx)
        .await
        .expect_err("a remote binding");
    assert!(
        err.to_string().contains("settings_reader_client"),
        "got `{err}`"
    );
}

#[tokio::test]
async fn init_refuses_a_step_up_window_wider_than_the_design_allows() {
    let ctx = context_with(
        json!({ "step_up": { "max_age_seconds": 3600 } }),
        wired_hub(),
    )
    .await;
    assert!(
        SettingsService::default().init(&ctx).await.is_err(),
        "a window above five minutes is not a deployment choice"
    );
}

#[tokio::test]
async fn init_refuses_a_key_the_schema_does_not_know() {
    // A mistyped key silently ignored would leave the operator believing they
    // had configured something they had not.
    let ctx = context_with(json!({ "cache_ttl_second": 5 }), wired_hub()).await;
    assert!(SettingsService::default().init(&ctx).await.is_err());
}

#[tokio::test]
async fn init_refuses_to_start_without_the_registry_it_calls_during_init() {
    // The one `deps` entry. Registering this gear's GTS schemas is a real call,
    // so a registry that is absent must be found here — not by a read that has
    // already passed authorization and reached the database.
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn credstore_sdk::CredStoreClientV1>(Arc::new(
        credstore_sdk::test_util::MockCredStoreClient::empty(),
    ));
    let ctx = context_with(json!({}), hub).await;
    let err = SettingsService::default()
        .init(&ctx)
        .await
        .expect_err("no registry");
    assert!(err.to_string().contains("types registry"), "got `{err}`");
}

#[tokio::test]
async fn init_refuses_to_start_without_the_credential_store() {
    // Secrets never live in this gear's rows, so a deployment without the store
    // could accept a secret setting and have nowhere to put its value.
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(
        types_registry_sdk::testing::MockTypesRegistryClient::new(),
    ));
    let ctx = context_with(json!({}), hub).await;
    let err = SettingsService::default()
        .init(&ctx)
        .await
        .expect_err("no credential store");
    assert!(err.to_string().contains("credstore"), "got `{err}`");
}

// ── The REST capability ──────────────────────────────────────────────────────

#[tokio::test]
async fn the_rest_capability_registers_every_surface_the_gear_owns() {
    // The one check that the router the runtime mounts carries what the gear
    // built. The test harness registers the six families itself, so a family
    // wired in init but never merged here would be invisible everywhere else.
    let ctx = default_context().await;
    let gear = SettingsService::default();
    gear.init(&ctx).await.expect("init");

    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let router = gear
        .register_rest(&ctx, axum::Router::new(), &openapi)
        .expect("the capability registers");

    // The router the runtime would mount answers on a registered path. What it
    // answers is another layer's business — here it only must not be a 404.
    let mut request = axum::http::Request::builder()
        .method("GET")
        .uri("/settings-service/v1/categories")
        .body(axum::body::Body::empty())
        .expect("a well-formed request");
    request
        .extensions_mut()
        .insert(crate::test_support::context_for(Uuid::new_v4()));
    let answer = router.oneshot(request).await.expect("the router answers");
    assert_ne!(
        answer.status(),
        axum::http::StatusCode::NOT_FOUND,
        "the categories listing is mounted"
    );

    let operations: Vec<String> = openapi
        .operation_specs
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    let joined = operations.join("\n");
    // One operation per family, named the way the contract names it.
    for surface in [
        "GET:/settings-service/v1/categories",
        "GET:/settings-service/v1/declarations",
        "GET:/settings-service/v1/settings",
        "PUT:/settings-service/v1/settings/{key}/value",
        "POST:/settings-service/v1/settings/batch",
        "GET:/settings-service/v1/settings/{key}/permissions",
        "GET:/settings-service/v1/search",
    ] {
        assert!(
            joined.contains(surface),
            "`{surface}` is not among the registered operations:\n{joined}"
        );
    }
    for operation in &operations {
        let (_, path) = operation.split_once(':').expect("METHOD:path");
        assert!(
            path.starts_with("/settings-service/v1/"),
            "every route lives under this gear's own prefix: `{operation}`"
        );
    }
}

#[tokio::test]
async fn the_rest_capability_refuses_before_init_and_names_what_is_missing() {
    // Mounting a router over half-built services would answer requests with a
    // panic per route instead of a startup failure.
    let ctx = default_context().await;
    let err = SettingsService::default()
        .register_rest(
            &ctx,
            axum::Router::new(),
            &toolkit::api::OpenApiRegistryImpl::new(),
        )
        .expect_err("nothing is built yet");
    assert!(
        err.to_string().contains("category service not initialized"),
        "got `{err}`"
    );
}

#[tokio::test]
async fn init_refuses_a_cache_ttl_wider_than_the_design_backstop_or_zero() {
    // Thirty seconds is the ceiling on how stale a replica may serve after a
    // missed invalidation; a deployment may tighten it, never widen it. A
    // zero would put every read on the database, like a cache of no entries.
    let ctx = context_with(json!({ "cache_ttl_seconds": 999_999 }), wired_hub()).await;
    let err = SettingsService::default()
        .init(&ctx)
        .await
        .expect_err("a wider backstop");
    let message = err.to_string();
    assert!(
        message.contains("cache_ttl_seconds") && message.contains("30"),
        "the message names the field and the ceiling: `{message}`"
    );

    let ctx = context_with(json!({ "cache_ttl_seconds": 0 }), wired_hub()).await;
    assert!(SettingsService::default().init(&ctx).await.is_err());

    let ctx = context_with(json!({ "cache_ttl_seconds": 5 }), wired_hub()).await;
    SettingsService::default()
        .init(&ctx)
        .await
        .expect("a shorter backstop is the deployment's to choose");
}

#[tokio::test]
async fn init_refuses_a_blank_step_up_pin() {
    // An empty issuer is a templating mistake that would refuse every
    // step-up-gated write at runtime; the boot is where it is caught.
    let ctx = context_with(json!({ "step_up": { "issuer": "" } }), wired_hub()).await;
    let err = SettingsService::default()
        .init(&ctx)
        .await
        .expect_err("a blank issuer");
    assert!(err.to_string().contains("step_up.issuer"), "got `{err}`");
}

#[tokio::test]
async fn a_retention_pass_prunes_what_is_past_its_horizon_and_nothing_younger() {
    // The pass the lifecycle runs once a day: records past the configured
    // default leave, a fresh record stays. `now` is passed in, so the test
    // moves the clock instead of waiting a year.
    use crate::audit::{AuditOperation, AuditRecord, AuditSink as _};
    let db = crate::test_support::sqlite_provider().await;
    let tenant = uuid::Uuid::new_v4();
    {
        let conn = db.conn().expect("connection");
        crate::infra::storage::audit_store::AuditStore
            .append(
                &conn,
                &toolkit_security::AccessScope::allow_all(),
                AuditRecord::new("k", Some(tenant), "admin", AuditOperation::Change, "req"),
            )
            .await
            .expect("append");
    }
    let year = std::time::Duration::from_hours(365 * 24);
    let now = time::OffsetDateTime::now_utc();

    let live = tokio_util::sync::CancellationToken::new();
    let batches = super::PruneBatches {
        size: 1_000,
        per_tick: 10,
    };
    assert_eq!(
        SettingsService::prune_once(&db, year, now, batches, &live).await,
        0,
        "a fresh record stays"
    );
    assert_eq!(
        SettingsService::prune_once(&db, year, now + time::Duration::days(400), batches, &live)
            .await,
        1,
        "past its horizon, it leaves"
    );
}

#[tokio::test]
async fn a_retention_pass_works_in_batches_up_to_its_cap_and_stops_when_cancelled() {
    use crate::audit::{AuditOperation, AuditRecord, AuditSink as _};
    let db = crate::test_support::sqlite_provider().await;
    {
        let conn = db.conn().expect("connection");
        for i in 0..5 {
            crate::infra::storage::audit_store::AuditStore
                .append(
                    &conn,
                    &toolkit_security::AccessScope::allow_all(),
                    AuditRecord::new("k", None, "admin", AuditOperation::Change, format!("r{i}")),
                )
                .await
                .expect("append");
        }
    }
    let year = std::time::Duration::from_hours(365 * 24);
    let later = time::OffsetDateTime::now_utc() + time::Duration::days(400);
    let live = tokio_util::sync::CancellationToken::new();

    // Batches of two, two batches a tick: four go now, the last one next tick.
    let capped = super::PruneBatches {
        size: 2,
        per_tick: 2,
    };
    assert_eq!(
        SettingsService::prune_once(&db, year, later, capped, &live).await,
        4
    );
    assert_eq!(
        SettingsService::prune_once(&db, year, later, capped, &live).await,
        1
    );
    assert_eq!(
        SettingsService::prune_once(&db, year, later, capped, &live).await,
        0
    );

    // A lifecycle being stopped prunes nothing more.
    {
        let conn = db.conn().expect("connection");
        crate::infra::storage::audit_store::AuditStore
            .append(
                &conn,
                &toolkit_security::AccessScope::allow_all(),
                AuditRecord::new("k", None, "admin", AuditOperation::Change, "late"),
            )
            .await
            .expect("append");
    }
    let stopped = tokio_util::sync::CancellationToken::new();
    stopped.cancel();
    assert_eq!(
        SettingsService::prune_once(&db, year, later, capped, &stopped).await,
        0
    );
}

/// Records what the review pass publishes, in order.
#[derive(Default)]
struct RecordingReviewMetrics(std::sync::Mutex<Vec<(&'static str, u64)>>);

impl crate::domain::ports::ReviewMetrics for RecordingReviewMetrics {
    fn needs_review(&self, source: &'static str, count: u64) {
        self.0.lock().expect("lock").push((source, count));
    }
}

#[tokio::test]
async fn a_review_pass_counts_the_flagged_overrides_by_source_and_says_zero_where_none() {
    // The pass the lifecycle runs once a minute. Every source is published
    // each time, zero included: a gauge that skipped a source with nothing
    // flagged would keep showing its last count after the fix.
    let h = crate::test_support::ResolutionHarness::new().await;
    let d = h
        .declare("proxy", "cascading", serde_json::json!(true))
        .await;
    h.set_flagged(d, h.tree.a, serde_json::json!("aggressive"))
        .await;
    h.set_flagged(d, h.tree.b, serde_json::json!("gentle"))
        .await;
    h.set(d, h.tree.root, serde_json::json!(false)).await;

    let metrics = RecordingReviewMetrics::default();
    SettingsService::review_once(&h.db, &metrics).await;
    assert_eq!(
        *metrics.0.lock().expect("lock"),
        vec![("admin_authored", 0), ("module_contributed", 2)],
        "the harness declares as a module; the unflagged row is not counted"
    );

    // An administrator's declaration with one flagged row is counted under
    // its own source, and the module's count is unchanged.
    let admin = {
        use crate::domain::declaration::{DeclarationDraft, DeclarationRepository as _};
        let conn = h.db.conn().expect("connection");
        crate::infra::storage::declaration_repo::DeclarationRepo
            .insert(
                &conn,
                &toolkit_security::AccessScope::allow_all(),
                DeclarationDraft {
                    key: "gts.cf.core.settings.setting_type.v1~acme.settings.network.retry.v1~"
                        .to_owned(),
                    leaf_slug: "retry".to_owned(),
                    value_type_id: crate::test_support::BOOL.to_owned(),
                    category_id: h.category_id(),
                    default_value: serde_json::json!(false),
                    scope_class: "cascading".to_owned(),
                    mode: "standard".to_owned(),
                    requires_step_up: false,
                    anonymous_exposable: false,
                    domain_affinity: None,
                    has_secret_trait: false,
                    data_classification: "public".to_owned(),
                    source: "admin_authored".to_owned(),
                    owner_module: None,
                    licence_feature: None,
                    description: None,
                    created_by: "test".to_owned(),
                },
            )
            .await
            .expect("an admin declaration")
            .id
    };
    h.set_flagged(admin, h.tree.a, serde_json::json!("often"))
        .await;
    let metrics = RecordingReviewMetrics::default();
    SettingsService::review_once(&h.db, &metrics).await;
    assert_eq!(
        *metrics.0.lock().expect("lock"),
        vec![("admin_authored", 1), ("module_contributed", 2)]
    );
}
