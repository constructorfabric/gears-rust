//! Tests for the gear scaffold's startup contract.
//!
//! `Gear::init` needs a live `GearCtx` (database capability, config provider),
//! so its happy path belongs to the integration suite. What is pinned here is
//! the part that must hold before any of that: the gear must not hand out
//! resources it has not acquired.
//!
//! Log-capture assertions assume one process per test (nextest, as the gear's
//! `make` targets run) and may flake under plain `cargo test`.

use super::SettingsService;
use crate::domain::error::DomainError;

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
fn an_uninitialized_gear_refuses_to_hand_out_the_names() {
    // The name resolver is built at init with the hub's optional directory;
    // a default here would answer for a hub it never saw.
    let gear = SettingsService::default();
    assert!(gear.names().is_err());
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
    // `serve` also runs the cache invalidation consumer; with no broker in the
    // hub it only waits.
    gear.cache
        .set(std::sync::Arc::new(
            crate::domain::resolution::EffectiveCache::new(std::time::Duration::from_secs(30)),
        ))
        .map_err(|_| "already set")
        .expect("a fresh gear");
    gear.event_broker
        .set(std::sync::Arc::new(
            crate::infra::event_broker::EventBrokerClient::new(
                std::sync::Arc::new(toolkit::ClientHub::new()),
                std::sync::Arc::new(NoRegistry),
                std::sync::Arc::new(crate::test_support::FixedScope(uuid::Uuid::from_u128(1))),
                std::time::Duration::from_secs(2),
                uuid::Uuid::from_u128(2),
            ),
        ))
        .map_err(|_| "already set")
        .expect("a fresh gear");
    // ... and the activation delivery loop, which idles without a broker.
    let config = gear.config().expect("set above");
    gear.bind_notifications(&config)
        .expect("the notification half binds");
    // ... and the activation client, whose stop the lifecycle awaits: over a
    // service that answers nothing, with no broker in its hub.
    gear.activations
        .set(std::sync::Arc::new(
            crate::infra::activation_client::ActivationClient::new(
                crate::infra::activation_client::ConsumerDeps {
                    backend: std::sync::Arc::new(NoService),
                    reader: std::sync::Arc::new(NoService),
                    metrics: gear.notify_metrics().expect("bound above"),
                    hub: std::sync::Arc::new(toolkit::ClientHub::new()),
                    timing: crate::infra::activation_client::ClientTiming::standard(),
                    identity: settings_service_sdk::InstanceIdentity::process().clone(),
                },
            ),
        ))
        .map_err(|_| "already set")
        .expect("a fresh gear");
    std::sync::Arc::new(gear)
}

/// The service behind the activation client of the lifecycle tests: nothing
/// subscribes through it, so it only refuses.
struct NoService;

fn unavailable() -> DomainError {
    DomainError::Unavailable {
        detail: "no service in this test".to_owned(),
    }
}

#[async_trait::async_trait]
impl crate::infra::activation_client::ActivationBackend for NoService {
    async fn register(
        &self,
        _: &toolkit_security::SecurityContext,
        _: &settings_service_sdk::SettingKey,
        _: uuid::Uuid,
        _: &settings_service_sdk::InstanceIdentity,
    ) -> Result<(), crate::infra::activation_client::RegisterError> {
        Err(crate::infra::activation_client::RegisterError::Unavailable(
            "internal".to_owned(),
        ))
    }

    async fn unsubscribe(
        &self,
        _: &toolkit_security::SecurityContext,
        _: &settings_service_sdk::SettingKey,
        _: uuid::Uuid,
        _: uuid::Uuid,
    ) -> Result<(), crate::infra::activation_client::UnsubscribeFailure> {
        Err(crate::infra::activation_client::UnsubscribeFailure::Unavailable)
    }

    async fn report(
        &self,
        _: &toolkit_security::SecurityContext,
        _: crate::domain::activation::BackResponse,
    ) -> Result<crate::domain::activation::ReportResult, DomainError> {
        Err(unavailable())
    }
}

#[async_trait::async_trait]
impl settings_service_sdk::api::SettingsReaderClient for NoService {
    async fn get_effective(
        &self,
        _: &toolkit_security::SecurityContext,
        _: settings_service_sdk::models::GetEffectiveRequest,
    ) -> Result<
        settings_service_sdk::models::EffectiveValueResponse,
        toolkit_canonical_errors::CanonicalError,
    > {
        Err(unavailable().into())
    }

    async fn get_effective_bulk(
        &self,
        _: &toolkit_security::SecurityContext,
        _: settings_service_sdk::api::BulkSelector,
        _: String,
    ) -> Result<Vec<settings_service_sdk::api::BulkOutcome>, toolkit_canonical_errors::CanonicalError>
    {
        Err(unavailable().into())
    }

