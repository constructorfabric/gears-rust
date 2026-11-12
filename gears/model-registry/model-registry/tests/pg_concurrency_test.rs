//! Concurrent writers on a real `PostgreSQL` (DESIGN §3.6, ADR-0004). SQLite
//! serializes writers, so these guarantees are covered here only. Each test
//! races two service calls over several rounds and asserts what must hold in
//! every interleaving; the interleaving a race rarely produces is forced
//! through a gate in the schema double. Docker required; run with
//! `make test-integration LANE=model-registry-pg`.

#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::large_futures,
    clippy::many_single_char_names
)]

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use authz_resolver_sdk::PolicyEnforcer;
use model_registry::config::ModelRegistryConfig;
use model_registry::domain::access_guard::AccessGuard;
use model_registry::domain::clock::ManualClock;
use model_registry::domain::error::{DomainError, Resource};
use model_registry::domain::gts_validator::SchemaSource;
use model_registry::domain::service::Service;
use model_registry::infra::chain_cache::ChainCache;
use model_registry::testing::{
    StaticAuthz, StaticSchemaSource, StaticSecretRefSource, StaticTenantResolver, model_update,
    provider_update, security_context,
};
use model_registry_sdk::{
    ApprovalState, ContextWindow, CreateModelRequestV1, CreateProviderRequestV1, ModelV1,
    ProviderV1,
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use toolkit_db::DBProvider;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use types_registry_sdk::TypeSchema;
use uuid::Uuid;

mod common;
use common::{PgServer, connect, migrate, pg_server};

const OPENAI: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~");
const STRICT: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.strict.v1~");

/// Races per test; each round uses rows of its own.
const ROUNDS: usize = 20;

/// The schema double with a gate: once armed, the next lookup of the OPENAI
/// type signals `arrived` and waits for `release`. A model write looks its
/// provider type up between reading the provider and its transaction, so the
/// gate holds the write exactly there.
struct GatedSchemas {
    inner: StaticSchemaSource,
    armed: AtomicBool,
    arrived: Semaphore,
    release: Semaphore,
}

#[async_trait::async_trait]
impl SchemaSource for GatedSchemas {
    async fn type_schema(
        &self,
        ctx: &SecurityContext,
        type_id: &gts::GtsTypeId,
    ) -> Result<Option<TypeSchema>, DomainError> {
        if type_id.as_ref() == OPENAI && self.armed.swap(false, Ordering::SeqCst) {
            self.arrived.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        self.inner.type_schema(ctx, type_id).await
    }
}

struct Harness {
    service: Service,
    schemas: Arc<GatedSchemas>,
    ctx: SecurityContext,
    _server: PgServer,
}

async fn harness() -> Option<Harness> {
    let server = pg_server("concurrency").await?;
    let db = connect(&server.url, 8).await;
    migrate(&db).await;
    let tenant = Uuid::new_v4();
    let cfg = ModelRegistryConfig::default();
    let clock = Arc::new(ManualClock::fixed());
    let chain_cache = Arc::new(ChainCache::new(
        Arc::new(StaticTenantResolver::new().with_chain(tenant, vec![])),
        clock.clone(),
        cfg.chain_cache_ttl(),
        cfg.chain_cache_max_entries,
        cfg.tenant_resolver_deadline(),
    ));
    let guard = AccessGuard::new(
        PolicyEnforcer::new(Arc::new(StaticAuthz::permit_own_tenant()))
            .with_deadline(cfg.pdp_deadline()),
        cfg.max_pdp_constraint_values,
    );
    // OPENAI accepts any settings; STRICT requires `oagw_alias` and nothing else.
    let schemas = Arc::new(GatedSchemas {
        inner: StaticSchemaSource::new().with_open_type(OPENAI).with_type(
            STRICT,
            &json!({
                "type": "object",
                "required": ["oagw_alias"],
                "properties": { "oagw_alias": { "type": "string" } },
                "additionalProperties": false
            }),
        ),
        armed: AtomicBool::new(false),
        arrived: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    let service = Service::new(
        Arc::new(DBProvider::<DomainError>::new(db)),
        chain_cache,
        guard,
        schemas.clone(),
        Arc::new(StaticSecretRefSource::new()),
        clock,
        &cfg,
    );
    Some(Harness {
        service,
        schemas,
        ctx: security_context(tenant),
        _server: server,
    })
}

fn strict_settings() -> Value {
    json!({ "oagw_alias": "a" })
}

fn provider_req(slug: &str) -> CreateProviderRequestV1 {
    CreateProviderRequestV1::builder(slug, slug, gts::GtsTypeId::new(OPENAI)).build()
}

fn model_req(provider: Uuid, name: &str, settings: Value) -> CreateModelRequestV1 {
    let mut req = CreateModelRequestV1::new(provider, name, name, name, ContextWindow::new(8));
    req.provider_settings = settings;
    req
}

impl Harness {
    async fn provider(&self, slug: &str) -> Uuid {
        self.service
            .create_provider(&self.ctx, provider_req(slug))
            .await
            .unwrap()
            .provider
            .id
    }

    /// A model whose settings satisfy both provider types.
    async fn model(&self, provider: Uuid, name: &str) -> Uuid {
        self.service
            .create_model(&self.ctx, model_req(provider, name, strict_settings()))
            .await
            .unwrap()
            .model
            .id
    }

    async fn get_provider(&self, id: Uuid) -> ProviderV1 {
        self.service
            .get_provider(&self.ctx, id)
            .await
            .unwrap()
            .provider
    }

    async fn get_model(&self, id: Uuid) -> ModelV1 {
        self.service.get_model(&self.ctx, id).await.unwrap().model
    }

    /// Runs `write` up to its provider type lookup, migrates `provider` to
    /// STRICT, then lets `write` finish; returns its result.
    async fn across_a_type_migration<T>(
        &self,
        provider: Uuid,
        write: impl Future<Output = Result<T, DomainError>>,
    ) -> Result<T, DomainError> {
        self.schemas.armed.store(true, Ordering::SeqCst);
        let migrate = async {
            self.schemas.arrived.acquire().await.unwrap().forget();
            let migrated = self
                .service
                .update_provider(
                    &self.ctx,
                    provider,
                    provider_update(|r| r.gts_type = Some(gts::GtsTypeId::new(STRICT))),
                )
                .await;
            self.schemas.release.add_permits(1);
            migrated
        };
        let (written, migrated) = tokio::join!(write, migrate);
        assert_eq!(migrated.unwrap().provider.gts_type.as_ref(), STRICT);
        written
    }
}

fn is_stale(err: &DomainError) -> bool {
    matches!(err, DomainError::OptimisticLockFailure { .. })
}

fn is_invalid(err: &DomainError) -> bool {
    matches!(err, DomainError::Invalid { .. })
}

/// One of two writers with the same `expected_version` wins; the other fails
/// with `OPTIMISTIC_LOCK_FAILURE` on `resource`.
fn the_winner<T: Debug>(
    a: Result<T, DomainError>,
    b: Result<T, DomainError>,
    resource: Resource,
) -> T {
    match (a, b) {
        (Ok(won), Err(lost)) | (Err(lost), Ok(won)) => {
            assert!(
                matches!(lost, DomainError::OptimisticLockFailure { resource: r, .. } if r == resource),
                "{lost:?}"
            );
            won
        }
        other => panic!("exactly one writer must win: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn of_two_writers_with_the_same_version_one_wins() {
    let Some(h) = harness().await else {
        return;
    };
    for round in 0..ROUNDS {
        let p = h.provider(&format!("p{round}")).await;
        let m = h.model(p, "m").await;

        let rename = |name: &'static str| {
            h.service.update_provider(
                &h.ctx,
                p,
                provider_update(|r| {
                    r.expected_version = Some(1);
                    r.name = Some(name.into());
                }),
            )
        };
        let (a, b) = tokio::join!(rename("a"), rename("b"));
        let won = the_winner(a, b, Resource::Provider).provider;
        let stored = h.get_provider(p).await;
        assert_eq!(stored.version, 2);
        assert_eq!(stored.name, won.name);

        let rename = |name: &'static str| {
            h.service.update_model(
                &h.ctx,
                m,
                model_update(|r| {
                    r.expected_version = Some(1);
                    r.display_name = Some(name.into());
                }),
            )
        };
        let (a, b) = tokio::join!(rename("a"), rename("b"));
        let won = the_winner(a, b, Resource::Model).model;
        let stored = h.get_model(m).await;
        assert_eq!(stored.version, 2);
        assert_eq!(stored.info.display_name, won.info.display_name);
    }
}

/// Unconditional updates of different fields both apply: neither writes back
/// the value it read for the field the other changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_updates_of_different_fields_keep_both() {
    let Some(h) = harness().await else {
        return;
    };
    for round in 0..ROUNDS {
        let p = h.provider(&format!("p{round}")).await;
        let m = h.model(p, "m").await;

        let (revoke, rename) = tokio::join!(
            h.service.update_model(
                &h.ctx,
                m,
                model_update(|r| r.approval_state = Some(ApprovalState::Revoked)),
            ),
            h.service.update_model(
                &h.ctx,
                m,
                model_update(|r| r.display_name = Some("renamed".into())),
            ),
        );
        revoke.unwrap();
        rename.unwrap();
        let stored = h.get_model(m).await;
        assert_eq!(stored.approval.state, ApprovalState::Revoked);
        assert_eq!(stored.info.display_name, "renamed");
        assert_eq!(stored.version, 3);

        let (disable, rename) = tokio::join!(
            h.service
                .update_provider(&h.ctx, p, provider_update(|r| r.enabled = Some(false))),
            h.service.update_provider(
                &h.ctx,
                p,
                provider_update(|r| r.name = Some("renamed".into())),
            ),
        );
        disable.unwrap();
        rename.unwrap();
        let stored = h.get_provider(p).await;
        assert!(!stored.enabled);
        assert_eq!(stored.name, "renamed");
        assert_eq!(stored.version, 3);
    }
}

/// A settings update the new type rejects races the type migration: one of
/// them fails, and a migrated provider never holds settings its type rejects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settings_update_and_a_type_migration_do_not_both_apply() {
    let Some(h) = harness().await else {
        return;
    };
    for round in 0..ROUNDS {
        let p = h.provider(&format!("p{round}")).await;
        let m = h.model(p, "m").await;
        let conditional = round % 2 == 0;

        let (update, migration) = tokio::join!(
            h.service.update_model(
                &h.ctx,
                m,
                model_update(|r| {
                    r.expected_version = conditional.then_some(1);
                    r.provider_settings = Some(json!({ "endpoint": "eu" }));
                }),
            ),
            h.service.update_provider(
                &h.ctx,
                p,
                provider_update(|r| r.gts_type = Some(gts::GtsTypeId::new(STRICT))),
            ),
        );
        let provider = h.get_provider(p).await;
        let model = h.get_model(m).await;
        assert_eq!(model.info.gts_type, provider.gts_type);
        match (update, migration) {
            (Ok(_), Err(err)) => {
                assert!(is_stale(&err) || is_invalid(&err), "{err:?}");
                assert_eq!(provider.gts_type.as_ref(), OPENAI);
                assert_eq!(model.info.provider_settings, json!({ "endpoint": "eu" }));
            }
            (Err(err), Ok(_)) => {
                // Second to commit: stale when conditional, PROVIDER_TYPE_CHANGED
                // otherwise; started after the migration: rejected by the schema.
                let second = if conditional {
                    is_stale(&err)
                } else {
                    matches!(err, DomainError::ProviderTypeChanged { .. })
                };
                assert!(second || is_invalid(&err), "{err:?}");
                assert_eq!(provider.gts_type.as_ref(), STRICT);
                assert_eq!(model.info.provider_settings, strict_settings());
            }
            other => panic!("exactly one of the two must apply: {other:?}"),
        }
    }
}

/// A model the new type rejects is created while the provider migrates: one
/// of them fails, and no model is left under a type that rejects its settings.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_create_and_a_type_migration_do_not_both_apply() {
    let Some(h) = harness().await else {
        return;
    };
    for round in 0..ROUNDS {
        let p = h.provider(&format!("p{round}")).await;
        let m = h.model(p, "m").await;

        let (create, migration) = tokio::join!(
            h.service
                .create_model(&h.ctx, model_req(p, "late", json!({}))),
            h.service.update_provider(
                &h.ctx,
                p,
                provider_update(|r| r.gts_type = Some(gts::GtsTypeId::new(STRICT))),
            ),
        );
        let provider = h.get_provider(p).await;
        assert_eq!(h.get_model(m).await.info.gts_type, provider.gts_type);
        match (create, migration) {
            (Ok(created), Err(err)) => {
                assert!(is_stale(&err) || is_invalid(&err), "{err:?}");
                assert_eq!(provider.gts_type.as_ref(), OPENAI);
                let late = h.get_model(created.model.id).await;
                assert_eq!(late.info.gts_type.as_ref(), OPENAI);
            }
            (Err(err), Ok(_)) => {
                assert!(
                    matches!(err, DomainError::ProviderTypeChanged { .. }) || is_invalid(&err),
                    "{err:?}"
                );
                assert_eq!(provider.gts_type.as_ref(), STRICT);
                // The name is free: the failed create left no row.
                h.model(p, "late").await;
            }
            other => panic!("exactly one of the two must apply: {other:?}"),
        }
    }
}

/// A model write validated against the old type commits after the migration
/// (ADR-0004): a create and an unconditional update fail with
/// `PROVIDER_TYPE_CHANGED`, a conditional update with
/// `OPTIMISTIC_LOCK_FAILURE`, and none of them leaves a trace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_write_validated_before_a_type_migration_fails_after_it() {
    let Some(h) = harness().await else {
        return;
    };

    let p = h.provider("create").await;
    let err = h
        .across_a_type_migration(
            p,
            h.service
                .create_model(&h.ctx, model_req(p, "late", json!({}))),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::ProviderTypeChanged { provider_id } if provider_id == p),
        "{err:?}"
    );
    // The name is free: the failed create left no row.
    h.model(p, "late").await;

    for (slug, expected_version) in [("unconditional", None), ("conditional", Some(1))] {
        let p = h.provider(slug).await;
        let m = h.model(p, "m").await;
        let err = h
            .across_a_type_migration(
                p,
                h.service.update_model(
                    &h.ctx,
                    m,
                    model_update(|r| {
                        r.expected_version = expected_version;
                        r.provider_settings = Some(json!({ "endpoint": "eu" }));
                    }),
                ),
            )
            .await
            .unwrap_err();
        if expected_version.is_some() {
            assert!(
                matches!(
                    err,
                    DomainError::OptimisticLockFailure {
                        resource: Resource::Model,
                        ..
                    }
                ),
                "{err:?}"
            );
        } else {
            assert!(
                matches!(err, DomainError::ProviderTypeChanged { provider_id } if provider_id == p),
                "{err:?}"
            );
        }
        let stored = h.get_model(m).await;
        assert_eq!(stored.info.gts_type.as_ref(), STRICT);
        assert_eq!(stored.info.provider_settings, strict_settings());
        assert_eq!(stored.version, 2, "only the migration touched the model");
    }
}