    async fn resolve_secret(
        &self,
        _: &toolkit_security::SecurityContext,
        _: settings_service_sdk::SecretHandle,
    ) -> Result<secrecy::SecretString, toolkit_canonical_errors::CanonicalError> {
        Err(unavailable().into())
    }
}

/// A registry the lifecycle tests never reach: no broker is in their hub.
struct NoRegistry;

#[async_trait::async_trait]
impl crate::infra::event_broker::CacheSchemaRegistry for NoRegistry {
    async fn register_topic(
        &self,
        _: serde_json::Value,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(Vec::new())
    }

    async fn register_event_type(
        &self,
        _: serde_json::Value,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(Vec::new())
    }
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
    SettingsService::sweep_once(&writes, &tokio_util::sync::CancellationToken::new()).await;
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

    SettingsService::sweep_once(&writes, &tokio_util::sync::CancellationToken::new()).await;

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
    SettingsService::sweep_once(&writes, &tokio_util::sync::CancellationToken::new()).await;
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
        gear.names().is_ok(),
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
    assert_eq!(config.publish_timeout_seconds, 2);
    assert_eq!(config.mutation_lock_wait_seconds, 10);
    assert_eq!(config.redeliver_interval_seconds, 30);
    assert_eq!(config.audit_retention_days, 365);
    assert_eq!(config.step_up.max_age_seconds, 300);
}