/// An unconditional update that resends the type it read races a migration:
/// the loser is stale, and the provider and its models end on one type.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resending_the_old_type_does_not_split_a_migration() {
    let Some(h) = harness().await else {
        return;
    };
    for round in 0..ROUNDS {
        let p = h.provider(&format!("p{round}")).await;
        let m = h.model(p, "m").await;

        let (resend, migration) = tokio::join!(
            h.service.update_provider(
                &h.ctx,
                p,
                provider_update(|r| {
                    r.gts_type = Some(gts::GtsTypeId::new(OPENAI));
                    r.managed = Some(true);
                }),
            ),
            h.service.update_provider(
                &h.ctx,
                p,
                provider_update(|r| r.gts_type = Some(gts::GtsTypeId::new(STRICT))),
            ),
        );
        let provider = h.get_provider(p).await;
        assert_eq!(h.get_model(m).await.info.gts_type, provider.gts_type);
        match (resend, migration) {
            (Err(err), Ok(_)) => {
                assert!(is_stale(&err), "{err:?}");
                assert_eq!(provider.gts_type.as_ref(), STRICT);
                assert!(!provider.managed);
            }
            (Ok(_), Err(err)) => {
                assert!(is_stale(&err), "{err:?}");
                assert_eq!(provider.gts_type.as_ref(), OPENAI);
                assert!(provider.managed);
            }
            // One ran wholly after the other.
            (Ok(_), Ok(_)) => assert!(provider.managed),
            other => panic!("at least one of the two must apply: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn of_two_creates_with_the_same_key_one_wins() {
    let Some(h) = harness().await else {
        return;
    };
    for round in 0..ROUNDS {
        let slug = format!("p{round}");
        let (a, b) = tokio::join!(
            h.service.create_provider(&h.ctx, provider_req(&slug)),
            h.service.create_provider(&h.ctx, provider_req(&slug)),
        );
        let p = match (a, b) {
            (Ok(won), Err(lost)) | (Err(lost), Ok(won)) => {
                assert!(
                    matches!(lost, DomainError::ProviderAlreadyExists { .. }),
                    "{lost:?}"
                );
                won.provider.id
            }
            other => panic!("exactly one create must win: {other:?}"),
        };

        let (a, b) = tokio::join!(
            h.service.create_model(&h.ctx, model_req(p, "m", json!({}))),
            h.service.create_model(&h.ctx, model_req(p, "m", json!({}))),
        );
        match (a, b) {
            (Ok(_), Err(lost)) | (Err(lost), Ok(_)) => assert!(
                matches!(lost, DomainError::ModelAlreadyExists { .. }),
                "{lost:?}"
            ),
            other => panic!("exactly one create must win: {other:?}"),
        }
    }
}

/// A model created while its provider is deleted either goes with the
/// provider or is refused; it never outlives it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_does_not_outlive_a_concurrently_deleted_provider() {
    let Some(h) = harness().await else {
        return;
    };
    for round in 0..ROUNDS {
        let p = h.provider(&format!("p{round}")).await;
        let (create, delete) = tokio::join!(
            h.service.create_model(&h.ctx, model_req(p, "m", json!({}))),
            h.service.delete_provider(&h.ctx, p, None),
        );
        delete.unwrap();
        assert!(matches!(
            h.service.get_provider(&h.ctx, p).await,
            Err(DomainError::ProviderNotFound { .. })
        ));
        match create {
            Ok(created) => assert!(matches!(
                h.service.get_model(&h.ctx, created.model.id).await,
                Err(DomainError::ModelNotFound { .. })
            )),
            Err(err) => assert!(
                matches!(err, DomainError::ProviderNotFound { .. }),
                "{err:?}"
            ),
        }
    }
}