#[tokio::test]
async fn init_binds_the_event_broker_client_without_resolving_the_broker() {
    // The hub of the default context holds no broker: init still succeeds, and
    // the client it bound has resolved nothing.
    let ctx = default_context().await;
    let gear = SettingsService::default();
    gear.init(&ctx)
        .await
        .expect("init with no broker in the hub");
    let client = gear.event_broker().expect("the client is bound");
    assert!(client.broker().is_none());
    let state = client.state();
    assert!(!state.types_registered && !state.root_tenant_known);
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
    let db = crate::test_support::sqlite_provider().await;
    let ctx = GearCtx::new(
        SettingsService::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(json!({
            "config": {},
            "clients": { "provides": { "settings_reader_client": {
                "transport": "rest", "endpoint": "http://elsewhere"
            } } },
        }))),
        wired_hub(),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db((*db).clone());
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
async fn init_refuses_a_publish_timeout_of_zero_or_above_ten_without_a_broker_in_the_hub() {
    // The broadcast is bounded so a request never waits on the broker for
    // long; a zero would time every publish out and a long one would stall the
    // request. The hub holds no broker here, and init neither resolves nor
    // waits for one.
    for seconds in [0, 11] {
        let ctx = context_with(json!({ "publish_timeout_seconds": seconds }), wired_hub()).await;
        let err = SettingsService::default()
            .init(&ctx)
            .await
            .expect_err("outside 1..=10");
        let message = err.to_string();
        assert!(
            message.contains("publish_timeout_seconds") && message.contains("between 1 and 10"),
            "the message names the field and the bounds: `{message}`"
        );
    }

    let ctx = context_with(json!({ "publish_timeout_seconds": 10 }), wired_hub()).await;
    SettingsService::default()
        .init(&ctx)
        .await
        .expect("ten seconds is the ceiling, and init completes with no broker");
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
    use crate::audit::{ActorTenant, AuditOperation, AuditRecord, AuditSink as _};
    let db = crate::test_support::sqlite_provider().await;
    let tenant = uuid::Uuid::new_v4();
    {
        let conn = db.conn().expect("connection");
        crate::infra::storage::audit_store::AuditStore
            .append(
                &conn,
                &toolkit_security::AccessScope::allow_all(),
                AuditRecord::new(
                    "k",
                    Some(tenant),
                    "admin",
                    ActorTenant::machine(),
                    AuditOperation::Change,
                    "req",
                ),
            )
            .await
            .expect("append");
    }
    let year = std::time::Duration::from_hours(365 * 24);
    let now = time::OffsetDateTime::now_utc();

    let live = tokio_util::sync::CancellationToken::new();
    let quiet = RecordingLifecycleMetrics::default();
    let batches = super::PruneBatches {
        size: 1_000,
        per_tick: 10,
    };
    assert_eq!(
        SettingsService::prune_once(&db, year, now, batches, &live, &quiet).await,
        0,
        "a fresh record stays"
    );
    assert_eq!(
        SettingsService::prune_once(
            &db,
            year,
            now + time::Duration::days(400),
            batches,
            &live,
            &quiet
        )
        .await,
        1,
        "past its horizon, it leaves"
    );
}

#[tokio::test]
async fn a_retention_pass_works_in_batches_up_to_its_cap_and_stops_when_cancelled() {
    use crate::audit::{ActorTenant, AuditOperation, AuditRecord, AuditSink as _};
    let db = crate::test_support::sqlite_provider().await;
    {
        let conn = db.conn().expect("connection");
        for i in 0..5 {
            crate::infra::storage::audit_store::AuditStore
                .append(
                    &conn,
                    &toolkit_security::AccessScope::allow_all(),
                    AuditRecord::new(
                        "k",
                        None,
                        "admin",
                        ActorTenant::machine(),
                        AuditOperation::Change,
                        format!("r{i}"),
                    ),
                )
                .await
                .expect("append");
        }
    }
    let year = std::time::Duration::from_hours(365 * 24);
    let later = time::OffsetDateTime::now_utc() + time::Duration::days(400);
    let live = tokio_util::sync::CancellationToken::new();
    let quiet = RecordingLifecycleMetrics::default();

    // Batches of two, two batches a tick: four go now, the last one next tick.
    let capped = super::PruneBatches {
        size: 2,
        per_tick: 2,
    };
    assert_eq!(
        SettingsService::prune_once(&db, year, later, capped, &live, &quiet).await,
        4
    );
    assert_eq!(
        SettingsService::prune_once(&db, year, later, capped, &live, &quiet).await,
        1
    );
    assert_eq!(
        SettingsService::prune_once(&db, year, later, capped, &live, &quiet).await,
        0
    );

    // A lifecycle being stopped prunes nothing more.
    {
        let conn = db.conn().expect("connection");
        crate::infra::storage::audit_store::AuditStore
            .append(
                &conn,
                &toolkit_security::AccessScope::allow_all(),
                AuditRecord::new(
                    "k",
                    None,
                    "admin",
                    ActorTenant::machine(),
                    AuditOperation::Change,
                    "late",
                ),
            )
            .await
            .expect("append");
    }
    let stopped = tokio_util::sync::CancellationToken::new();
    stopped.cancel();
    assert_eq!(
        SettingsService::prune_once(&db, year, later, capped, &stopped, &quiet).await,
        0
    );
}

/// Records what the lifecycle passes publish, in order.
#[derive(Default)]
struct RecordingLifecycleMetrics {
    review: std::sync::Mutex<Vec<(&'static str, u64)>>,
    retention: std::sync::Mutex<Vec<(&'static str, u64)>>,
}

impl crate::domain::ports::LifecycleMetrics for RecordingLifecycleMetrics {
    fn needs_review(&self, source: &'static str, count: u64) {
        self.review.lock().expect("lock").push((source, count));
    }

    fn retention_pass(&self, result: &'static str, pruned: u64) {
        self.retention.lock().expect("lock").push((result, pruned));
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

    let metrics = RecordingLifecycleMetrics::default();
    SettingsService::review_once(&h.db, &metrics).await;
    assert_eq!(
        *metrics.review.lock().expect("lock"),
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
                    display_name: None,
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
    let metrics = RecordingLifecycleMetrics::default();
    SettingsService::review_once(&h.db, &metrics).await;
    assert_eq!(
        *metrics.review.lock().expect("lock"),
        vec![("admin_authored", 1), ("module_contributed", 2)]
    );
}

#[tokio::test]
async fn a_retention_pass_reports_its_outcome_so_a_failure_is_not_a_quiet_zero() {
    // A pass that pruned nothing and a pass that could not run both return
    // zero; only the metric tells them apart, which is what an alert needs.
    let year = std::time::Duration::from_hours(365 * 24);
    let live = tokio_util::sync::CancellationToken::new();
    let batches = super::PruneBatches {
        size: 10,
        per_tick: 1,
    };
    let now = time::OffsetDateTime::now_utc();

    let healthy = crate::test_support::sqlite_provider().await;
    let metrics = RecordingLifecycleMetrics::default();
    assert_eq!(
        SettingsService::prune_once(&healthy, year, now, batches, &live, &metrics).await,
        0
    );

    // A database without the audit table: the delete fails.
    let broken = toolkit_db::DBProvider::new(
        toolkit_db::connect_db(
            "sqlite::memory:",
            toolkit_db::ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("an empty database"),
    );
    assert_eq!(
        SettingsService::prune_once(&broken, year, now, batches, &live, &metrics).await,
        0
    );

    assert_eq!(
        *metrics.retention.lock().expect("lock"),
        vec![("ok", 0), ("failed", 0)]
    );
}

#[tokio::test]
async fn a_failed_review_pass_publishes_nothing_so_the_gauge_keeps_its_last_values() {
    // All or nothing: a pass that cannot count every source records no source
    // at all, rather than zero for the ones it did not reach — a zero would
    // read as "the backlog was fixed".
    let broken = toolkit_db::DBProvider::new(
        toolkit_db::connect_db(
            "sqlite::memory:",
            toolkit_db::ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("an empty database"),
    );
    let metrics = RecordingLifecycleMetrics::default();
    SettingsService::review_once(&broken, &metrics).await;
    assert!(metrics.review.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_retention_pass_writes_the_configured_retention_where_the_trigger_reads_it() {
    // The database's trigger refuses deleting a record younger than the
    // greater of the platform minimum and this row, so a deployment keeping
    // records two years is guarded for two years, not one.
    let db = crate::test_support::sqlite_provider().await;
    let live = tokio_util::sync::CancellationToken::new();
    let quiet = RecordingLifecycleMetrics::default();
    let batches = super::PruneBatches {
        size: 10,
        per_tick: 1,
    };
    let now = time::OffsetDateTime::now_utc();
    let recorded = || async {
        let conn = db.conn().expect("connection");
        crate::infra::storage::audit_store::AuditStore
            .recorded_retention(&conn, &toolkit_security::AccessScope::allow_all())
            .await
            .expect("readable")
    };
    // Seeded by the migration at the minimum, so a pass only ever updates the
    // row: two replicas' first passes cannot race to insert it.
    assert_eq!(recorded().await, Some(365), "seeded before the first pass");

    let two_years = std::time::Duration::from_hours(730 * 24);
    SettingsService::prune_once(&db, two_years, now, batches, &live, &quiet).await;
    assert_eq!(recorded().await, Some(730));

    // A changed configuration is carried by the next pass.
    let year = std::time::Duration::from_hours(400 * 24);
    SettingsService::prune_once(&db, year, now, batches, &live, &quiet).await;
    assert_eq!(recorded().await, Some(400));
}

#[tokio::test]
async fn a_consumer_that_outlives_the_shutdown_bound_is_aborted() {
    // A task parked on a broker call that never answers: the stop returns once
    // the bound has passed, and the task is gone, not left running.
    struct Dropped(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = Dropped(std::sync::Arc::clone(&dropped));
    let stuck = tokio::spawn(async move {
        let _held = guard;
        std::future::pending::<()>().await;
    });
    let started = std::time::Instant::now();
    SettingsService::await_tasks(
        vec![super::ConsumerTask::new("test", stuck)],
        std::time::Duration::from_millis(50),
    )
    .await;
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    // `abort` takes effect at the task's next poll; give the runtime one.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "the task was aborted"
    );
}

#[tokio::test]
async fn a_consumer_that_ends_in_time_is_awaited_without_abort() {
    let quick = tokio::spawn(async {});
    SettingsService::await_tasks(
        vec![super::ConsumerTask::new("test", quick)],
        std::time::Duration::from_secs(5),
    )
    .await;
}

#[tokio::test]
async fn two_stalled_background_tasks_share_one_stop_deadline_and_are_both_aborted() {
    struct Dropped(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let flags: Vec<_> = (0..2)
        .map(|_| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)))
        .collect();
    let tasks: Vec<_> = flags
        .iter()
        .map(|flag| {
            let guard = Dropped(std::sync::Arc::clone(flag));
            super::ConsumerTask::new(
                "stalled",
                tokio::spawn(async move {
                    let _held = guard;
                    std::future::pending::<()>().await;
                }),
            )
        })
        .collect();
    let bound = std::time::Duration::from_millis(200);
    let started = std::time::Instant::now();
    SettingsService::await_tasks(tasks, bound).await;
    let waited = started.elapsed();
    assert!(
        waited >= bound && waited < bound * 2,
        "one shared deadline, not one each: {waited:?}"
    );
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    for flag in &flags {
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst), "aborted");
    }
}

#[tokio::test]
async fn a_task_that_ends_in_time_is_joined_while_its_stalled_neighbour_is_aborted() {
    let quick = tokio::spawn(async {});
    let stuck = tokio::spawn(std::future::pending::<()>());
    let aborted = stuck.abort_handle();
    SettingsService::await_tasks(
        vec![
            super::ConsumerTask::new("quick", quick),
            super::ConsumerTask::new("stuck", stuck),
        ],
        std::time::Duration::from_millis(50),
    )
    .await;
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(aborted.is_finished());
}

#[tokio::test]
async fn a_consumer_task_dropped_without_awaiting_is_aborted() {
    struct Dropped(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = Dropped(std::sync::Arc::clone(&dropped));
    let stuck = tokio::spawn(async move {
        let _held = guard;
        std::future::pending::<()>().await;
    });
    // Let the task start so the abort has a poll point to land on.
    tokio::task::yield_now().await;
    drop(super::ConsumerTask::new("test", stuck));
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "the task was aborted by the guard's drop"
    );
}

#[tokio::test]
async fn init_refuses_a_zero_or_oversized_lock_budget_or_redelivery_interval() {
    // A zero budget would fail every mutation that meets another; a zero
    // interval would republish without pause; a value above the ceiling — an
    // hour, half a day — is no bound at all. Init refuses each, naming the
    // field.
    for (field, value) in [
        ("mutation_lock_wait_seconds", 0),
        ("redeliver_interval_seconds", 0),
        ("mutation_lock_wait_seconds", 3_601),
        ("redeliver_interval_seconds", 43_201),
    ] {
        let ctx = context_with(json!({ field: value }), wired_hub()).await;
        let err = SettingsService::default()
            .init(&ctx)
            .await
            .expect_err("out of bounds is refused");
        let message = err.to_string();
        assert!(
            message.contains(field) && message.contains("between 1 and"),
            "the message names the field and the bounds: `{message}`"
        );
    }
    let ctx = context_with(
        json!({ "mutation_lock_wait_seconds": 1, "redeliver_interval_seconds": 1 }),
        wired_hub(),
    )
    .await;
    SettingsService::default()
        .init(&ctx)
        .await
        .expect("one second is the floor");
}

#[tokio::test]
async fn init_refuses_a_report_timeout_below_two_intervals_or_above_a_day_and_binds_k() {
    for config in [
        json!({ "activation_report_timeout_seconds": 59 }),
        json!({ "activation_report_timeout_seconds": 86_401 }),
        json!({ "redeliver_interval_seconds": 400 }),
    ] {
        let ctx = context_with(config.clone(), wired_hub()).await;
        let err = SettingsService::default()
            .init(&ctx)
            .await
            .expect_err("out of bounds is refused");
        let message = err.to_string();
        assert!(
            message.contains("activation_report_timeout_seconds"),
            "{config}: the message names the field: `{message}`"
        );
    }
    let ctx = context_with(
        json!({ "redeliver_interval_seconds": 10, "activation_report_timeout_seconds": 95 }),
        wired_hub(),
    )
    .await;
    let gear = SettingsService::default();
    gear.init(&ctx)
        .await
        .expect("two intervals or more is accepted");
    assert_eq!(
        gear.notifier().expect("bound").silent_after(),
        10,
        "K = ceil(95 / 10)"
    );
}

#[tokio::test]
async fn init_binds_the_subscription_service_and_the_write_path_to_the_activation_state() {
    let ctx = default_context().await;
    let gear = SettingsService::default();
    assert!(gear.subscriptions().is_err(), "nothing before init");
    gear.init(&ctx).await.expect("init");
    assert!(gear.subscriptions().is_ok());
}

#[tokio::test]
async fn init_removes_the_stale_lock_markers_of_the_scan_lock_and_every_declared_key_on_sqlite() {
    use crate::domain::activation::KeyLocks;

    // A migrated database already holding one declared key.
    let harness = crate::test_support::ResolutionHarness::new().await;
    harness
        .declare(
            "flag",
            crate::domain::resolution::scope_class::CASCADING,
            json!(false),
        )
        .await;
    let key = harness.key("flag");
    // A crash left the markers behind: the holders are gone.
    let probe = crate::infra::mutation_lock::MutationLock::new(
        std::sync::Arc::clone(&harness.db),
        std::time::Duration::from_millis(30),
        std::sync::Arc::new(crate::domain::activation::testing::RecordingLockMetrics::default()),
    );
    for stale in [key.as_str(), crate::infra::mutation_lock::SCAN_LOCK_KEY] {
        std::mem::forget(probe.acquire(stale, None).await.expect("held, then lost"));
    }
    assert!(
        probe.acquire(key.as_str(), None).await.is_err(),
        "the marker blocks"
    );

    let ctx = GearCtx::new(
        SettingsService::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(json!({ "config": {} }))),
        wired_hub(),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db((*harness.db).clone());
    SettingsService::default().init(&ctx).await.expect("init");

    for key in [key.as_str(), crate::infra::mutation_lock::SCAN_LOCK_KEY] {
        let held = probe
            .acquire(key, None)
            .await
            .unwrap_or_else(|e| panic!("the marker of `{key}` was removed at init: {e:?}"));
        held.release().await;
    }
}

#[tokio::test]
async fn init_binds_the_notifier_and_the_recorder_and_refuses_them_before() {
    let ctx = default_context().await;
    let gear = SettingsService::default();
    assert!(gear.recorder().is_err(), "nothing before init");
    assert!(gear.notifier().is_err());
    gear.init(&ctx).await.expect("init");
    assert!(gear.recorder().is_ok());
    assert!(gear.notifier().is_ok());
    assert!(gear.notify_metrics().is_ok());
    // The delivery loop's interval is the configured `redeliver_interval_seconds`.
    assert_eq!(
        gear.notifier().expect("bound").interval(),
        std::time::Duration::from_secs(30)
    );
}

#[tokio::test]
async fn the_managed_lifecycle_runs_the_delivery_loop_beside_the_consumer_and_stops_it() {
    let inner = crate::test_support::ResolutionHarness::new().await;
    let secrets = std::sync::Arc::new(crate::test_support::RecordingSecrets::default());
    let gear = gear_with_writes(&inner, &secrets);
    let (buf, _guard) = crate::domain::activation::testing::capture();

    let cancel = tokio_util::sync::CancellationToken::new();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(SettingsService::serve(
        std::sync::Arc::clone(&gear),
        cancel.clone(),
        toolkit::lifecycle::ReadySignal::from_sender(tx),
    ));
    rx.await.expect("readiness is announced");
    cancel.cancel();
    serving
        .await
        .expect("the task joins")
        .expect("and returns cleanly");

    let log = crate::domain::activation::testing::logged(&buf);
    assert!(log.contains("activation delivery loop started"), "{log}");
    assert!(log.contains("activation delivery loop stopped"), "{log}");
    // ... and the cache invalidation consumer beside it.
    assert!(log.contains("cache invalidation consumer started"), "{log}");
    assert!(log.contains("cache invalidation consumer stopped"), "{log}");
}

#[tokio::test]
async fn an_uninitialized_gear_refuses_to_start_the_delivery_loop() {
    let gear = SettingsService::default();
    let refusal = gear
        .spawn_delivery(&tokio_util::sync::CancellationToken::new())
        .err()
        .expect("nothing to deliver with");
    assert!(refusal.to_string().contains("not initialized"));
}

// ---------------------------------------------------------------------------
// The end-of-request hook, as init wires it
// ---------------------------------------------------------------------------

/// A broker double that accepts and records every published event; the rest of
/// the broker API is refused.
mod recording_broker {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use event_broker_sdk::models::EventType;
    use event_broker_sdk::{
        Ack, ConsumerGroup, ConsumerGroupId, ConsumerGroupQuery, CreateConsumerGroupRequest,
        EventBrokerApi, EventBrokerError, EventV1, FrameStream, IngestOutcome, JoinRequest, Page,
        PartitionRange, ProducerCursors, ProducerId, ProducerMode, ResetScope, SeekPosition,
        SeekResult, Subscription, SubscriptionAssignment, SubscriptionId, Topic, TopicSegment,
    };
    use toolkit_security::SecurityContext;

    #[derive(Default)]
    pub struct RecordingBroker {
        pub published: Mutex<Vec<EventV1>>,
    }

    fn refused() -> EventBrokerError {
        EventBrokerError::Internal("not supported by the recording broker".to_owned())
    }

    #[async_trait]
    impl EventBrokerApi for RecordingBroker {
        async fn register_producer(
            &self,
            _: &SecurityContext,
            _: ProducerMode,
            _: &str,
        ) -> Result<ProducerId, EventBrokerError> {
            Err(refused())
        }
        async fn publish(
            &self,
            _ctx: &SecurityContext,
            event: &EventV1,
            _: Ack,
        ) -> Result<IngestOutcome, EventBrokerError> {
            self.published.lock().unwrap().push(event.clone());
            Ok(IngestOutcome::Accepted)
        }
        async fn publish_batch(
            &self,
            _: &SecurityContext,
            _: &[EventV1],
            _: Ack,
        ) -> Result<IngestOutcome, EventBrokerError> {
            Err(refused())
        }
        async fn get_producer_cursors(
            &self,
            _: &SecurityContext,
            _: ProducerId,
        ) -> Result<ProducerCursors, EventBrokerError> {
            Err(refused())
        }
        async fn reset_producer_chain(
            &self,
            _: &SecurityContext,
            _: ProducerId,
            _: ResetScope<'_>,
        ) -> Result<(), EventBrokerError> {
            Err(refused())
        }
        async fn create_consumer_group(
            &self,
            _: &SecurityContext,
            _: CreateConsumerGroupRequest,
        ) -> Result<ConsumerGroup, EventBrokerError> {
            Err(refused())
        }
        async fn get_consumer_group(
            &self,
            _: &SecurityContext,
            _: &ConsumerGroupId,
        ) -> Result<ConsumerGroup, EventBrokerError> {
            Err(refused())
        }
        async fn list_consumer_groups(
            &self,
            _: &SecurityContext,
            _: ConsumerGroupQuery,
        ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
            Err(refused())
        }
        async fn delete_consumer_group(
            &self,
            _: &SecurityContext,
            _: &ConsumerGroupId,
        ) -> Result<(), EventBrokerError> {
            Err(refused())
        }
        async fn join(
            &self,
            _: &SecurityContext,
            _: JoinRequest,
        ) -> Result<SubscriptionAssignment, EventBrokerError> {
            Err(refused())
        }
        async fn get_subscription(
            &self,
            _: &SecurityContext,
            _: SubscriptionId,
        ) -> Result<Subscription, EventBrokerError> {
            Err(refused())
        }
        async fn list_subscriptions(
            &self,
            _: &SecurityContext,
        ) -> Result<Vec<Subscription>, EventBrokerError> {
            Err(refused())
        }
        async fn leave(
            &self,
            _: &SecurityContext,
            _: SubscriptionId,
        ) -> Result<(), EventBrokerError> {
            Err(refused())
        }
        async fn stream(
            &self,
            _: &SecurityContext,
            _: SubscriptionId,
        ) -> Result<FrameStream, EventBrokerError> {
            Err(refused())
        }
        async fn seek(
            &self,
            _: &SecurityContext,
            _: SubscriptionId,
            _: i64,
            _: &[SeekPosition],
        ) -> Result<Vec<SeekResult>, EventBrokerError> {
            Err(refused())
        }
        async fn list_topics(&self, _: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
            Err(refused())
        }
        async fn list_topic_segments(
            &self,
            _: &SecurityContext,
            _: &str,
            _: u32,
            _: PartitionRange,
        ) -> Result<TopicSegment, EventBrokerError> {
            Err(refused())
        }
        async fn list_event_types(
            &self,
            _: &SecurityContext,
        ) -> Result<Vec<EventType>, EventBrokerError> {
            Err(refused())
        }
        async fn get_event_type(
            &self,
            _: &SecurityContext,
            _: &str,
        ) -> Result<EventType, EventBrokerError> {
            Err(refused())
        }
    }
}

/// A tenant resolver that names one root and answers nothing else.
struct RootOnly(uuid::Uuid);

#[async_trait::async_trait]
impl tenant_resolver_sdk::TenantResolverClient for RootOnly {
    async fn get_tenant(
        &self,
        _: &toolkit_security::SecurityContext,
        _: tenant_resolver_sdk::TenantId,
    ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError> {
        unreachable!("not exercised")
    }

    async fn get_root_tenant(
        &self,
        _: &toolkit_security::SecurityContext,
    ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError> {
        Ok(tenant_resolver_sdk::TenantInfo {
            id: tenant_resolver_sdk::TenantId(self.0),
            name: "root".to_owned(),
            status: tenant_resolver_sdk::TenantStatus::Active,
            tenant_type: None,
            parent_id: None,
            self_managed: false,
        })
    }

    async fn get_tenants(
        &self,
        _: &toolkit_security::SecurityContext,
        _: &[tenant_resolver_sdk::TenantId],
        _: &tenant_resolver_sdk::GetTenantsOptions,
    ) -> Result<Vec<tenant_resolver_sdk::TenantInfo>, tenant_resolver_sdk::TenantResolverError>
    {
        unreachable!("not exercised")
    }

    async fn get_ancestors(
        &self,
        _: &toolkit_security::SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::GetAncestorsOptions,
    ) -> Result<tenant_resolver_sdk::GetAncestorsResponse, tenant_resolver_sdk::TenantResolverError>
    {
        unreachable!("not exercised")
    }

    async fn get_descendants(
        &self,
        _: &toolkit_security::SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::GetDescendantsOptions,
    ) -> Result<tenant_resolver_sdk::GetDescendantsResponse, tenant_resolver_sdk::TenantResolverError>
    {
        unreachable!("not exercised")
    }

    async fn is_ancestor(
        &self,
        _: &toolkit_security::SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::IsAncestorOptions,
    ) -> Result<bool, tenant_resolver_sdk::TenantResolverError> {
        unreachable!("not exercised")
    }
}

/// The `cache_invalidate` events a recording broker holds, as the key lists
/// they name, under the tenants they were published for.
fn cache_invalidations(
    broker: &recording_broker::RecordingBroker,
) -> Vec<(uuid::Uuid, Vec<String>)> {
    broker
        .published
        .lock()
        .expect("published")
        .iter()
        .filter(|e| *e.event_type() == settings_service_sdk::gts::CACHE_INVALIDATE_EVENT_TYPE)
        .map(|e| {
            let body: crate::infra::event_schemas::CacheInvalidation =
                serde_json::from_value(e.data().cloned().expect("a body")).expect("a body");
            (e.tenant_id(), body.changed_keys)
        })
        .collect()
}

/// The test registry with registration accepted: the mock refuses to register
/// anything, and the broker client registers its topic and type once resolved.
struct AcceptingRegistry(types_registry_sdk::testing::MockTypesRegistryClient);

fn accepted(count: usize) -> Vec<types_registry_sdk::RegisterResult> {
    (0..count)
        .map(|_| types_registry_sdk::RegisterResult::Ok {
            gts_id: "gts.x".to_owned(),
        })
        .collect()
}

#[async_trait::async_trait]
impl types_registry_sdk::TypesRegistryClient for AcceptingRegistry {
    async fn register(
        &self,
        entities: Vec<Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(accepted(entities.len()))
    }

    async fn register_type_schemas(
        &self,
        type_schemas: Vec<Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(accepted(type_schemas.len()))
    }

    async fn register_instances(
        &self,
        instances: Vec<Value>,
    ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
    {
        Ok(accepted(instances.len()))
    }

    async fn get_type_schema(
        &self,
        type_id: &str,
    ) -> Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError> {
        self.0.get_type_schema(type_id).await
    }

    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError> {
        self.0.get_type_schema_by_uuid(type_uuid).await
    }

    async fn get_type_schemas(
        &self,
        type_ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>,
    > {
        self.0.get_type_schemas(type_ids).await
    }

    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> std::collections::HashMap<
        Uuid,
        Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>,
    > {
        self.0.get_type_schemas_by_uuid(type_uuids).await
    }

    async fn list_type_schemas(
        &self,
        query: types_registry_sdk::TypeSchemaQuery,
    ) -> Result<Vec<types_registry_sdk::GtsTypeSchema>, toolkit_canonical_errors::CanonicalError>
    {
        self.0.list_type_schemas(query).await
    }

    async fn get_instance(
        &self,
        id: &str,
    ) -> Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError> {
        self.0.get_instance(id).await
    }

    async fn get_instance_by_uuid(
        &self,
        uuid: Uuid,
    ) -> Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError> {
        self.0.get_instance_by_uuid(uuid).await
    }

    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> std::collections::HashMap<
        String,
        Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>,
    > {
        self.0.get_instances(ids).await
    }

    async fn get_instances_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> std::collections::HashMap<
        Uuid,
        Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>,
    > {
        self.0.get_instances_by_uuid(uuids).await
    }

    async fn list_instances(
        &self,
        query: types_registry_sdk::InstanceQuery,
    ) -> Result<Vec<types_registry_sdk::GtsInstance>, toolkit_canonical_errors::CanonicalError>
    {
        self.0.list_instances(query).await
    }
}

#[tokio::test]
async fn init_wires_the_end_of_request_hook_into_the_declaration_admin_and_the_contribution_client()
{
    use settings_service_sdk::api::SettingsContributionClient;

    // Two contributed declarations already stored, owned by the harness's module.
    let harness = crate::test_support::ResolutionHarness::new().await;
    for name in ["one", "two"] {
        harness
            .declare(
                name,
                crate::domain::resolution::scope_class::CASCADING,
                json!(false),
            )
            .await;
    }
    let root = Uuid::from_u128(0xa0);
    let broker = Arc::new(recording_broker::RecordingBroker::default());
    let hub = wired_hub();
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(AcceptingRegistry(
        types_registry_sdk::testing::MockTypesRegistryClient::new(),
    )));
    hub.register::<dyn event_broker_sdk::EventBrokerApi>(
        Arc::clone(&broker) as Arc<dyn event_broker_sdk::EventBrokerApi>
    );
    hub.register::<dyn tenant_resolver_sdk::TenantResolverClient>(Arc::new(RootOnly(root)));
    let ctx = GearCtx::new(
        SettingsService::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(json!({ "config": {} }))),
        Arc::clone(&hub),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db((*harness.db).clone());
    let gear = SettingsService::default();
    gear.init(&ctx).await.expect("init");
    // What `serve` does beside the consumer: learn the root and register the
    // cache topic and type, so the request path may publish.
    assert!(
        gear.event_broker()
            .expect("bound")
            .resolve_once(&tokio_util::sync::CancellationToken::new())
            .await,
        "the broker is resolved"
    );
    assert!(cache_invalidations(&broker).is_empty(), "nothing yet");

    // A contribution reconcile through the client init put in the hub.
    let client = hub
        .get::<dyn SettingsContributionClient>()
        .expect("the contribution client");
    let retired = client
        .retire_declarations(
            &toolkit_security::SecurityContext::anonymous(),
            "test".to_owned(),
            vec![harness.key("one")],
        )
        .await
        .expect("retired");
    assert_eq!(retired.retired, 1);
    assert_eq!(
        cache_invalidations(&broker),
        vec![(root, vec![harness.key("one").to_string()])],
        "one cache_invalidate under the root tenant for the reconcile"
    );

    // A declaration retire's end-of-request step, on the admin init built.
    let admin = gear.declaration_admin.get().expect("the declaration admin");
    admin
        .after_retire(harness.key("two").as_str(), true, "req-1")
        .await;
    assert_eq!(
        cache_invalidations(&broker),
        vec![
            (root, vec![harness.key("one").to_string()]),
            (root, vec![harness.key("two").to_string()]),
        ],
        "and one for the declaration retire, under the root tenant"
    );
}
