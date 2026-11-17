//! Service-level integration tests on in-memory SQLite with the production
//! migration and the dependency doubles of `model_registry::testing`
//! (DESIGN §4 Testability, PRD §9 acceptance criteria).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::cognitive_complexity
)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::constraints::{
    Constraint, EqPredicate, InPredicate, InTenantSubtreePredicate, Predicate,
};
use model_registry::config::ModelRegistryConfig;
use model_registry::domain::access_guard::AccessGuard;
use model_registry::domain::clock::ManualClock;
use model_registry::domain::credential_check::OwnRowState;
use model_registry::domain::error::{Dependency, DomainError, Resource};
use model_registry::domain::service::Service;
use model_registry::infra::chain_cache::ChainCache;
use model_registry::infra::storage::migrations::Migrator;
use model_registry::testing::{
    StaticAuthz, StaticSchemaSource, StaticSecretRefSource, StaticTenantResolver, deny,
    model_update, own_tenant_constraint, permit_with, provider_update, security_context,
    security_context_for, subject_tenant,
};
use model_registry_sdk::odata::{
    MODEL_APPROVAL_STATE, MODEL_CANONICAL_ID, MODEL_MODEL_VERSION, MODEL_NAME, MODEL_SORT_ORDER,
    MODEL_VENDOR, ModelSchema, QueryBuilder, SortDir,
};
use model_registry_sdk::{
    ApprovalState, ContextWindow, CreateModelRequestV1, CreateProviderRequestV1, ModelCostV1,
    ModelEntryV1, ModelV1, SharingMode, field,
};
use sea_orm_migration::MigratorTrait;
use tenant_resolver_sdk::BarrierMode;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_gts::gts_id;
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use uuid::Uuid;

const OPENAI: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~");
const STRICT: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.strict.v1~");

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    service: Service,
    authz: Arc<StaticAuthz>,
    resolver: Arc<StaticTenantResolver>,
    schemas: Arc<StaticSchemaSource>,
    secrets: Arc<StaticSecretRefSource>,
    clock: Arc<ManualClock>,
}

fn schemas() -> StaticSchemaSource {
    StaticSchemaSource::new().with_open_type(OPENAI).with_type(
        STRICT,
        &serde_json::json!({
            "type": "object",
            "required": ["oagw_alias"],
            "properties": {
                "oagw_alias": { "type": "string" },
                "endpoint": { "type": "object", "properties": { "port": { "type": "number" } } }
            },
            "additionalProperties": false
        }),
    )
}

/// The default `CredStore` double: `openai-key` is an own, active secret.
fn secrets() -> StaticSecretRefSource {
    StaticSecretRefSource::new().owned("openai-key")
}

async fn harness_with(
    resolver: StaticTenantResolver,
    authz: StaticAuthz,
    schemas: StaticSchemaSource,
    cfg: ModelRegistryConfig,
) -> Harness {
    harness_full(resolver, authz, schemas, secrets(), cfg).await
}

async fn harness_full(
    resolver: StaticTenantResolver,
    authz: StaticAuthz,
    schemas: StaticSchemaSource,
    secrets: StaticSecretRefSource,
    cfg: ModelRegistryConfig,
) -> Harness {
    let raw = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("in-memory SQLite");
    run_migrations_for_testing(&raw, Migrator::migrations())
        .await
        .expect("migrations");
    let db = Arc::new(DBProvider::<DomainError>::new(raw));
    let authz = Arc::new(authz);
    let resolver = Arc::new(resolver);
    let schemas = Arc::new(schemas);
    let secrets = Arc::new(secrets);
    let clock = Arc::new(ManualClock::fixed());
    let chain_cache = Arc::new(ChainCache::new(
        resolver.clone(),
        clock.clone(),
        cfg.chain_cache_ttl(),
        cfg.chain_cache_max_entries,
        cfg.tenant_resolver_deadline(),
    ));
    let guard = AccessGuard::new(
        PolicyEnforcer::new(authz.clone()).with_deadline(cfg.pdp_deadline()),
        cfg.max_pdp_constraint_values,
    );
    let service = Service::new(
        db,
        chain_cache,
        guard,
        schemas.clone(),
        secrets.clone(),
        clock.clone(),
        &cfg,
    );
    Harness {
        service,
        authz,
        resolver,
        schemas,
        secrets,
        clock,
    }
}

async fn harness(resolver: StaticTenantResolver) -> Harness {
    harness_with(
        resolver,
        StaticAuthz::permit_own_tenant(),
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await
}

/// DESIGN §3.6 hierarchy: Root → Partner → Customer → Team, Root → Other.
struct Tenants {
    root: Uuid,
    partner: Uuid,
    other: Uuid,
    customer: Uuid,
    team: Uuid,
}

impl Tenants {
    fn new() -> Self {
        Self {
            root: Uuid::new_v4(),
            partner: Uuid::new_v4(),
            other: Uuid::new_v4(),
            customer: Uuid::new_v4(),
            team: Uuid::new_v4(),
        }
    }

    fn resolver(&self) -> StaticTenantResolver {
        StaticTenantResolver::new()
            .with_chain(self.root, vec![])
            .with_chain(self.partner, vec![self.root])
            .with_chain(self.other, vec![self.root])
            .with_chain(self.customer, vec![self.partner, self.root])
            .with_chain(self.team, vec![self.customer, self.partner, self.root])
    }

    fn all(&self) -> [Uuid; 5] {
        [
            self.root,
            self.partner,
            self.other,
            self.customer,
            self.team,
        ]
    }
}

fn ctx(tenant: Uuid) -> SecurityContext {
    security_context(tenant)
}

fn provider_req(slug: &str, mode: SharingMode, enabled: bool) -> CreateProviderRequestV1 {
    CreateProviderRequestV1::builder(slug, slug.to_uppercase(), gts::GtsTypeId::new(OPENAI))
        .sharing_mode(mode)
        .enabled(enabled)
        .build()
}

/// `name` doubles as `provider_model_id`, so the canonical id is `slug::name`.
fn model_req(
    provider_id: Uuid,
    name: &str,
    mode: SharingMode,
    state: ApprovalState,
) -> CreateModelRequestV1 {
    let mut req = CreateModelRequestV1::new(provider_id, name, name, name, ContextWindow::new(8));
    req.context_window.max_output_tokens = Some(4);
    req.sharing_mode = Some(mode);
    req.approval_state = Some(state);
    req
}

impl Harness {
    async fn provider(&self, tenant: Uuid, slug: &str, mode: SharingMode, enabled: bool) -> Uuid {
        self.service
            .create_provider(&ctx(tenant), provider_req(slug, mode, enabled))
            .await
            .unwrap_or_else(|e| panic!("create provider {slug}: {e:?}"))
            .provider
            .id
    }

    async fn model(
        &self,
        tenant: Uuid,
        provider: Uuid,
        pmid: &str,
        mode: SharingMode,
        state: ApprovalState,
    ) -> Uuid {
        self.service
            .create_model(&ctx(tenant), model_req(provider, pmid, mode, state))
            .await
            .unwrap_or_else(|e| panic!("create model {pmid}: {e:?}"))
            .model
            .id
    }

    async fn resolve(&self, tenant: Uuid, canonical_id: &str) -> Result<ModelV1, DomainError> {
        self.service.resolve_model(&ctx(tenant), canonical_id).await
    }

    async fn available(&self, tenant: Uuid) -> BTreeSet<String> {
        self.service
            .list_available_models(&ctx(tenant), &ODataQuery::default())
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|m| m.canonical_id)
            .collect()
    }

    async fn managed_models(&self, tenant: Uuid) -> Vec<ModelEntryV1> {
        self.service
            .list_models(&ctx(tenant), &ODataQuery::default())
            .await
            .unwrap()
            .items
    }
}

/// The example catalog of DESIGN §3.6.
struct Catalog {
    root_openai: Uuid,
    partner_openai: Uuid,
    customer_openai: Uuid,
    root_gpt4o: Uuid,
    root_mini: Uuid,
    root_o1: Uuid,
    partner_gpt4o: Uuid,
    customer_gpt4o: Uuid,
}

async fn seed(h: &Harness, t: &Tenants, customer_enabled: bool) -> Catalog {
    let root_openai = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let partner_openai = h
        .provider(t.partner, "openai", SharingMode::Tenant, true)
        .await;
    let customer_openai = h
        .provider(t.customer, "openai", SharingMode::Shared, customer_enabled)
        .await;
    Catalog {
        root_openai,
        partner_openai,
        customer_openai,
        root_gpt4o: h
            .model(
                t.root,
                root_openai,
                "gpt-4o",
                SharingMode::Shared,
                ApprovalState::Approved,
            )
            .await,
        root_mini: h
            .model(
                t.root,
                root_openai,
                "gpt-4o-mini",
                SharingMode::Shared,
                ApprovalState::Approved,
            )
            .await,
        root_o1: h
            .model(
                t.root,
                root_openai,
                "o1-preview",
                SharingMode::Tenant,
                ApprovalState::Approved,
            )
            .await,
        partner_gpt4o: h
            .model(
                t.partner,
                partner_openai,
                "gpt-4o",
                SharingMode::Shared,
                ApprovalState::Approved,
            )
            .await,
        customer_gpt4o: h
            .model(
                t.customer,
                customer_openai,
                "gpt-4o",
                SharingMode::Shared,
                ApprovalState::Approved,
            )
            .await,
    }
}

fn ids(set: &[&str]) -> BTreeSet<String> {
    set.iter().map(|s| (*s).to_owned()).collect()
}

// ---------------------------------------------------------------------------
// Effective catalog and resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolution_follows_the_design_hierarchy_table() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;

    let root = h.resolve(t.root, "openai::gpt-4o").await.unwrap();
    assert_eq!(root.id, c.root_gpt4o);
    assert_eq!(
        h.resolve(t.root, "openai::gpt-4o-mini").await.unwrap().id,
        c.root_mini
    );
    assert_eq!(
        h.resolve(t.root, "openai::o1-preview").await.unwrap().id,
        c.root_o1
    );
    assert_eq!(
        h.available(t.root).await,
        ids(&[
            "openai::gpt-4o",
            "openai::gpt-4o-mini",
            "openai::o1-preview"
        ])
    );

    let partner = h.resolve(t.partner, "openai::gpt-4o").await.unwrap();
    assert_eq!(
        partner.id, c.partner_gpt4o,
        "own provider wins even in tenant mode"
    );
    assert!(matches!(
        h.resolve(t.partner, "openai::gpt-4o-mini").await,
        Err(DomainError::ModelNotFound { .. })
    ));
    assert_eq!(h.available(t.partner).await, ids(&["openai::gpt-4o"]));

    let other = h.resolve(t.other, "openai::gpt-4o").await.unwrap();
    assert_eq!(other.id, c.root_gpt4o);
    assert_eq!(other.info.provider_settings, serde_json::json!({}));
    assert!(
        matches!(
            h.resolve(t.other, "openai::o1-preview").await,
            Err(DomainError::ModelNotFound { .. })
        ),
        "tenant-mode model"
    );
    assert_eq!(
        h.available(t.other).await,
        ids(&["openai::gpt-4o", "openai::gpt-4o-mini"])
    );

    let customer = h.resolve(t.customer, "openai::gpt-4o").await.unwrap();
    assert_eq!(
        customer.id, c.customer_gpt4o,
        "customer's own provider is nearer than root's"
    );
    assert!(
        matches!(
            h.resolve(t.customer, "openai::gpt-4o-mini").await,
            Err(DomainError::ModelNotFound { .. })
        ),
        "no canonical_id dedupe leak"
    );
    assert_eq!(h.available(t.customer).await, ids(&["openai::gpt-4o"]));

    let team = h.resolve(t.team, "openai::gpt-4o").await.unwrap();
    assert_eq!(team.id, c.customer_gpt4o);
    assert_eq!(h.available(t.team).await, ids(&["openai::gpt-4o"]));

    for bad in ["openai", "::gpt-4o", "openai::", "nope::gpt-4o"] {
        assert!(
            matches!(
                h.resolve(t.root, bad).await,
                Err(DomainError::ModelNotFound { .. })
            ),
            "{bad}"
        );
    }
    let _ = (c.root_openai, c.partner_openai, c.customer_openai);
}

#[tokio::test]
async fn disabled_shared_shadow_blocks_the_slug_in_the_whole_subtree() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, false).await;
    for tenant in [t.customer, t.team] {
        assert!(matches!(
            h.resolve(tenant, "openai::gpt-4o").await,
            Err(DomainError::ProviderDisabled { .. })
        ));
        assert!(matches!(
            h.resolve(tenant, "openai::gpt-4o-mini").await,
            Err(DomainError::ModelNotFound { .. })
        ));
        assert!(h.available(tenant).await.is_empty());
    }
    assert_eq!(
        h.available(t.partner).await,
        ids(&["openai::gpt-4o"]),
        "the owner's subtree only"
    );

    // Re-enabling restores the models on the next request.
    h.service
        .update_provider(
            &ctx(t.customer),
            c.customer_openai,
            provider_update(|r| r.enabled = Some(true)),
        )
        .await
        .unwrap();
    assert_eq!(
        h.resolve(t.team, "openai::gpt-4o").await.unwrap().id,
        c.customer_gpt4o
    );
}

#[tokio::test]
async fn deleting_a_shadowing_provider_restores_the_inherited_one() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;
    h.service
        .delete_provider(&ctx(t.customer), c.customer_openai, None)
        .await
        .unwrap();
    for tenant in [t.customer, t.team] {
        assert_eq!(
            h.resolve(tenant, "openai::gpt-4o").await.unwrap().id,
            c.root_gpt4o,
            "partner's tenant-mode provider is not a candidate"
        );
        assert_eq!(
            h.available(tenant).await,
            ids(&["openai::gpt-4o", "openai::gpt-4o-mini"])
        );
    }
    assert!(
        matches!(
            h.service
                .get_model(&ctx(t.customer), c.customer_gpt4o)
                .await,
            Err(DomainError::ModelNotFound { .. })
        ),
        "cascade"
    );
    assert!(
        h.managed_models(t.customer)
            .await
            .iter()
            .all(|m| m.inherited)
    );
}

#[tokio::test]
async fn sharing_mode_changes_apply_on_the_next_request() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;
    h.service
        .update_provider(
            &ctx(t.partner),
            c.partner_openai,
            provider_update(|r| r.sharing_mode = Some(SharingMode::Shared)),
        )
        .await
        .unwrap();
    // Customer still wins with its own provider; a tenant without one now
    // sees Partner's.
    assert_eq!(
        h.resolve(t.customer, "openai::gpt-4o").await.unwrap().id,
        c.customer_gpt4o
    );
    h.service
        .delete_provider(&ctx(t.customer), c.customer_openai, None)
        .await
        .unwrap();
    assert_eq!(
        h.resolve(t.team, "openai::gpt-4o").await.unwrap().id,
        c.partner_gpt4o
    );

    h.service
        .update_model(
            &ctx(t.partner),
            c.partner_gpt4o,
            model_update(|r| r.sharing_mode = Some(SharingMode::Tenant)),
        )
        .await
        .unwrap();
    assert!(matches!(
        h.resolve(t.team, "openai::gpt-4o").await,
        Err(DomainError::ModelNotFound { .. })
    ));
}

// ---------------------------------------------------------------------------
// Approval
// ---------------------------------------------------------------------------

#[tokio::test]
async fn only_approved_models_resolve_and_revoke_applies_on_the_next_call() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let subject = Uuid::new_v4();
    let created = h
        .service
        .create_model(
            &security_context_for(t.root, subject),
            model_req(p, "gpt-4o", SharingMode::Shared, ApprovalState::Pending),
        )
        .await
        .unwrap();
    assert_eq!(created.model.approval.state, ApprovalState::Pending);
    assert_eq!(created.model.approval.decided_by, subject);
    assert_eq!(created.model.approval.decided_at, h.clock.now_for_test());
    assert!(!created.available_for_eval);
    let err = h.resolve(t.other, "openai::gpt-4o").await.unwrap_err();
    assert!(matches!(
        err,
        DomainError::ModelNotApproved {
            state: ApprovalState::Pending,
            ..
        }
    ));
    assert!(h.available(t.other).await.is_empty());

    let approver = Uuid::new_v4();
    h.clock.advance(chrono::Duration::minutes(5));
    let approved = h
        .service
        .update_model(
            &security_context_for(t.root, approver),
            created.model.id,
            model_update(|r| r.approval_state = Some(ApprovalState::Approved)),
        )
        .await
        .unwrap();
    assert_eq!(approved.model.approval.decided_by, approver);
    assert_eq!(approved.model.approval.decided_at, h.clock.now_for_test());
    assert!(approved.available_for_eval);
    assert!(h.resolve(t.other, "openai::gpt-4o").await.is_ok());
    assert_eq!(h.available(t.other).await, ids(&["openai::gpt-4o"]));

    // Approval defaults to approved on create.
    let default_created = h
        .service
        .create_model(&ctx(t.root), {
            let mut r = model_req(
                p,
                "gpt-4o-mini",
                SharingMode::Shared,
                ApprovalState::Approved,
            );
            r.approval_state = None;
            r
        })
        .await
        .unwrap();
    assert_eq!(
        default_created.model.approval.state,
        ApprovalState::Approved
    );

    h.service
        .update_model(
            &ctx(t.root),
            created.model.id,
            model_update(|r| r.approval_state = Some(ApprovalState::Revoked)),
        )
        .await
        .unwrap();
    assert!(matches!(
        h.resolve(t.other, "openai::gpt-4o").await,
        Err(DomainError::ModelNotApproved {
            state: ApprovalState::Revoked,
            ..
        })
    ));
    assert_eq!(h.available(t.other).await, ids(&["openai::gpt-4o-mini"]));
    // The admin list still returns it, flagged.
    let listed = h.managed_models(t.other).await;
    let revoked = listed
        .iter()
        .find(|m| m.model.canonical_id == "openai::gpt-4o")
        .unwrap();
    assert!(
        revoked.inherited
            && !revoked.shadowed
            && revoked.provider_enabled
            && !revoked.available_for_eval
    );
}

// ---------------------------------------------------------------------------
// GTS checks
// ---------------------------------------------------------------------------

fn reasons(err: &DomainError) -> Vec<(String, field::Reason)> {
    match err {
        DomainError::Invalid { violations: v, .. } => {
            v.iter().map(|v| (v.field.clone(), v.reason)).collect()
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[tokio::test]
async fn provider_type_and_settings_are_validated_on_writes() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = ctx(t.root);

    for (gts_type, reason) in [
        ("not-an-id", field::Reason::InvalidGtsType),
        (
            gts_id!("cf.genai.model.provider.v1~"),
            field::Reason::NotAProviderType,
        ),
        (
            gts_id!("cf.genai.model.provider.v1~cf.genai._.unknown.v1~"),
            field::Reason::GtsTypeNotRegistered,
        ),
    ] {
        let req = CreateProviderRequestV1::builder("p", "P", gts::GtsTypeId::new(gts_type)).build();
        let err = h.service.create_provider(&c, req).await.unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![("gts_type".to_owned(), reason)],
            "{gts_type}"
        );
    }

    let strict = h
        .service
        .create_provider(
            &c,
            CreateProviderRequestV1::builder("strict", "Strict", gts::GtsTypeId::new(STRICT))
                .build(),
        )
        .await
        .unwrap()
        .provider;
    let mut req = model_req(
        strict.id,
        "m1",
        SharingMode::Tenant,
        ApprovalState::Approved,
    );
    req.provider_settings = serde_json::json!({ "endpoint": { "port": "high" }, "extra": 1 });
    let err = h.service.create_model(&c, req.clone()).await.unwrap_err();
    assert_eq!(
        reasons(&err),
        vec![(
            "provider_settings".to_owned(),
            field::Reason::SchemaMismatch
        )]
    );

    req.provider_settings =
        serde_json::json!({ "oagw_alias": "strict-prod", "endpoint": { "port": 443 } });
    let created = h.service.create_model(&c, req).await.unwrap();
    assert_eq!(
        created.model.info.gts_type.as_ref(),
        STRICT,
        "gts_type is copied from the provider"
    );
    assert_eq!(created.model.canonical_id, "strict::m1");

    let err = h
        .service
        .update_model(
            &c,
            created.model.id,
            model_update(|r| r.provider_settings = Some(serde_json::json!({}))),
        )
        .await
        .unwrap_err();
    assert_eq!(reasons(&err)[0].1, field::Reason::SchemaMismatch);
    let calls_before = h.schemas.calls();
    h.service
        .update_model(
            &c,
            created.model.id,
            model_update(|r| r.display_name = Some("renamed".into())),
        )
        .await
        .unwrap();
    assert_eq!(
        h.schemas.calls(),
        calls_before,
        "no schema fetch without provider_settings"
    );
    assert!(h.resolve(t.root, "strict::m1").await.is_ok());
}

#[tokio::test]
async fn request_field_checks_and_duplicates() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = ctx(t.root);
    let err = h
        .service
        .create_provider(&c, provider_req("Bad Slug", SharingMode::Tenant, true))
        .await
        .unwrap_err();
    assert_eq!(
        reasons(&err)[0],
        ("slug".to_owned(), field::Reason::InvalidFormat)
    );

    let p = h
        .provider(t.root, "openai", SharingMode::Tenant, true)
        .await;
    let err = h
        .service
        .create_provider(&c, provider_req("openai", SharingMode::Shared, true))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::ProviderAlreadyExists { ref slug } if slug == "openai"));

    h.model(
        t.root,
        p,
        "gpt-4o",
        SharingMode::Tenant,
        ApprovalState::Approved,
    )
    .await;
    let err = h
        .service
        .create_model(
            &c,
            model_req(p, "gpt-4o", SharingMode::Shared, ApprovalState::Pending),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::ModelAlreadyExists { ref canonical_id } if canonical_id == "openai::gpt-4o")
    );

    let mut req = model_req(p, "bad-id", SharingMode::Tenant, ApprovalState::Approved);
    req.provider_model_id = "bad id".into();
    req.base_url = Some("ftp://x".into());
    let err = h.service.create_model(&c, req).await.unwrap_err();
    let r = reasons(&err);
    assert!(r.contains(&("provider_model_id".to_owned(), field::Reason::InvalidFormat)));
    assert!(r.contains(&("base_url".to_owned(), field::Reason::InvalidFormat)));
    assert!(!r.iter().any(|(f, _)| f == "name"), "{r:?}");
    let mut req = model_req(p, "Bad Name", SharingMode::Tenant, ApprovalState::Approved);
    req.provider_model_id = "bad-name".into();
    let err = h.service.create_model(&c, req).await.unwrap_err();
    assert_eq!(
        reasons(&err),
        vec![("name".to_owned(), field::Reason::InvalidFormat)]
    );

    let err = h
        .service
        .create_model(
            &c,
            model_req(
                Uuid::new_v4(),
                "x",
                SharingMode::Tenant,
                ApprovalState::Approved,
            ),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::ProviderNotFound { .. }));
}

// ---------------------------------------------------------------------------
// Isolation and management reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn isolation_matrix_over_siblings_ancestors_and_descendants() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;

    // Siblings see nothing of each other.
    assert!(matches!(
        h.service
            .get_provider(&ctx(t.other), c.partner_openai)
            .await,
        Err(DomainError::ProviderNotFound { .. })
    ));
    assert!(matches!(
        h.service.get_model(&ctx(t.other), c.partner_gpt4o).await,
        Err(DomainError::ModelNotFound { .. })
    ));
    assert!(matches!(
        h.service
            .update_provider(&ctx(t.other), c.partner_openai, provider_update(|_| {}))
            .await,
        Err(DomainError::ProviderNotFound { .. })
    ));

    // Ancestors never see descendant rows.
    assert_eq!(h.managed_models(t.root).await.len(), 3);
    assert!(matches!(
        h.service.get_model(&ctx(t.root), c.customer_gpt4o).await,
        Err(DomainError::ModelNotFound { .. })
    ));
    assert!(matches!(
        h.service
            .delete_model(&ctx(t.root), c.customer_gpt4o, None)
            .await,
        Err(DomainError::ModelNotFound { .. })
    ));

    // Descendants read inherited shared rows but cannot write them.
    let inherited = h
        .service
        .get_provider(&ctx(t.team), c.root_openai)
        .await
        .unwrap();
    assert!(inherited.inherited && inherited.shadowed && !inherited.available_for_eval);
    assert_eq!(inherited.provider.name, "OPENAI", "full record");
    for err in [
        h.service
            .update_provider(&ctx(t.team), c.root_openai, provider_update(|_| {}))
            .await
            .err(),
        h.service
            .delete_provider(&ctx(t.team), c.root_openai, None)
            .await
            .err(),
        h.service
            .update_model(
                &ctx(t.team),
                c.root_gpt4o,
                model_update(|r| r.approval_state = Some(ApprovalState::Revoked)),
            )
            .await
            .err(),
        h.service
            .delete_model(&ctx(t.team), c.root_gpt4o, None)
            .await
            .err(),
        h.service
            .create_model(
                &ctx(t.team),
                model_req(
                    c.root_openai,
                    "new",
                    SharingMode::Tenant,
                    ApprovalState::Approved,
                ),
            )
            .await
            .err(),
    ] {
        assert!(
            matches!(err, Some(DomainError::InheritedReadOnly { .. })),
            "{err:?}"
        );
    }
    // An ancestor's tenant-mode rows are not visible at all.
    assert!(matches!(
        h.service
            .get_provider(&ctx(t.customer), c.partner_openai)
            .await,
        Err(DomainError::ProviderNotFound { .. })
    ));
    assert!(matches!(
        h.service.get_model(&ctx(t.other), c.root_o1).await,
        Err(DomainError::ModelNotFound { .. })
    ));
    assert!(matches!(
        h.service
            .delete_provider(&ctx(t.customer), c.partner_openai, None)
            .await,
        Err(DomainError::ProviderNotFound { .. })
    ));
    assert_eq!(
        h.resolve(t.root, "openai::gpt-4o")
            .await
            .unwrap()
            .approval
            .state,
        ApprovalState::Approved,
        "root's row is untouched"
    );
}

#[tokio::test]
async fn management_lists_include_shadowed_candidates_with_flags() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;

    let providers = h
        .service
        .list_providers(&ctx(t.team), &ODataQuery::default())
        .await
        .unwrap()
        .items;
    let by_id = |id: Uuid| providers.iter().find(|p| p.provider.id == id).unwrap();
    assert_eq!(
        providers.len(),
        2,
        "partner's tenant-mode provider is excluded"
    );
    let customer = by_id(c.customer_openai);
    assert!(customer.inherited && !customer.shadowed && customer.available_for_eval);
    let root = by_id(c.root_openai);
    assert!(root.inherited && root.shadowed && !root.available_for_eval);

    let models = h.managed_models(t.team).await;
    let names: BTreeSet<(String, bool)> = models
        .iter()
        .map(|m| (m.model.canonical_id.clone(), m.shadowed))
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            ("openai::gpt-4o".to_owned(), false),
            ("openai::gpt-4o".to_owned(), true),
            ("openai::gpt-4o-mini".to_owned(), true),
        ]),
        "o1-preview is tenant mode, partner's gpt-4o has a tenant-mode provider"
    );
    let own = h.managed_models(t.customer).await;
    let mine = own.iter().find(|m| m.model.id == c.customer_gpt4o).unwrap();
    assert!(!mine.inherited && !mine.shadowed && mine.available_for_eval);

    // The root admin sees its own rows only, in every state.
    let pending = h
        .model(
            t.root,
            c.root_openai,
            "pending",
            SharingMode::Tenant,
            ApprovalState::Pending,
        )
        .await;
    let root_models = h.managed_models(t.root).await;
    assert_eq!(root_models.len(), 4);
    let p = root_models.iter().find(|m| m.model.id == pending).unwrap();
    assert!(!p.available_for_eval && !p.inherited);
    assert_eq!(
        h.service
            .get_model(&ctx(t.root), pending)
            .await
            .unwrap()
            .model
            .approval
            .state,
        ApprovalState::Pending
    );
}

// ---------------------------------------------------------------------------
// Authorization and fail-closed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pdp_outcomes_per_operation() {
    let t = Tenants::new();
    // Permit everything except what the test flips through a shared switch.
    let denied = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let denied_in = denied.clone();
    let authz = StaticAuthz::with(move |req| {
        if denied_in.lock().unwrap().contains(&req.action.name) {
            Ok(deny())
        } else {
            Ok(permit_with(vec![own_tenant_constraint(req)]))
        }
    });
    let h = harness_with(
        t.resolver(),
        authz,
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await;
    let c = seed(&h, &t, true).await;

    *denied.lock().unwrap() = vec!["evaluate".into()];
    assert!(matches!(
        h.resolve(t.root, "openai::gpt-4o").await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(
        matches!(
            h.service
                .list_available_models(&ctx(t.root), &ODataQuery::default())
                .await,
            Err(DomainError::NotPermitted { .. })
        ),
        "the available list takes the same evaluate decision"
    );
    assert!(
        h.service
            .get_model(&ctx(t.root), c.root_gpt4o)
            .await
            .is_ok(),
        "management read unaffected"
    );

    *denied.lock().unwrap() = vec![
        "list".into(),
        "get".into(),
        "update".into(),
        "delete".into(),
        "create".into(),
    ];
    assert!(matches!(
        h.service
            .list_models(&ctx(t.root), &ODataQuery::default())
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service
            .list_providers(&ctx(t.root), &ODataQuery::default())
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service.get_provider(&ctx(t.root), c.root_openai).await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service
            .delete_model(&ctx(t.root), c.root_gpt4o, None)
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service
            .create_provider(&ctx(t.root), provider_req("x", SharingMode::Tenant, true))
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(
        h.resolve(t.root, "openai::gpt-4o").await.is_ok(),
        "evaluation unaffected"
    );

    // Catalog reasons come before the PDP on resolve.
    *denied.lock().unwrap() = vec!["evaluate".into()];
    let calls_before = h.authz.actions().len();
    assert!(matches!(
        h.resolve(t.other, "openai::o1-preview").await,
        Err(DomainError::ModelNotFound { .. })
    ));
    assert_eq!(
        h.authz.actions().len(),
        calls_before,
        "a model outside the catalog makes no PDP call"
    );
}

#[tokio::test]
async fn pdp_row_constraints_admit_matching_rows() {
    let t = Tenants::new();
    let allowed = Arc::new(std::sync::Mutex::new(Uuid::nil()));
    let allowed_in = allowed.clone();
    let authz = StaticAuthz::with(move |req| {
        let id = *allowed_in.lock().unwrap();
        let mut predicates = vec![Predicate::Eq(EqPredicate::new(
            "owner_tenant_id",
            subject_tenant(req),
        ))];
        if id != Uuid::nil() {
            predicates.push(Predicate::In(InPredicate::new("provider_id", [id])));
        }
        Ok(permit_with(vec![Constraint { predicates }]))
    });
    let h = harness_with(
        t.resolver(),
        authz,
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await;
    let openai = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let anthropic = h
        .provider(t.root, "anthropic", SharingMode::Shared, true)
        .await;
    let gpt = h
        .model(
            t.root,
            openai,
            "gpt-4o",
            SharingMode::Shared,
            ApprovalState::Approved,
        )
        .await;
    let claude = h
        .model(
            t.root,
            anthropic,
            "claude",
            SharingMode::Shared,
            ApprovalState::Approved,
        )
        .await;
    *allowed.lock().unwrap() = openai;

    assert_eq!(h.resolve(t.other, "openai::gpt-4o").await.unwrap().id, gpt);
    assert!(matches!(
        h.resolve(t.other, "anthropic::claude").await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert_eq!(h.available(t.other).await, ids(&["openai::gpt-4o"]));
    let listed: Vec<Uuid> = h
        .managed_models(t.other)
        .await
        .into_iter()
        .map(|m| m.model.id)
        .collect();
    assert_eq!(listed, vec![gpt]);
    assert!(h.service.get_model(&ctx(t.root), gpt).await.is_ok());
    assert!(matches!(
        h.service.get_model(&ctx(t.root), claude).await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        h.service.delete_model(&ctx(t.root), claude, None).await,
        Err(DomainError::NotPermitted { .. })
    ));
    // Create checks the row constraints against the new record.
    assert!(matches!(
        h.service
            .create_model(
                &ctx(t.root),
                model_req(
                    anthropic,
                    "claude-2",
                    SharingMode::Shared,
                    ApprovalState::Approved
                )
            )
            .await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(
        h.service
            .create_model(
                &ctx(t.root),
                model_req(
                    openai,
                    "gpt-4o-mini",
                    SharingMode::Shared,
                    ApprovalState::Approved
                )
            )
            .await
            .is_ok()
    );
}

/// A type migration is checked against the PDP row constraints with the new
/// `gts_type`.
#[tokio::test]
async fn type_migration_out_of_the_permitted_types_is_not_permitted() {
    let t = Tenants::new();
    let authz = StaticAuthz::with(|req| {
        Ok(permit_with(vec![Constraint {
            predicates: vec![
                Predicate::Eq(EqPredicate::new("owner_tenant_id", subject_tenant(req))),
                Predicate::In(InPredicate::new("gts_type", [OPENAI])),
            ],
        }]))
    });
    let h = harness_with(
        t.resolver(),
        authz,
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await;
    let c = ctx(t.root);
    let p = h
        .provider(t.root, "openai", SharingMode::Tenant, true)
        .await;

    let err = h
        .service
        .update_provider(
            &c,
            p,
            provider_update(|r| r.gts_type = Some(gts::GtsTypeId::new(STRICT))),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotPermitted { .. }), "{err:?}");
    let provider = h.service.get_provider(&c, p).await.unwrap().provider;
    assert_eq!(provider.gts_type.as_ref(), OPENAI);
    assert_eq!(provider.version, 1);

    let renamed = h
        .service
        .update_provider(&c, p, provider_update(|r| r.name = Some("OpenAI 2".into())))
        .await
        .unwrap();
    assert_eq!(renamed.provider.version, 2);
}

#[tokio::test]
async fn every_dependency_fails_closed() {
    let t = Tenants::new();

    // PDP down: every operation is dependency unavailable, never a grant.
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let down_in = down.clone();
    let authz = StaticAuthz::with(move |req| {
        if down_in.load(std::sync::atomic::Ordering::SeqCst) {
            Err(toolkit_canonical_errors::CanonicalError::service_unavailable().create())
        } else {
            Ok(permit_with(vec![own_tenant_constraint(req)]))
        }
    });
    let h = harness_with(
        t.resolver(),
        authz,
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await;
    seed(&h, &t, true).await;
    down.store(true, std::sync::atomic::Ordering::SeqCst);
    for err in [
        h.resolve(t.root, "openai::gpt-4o").await.err(),
        h.service
            .list_available_models(&ctx(t.root), &ODataQuery::default())
            .await
            .err(),
        h.service
            .list_models(&ctx(t.root), &ODataQuery::default())
            .await
            .err(),
        h.service
            .create_provider(&ctx(t.root), provider_req("x", SharingMode::Tenant, true))
            .await
            .err(),
    ] {
        assert!(
            matches!(
                err,
                Some(DomainError::DependencyUnavailable {
                    dependency: Dependency::Pdp,
                    ..
                })
            ),
            "{err:?}"
        );
    }

    // PDP slow past the deadline.
    let cfg = ModelRegistryConfig {
        pdp_deadline_ms: 20,
        ..Default::default()
    };
    let h = harness_with(
        t.resolver(),
        StaticAuthz::permit_own_tenant().with_delay(Duration::from_millis(200)),
        schemas(),
        cfg,
    )
    .await;
    assert!(matches!(
        h.service
            .list_models(&ctx(t.root), &ODataQuery::default())
            .await,
        Err(DomainError::DependencyUnavailable {
            dependency: Dependency::Pdp,
            ..
        })
    ));

    // Tenant Resolver down, and refusing the caller's tenant.
    let h = harness(
        StaticTenantResolver::new()
            .failing_with(|| tenant_resolver_sdk::TenantResolverError::Internal("x".into())),
    )
    .await;
    assert!(matches!(
        h.resolve(t.root, "openai::gpt-4o").await,
        Err(DomainError::DependencyUnavailable {
            dependency: Dependency::TenantResolver,
            ..
        })
    ));
    let h = harness(StaticTenantResolver::new()).await; // unknown tenant → TenantNotFound
    assert!(matches!(
        h.resolve(t.root, "openai::gpt-4o").await,
        Err(DomainError::NotPermitted { .. })
    ));

    // Types Registry down: writes that need it fail, reads are unaffected.
    let h = harness_with(
        t.resolver(),
        StaticAuthz::permit_own_tenant(),
        StaticSchemaSource::new()
            .failing_with(|| DomainError::unavailable(Dependency::TypesRegistry, "down")),
        ModelRegistryConfig::default(),
    )
    .await;
    assert!(matches!(
        h.service
            .create_provider(
                &ctx(t.root),
                provider_req("openai", SharingMode::Shared, true)
            )
            .await,
        Err(DomainError::DependencyUnavailable {
            dependency: Dependency::TypesRegistry,
            ..
        })
    ));
    assert!(matches!(
        h.resolve(t.root, "openai::gpt-4o").await,
        Err(DomainError::ModelNotFound { .. })
    ));
}

// ---------------------------------------------------------------------------
// OData
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filters_only_narrow_and_orderby_follows_the_field_rules() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    for (pmid, state, vendor, sort_order) in [
        ("a", ApprovalState::Approved, "OpenAI", 2),
        ("b", ApprovalState::Pending, "OpenAI", 1),
        ("c", ApprovalState::Approved, "Other", 0),
    ] {
        let mut req = model_req(p, pmid, SharingMode::Shared, state);
        req.vendor = Some(vendor.into());
        req.sort_order = sort_order;
        h.service.create_model(&ctx(t.root), req).await.unwrap();
    }
    let c = ctx(t.other);

    // A caller filter cannot switch the approval predicate off.
    let q = QueryBuilder::<ModelSchema>::new()
        .filter(MODEL_APPROVAL_STATE.eq(ApprovalState::Pending))
        .build();
    assert!(
        h.service
            .list_available_models(&c, &q)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // The admin list does return it under the same filter.
    assert_eq!(h.service.list_models(&c, &q).await.unwrap().items.len(), 1);

    let q = QueryBuilder::<ModelSchema>::new()
        .filter(MODEL_VENDOR.eq("Other"))
        .build();
    let items = h.service.list_available_models(&c, &q).await.unwrap().items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].canonical_id, "openai::c");

    // Nullable fields are filter-only.
    let q = QueryBuilder::<ModelSchema>::new()
        .order_by(MODEL_VENDOR, SortDir::Asc)
        .build();
    assert!(matches!(
        h.service.list_available_models(&c, &q).await,
        Err(DomainError::OData(_))
    ));
    let q = QueryBuilder::<ModelSchema>::new()
        .order_by(MODEL_CANONICAL_ID, SortDir::Desc)
        .build();
    let items = h.service.list_available_models(&c, &q).await.unwrap().items;
    assert_eq!(
        items
            .iter()
            .map(|m| m.canonical_id.as_str())
            .collect::<Vec<_>>(),
        vec!["openai::c", "openai::a"]
    );
    // `sort_order` is non-nullable and therefore orderable.
    let q = QueryBuilder::<ModelSchema>::new()
        .order_by(MODEL_SORT_ORDER, SortDir::Asc)
        .build();
    let items = h.service.list_available_models(&c, &q).await.unwrap().items;
    assert_eq!(
        items
            .iter()
            .map(|m| (m.canonical_id.as_str(), m.info.sort_order))
            .collect::<Vec<_>>(),
        vec![("openai::c", 0), ("openai::a", 2)]
    );

    // Keyset paging in the default order with the id tie-breaker.
    let mut query = ODataQuery::default().with_limit(1);
    let mut seen = Vec::new();
    loop {
        let page = h.service.list_models(&c, &query).await.unwrap();
        assert_eq!(page.page_info.limit, 1);
        seen.extend(page.items.into_iter().map(|m| m.model.canonical_id));
        let Some(next) = page.page_info.next_cursor else {
            break;
        };
        query = ODataQuery::default()
            .with_limit(1)
            .with_cursor(toolkit_odata::CursorV1::decode(&next).unwrap());
    }
    assert_eq!(seen, vec!["openai::a", "openai::b", "openai::c"]);

    // Page size bounds.
    let cfg = ModelRegistryConfig {
        default_page_size: 2,
        max_page_size: 2,
        ..Default::default()
    };
    let h2 = harness_with(
        t.resolver(),
        StaticAuthz::permit_own_tenant(),
        schemas(),
        cfg,
    )
    .await;
    let page = h2
        .service
        .list_models(&ctx(t.root), &ODataQuery::default().with_limit(50))
        .await
        .unwrap();
    assert_eq!(page.page_info.limit, 2);
    let empty = h2
        .service
        .list_available_models(&ctx(t.root), &ODataQuery::default().with_limit(50))
        .await
        .unwrap();
    assert_eq!(
        empty.page_info.limit, 2,
        "an empty page reports the clamped limit"
    );
}

#[tokio::test]
async fn available_list_equals_the_set_of_resolvable_models() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;
    h.model(
        t.root,
        c.root_openai,
        "pending",
        SharingMode::Shared,
        ApprovalState::Pending,
    )
    .await;
    let anthropic = h
        .provider(t.partner, "anthropic", SharingMode::Shared, false)
        .await;
    h.model(
        t.partner,
        anthropic,
        "claude",
        SharingMode::Shared,
        ApprovalState::Approved,
    )
    .await;
    let all_ids = [
        "openai::gpt-4o",
        "openai::gpt-4o-mini",
        "openai::o1-preview",
        "openai::pending",
        "anthropic::claude",
    ];
    for tenant in t.all() {
        let mut resolvable = BTreeSet::new();
        for id in all_ids {
            if h.resolve(tenant, id).await.is_ok() {
                resolvable.insert(id.to_owned());
            }
        }
        assert_eq!(h.available(tenant).await, resolvable, "tenant {tenant}");
    }
}

/// One generated catalog: per tenant and slug an optional provider
/// `(shared, enabled)`, per provider and name an optional model
/// `(shared, approval state)`, and the self-managed flags of Partner and
/// Customer.
#[derive(Debug, Clone)]
struct GeneratedCatalog {
    providers: Vec<Option<(bool, bool)>>,
    models: Vec<Option<(bool, u8)>>,
    partner_self_managed: bool,
    customer_self_managed: bool,
}

const GENERATED_SLUGS: [&str; 2] = ["alpha", "beta"];
const GENERATED_NAMES: [&str; 2] = ["m1", "m2"];

fn generated_catalog() -> impl proptest::strategy::Strategy<Value = GeneratedCatalog> {
    use proptest::prelude::*;
    (
        proptest::collection::vec(proptest::option::of((any::<bool>(), any::<bool>())), 10),
        proptest::collection::vec(proptest::option::of((any::<bool>(), 0..4u8)), 20),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(providers, models, partner_self_managed, customer_self_managed)| GeneratedCatalog {
                providers,
                models,
                partner_self_managed,
                customer_self_managed,
            },
        )
}

fn sharing(shared: bool) -> SharingMode {
    if shared {
        SharingMode::Shared
    } else {
        SharingMode::Tenant
    }
}

/// Builds the catalog and returns, per tenant, the available list and the
/// set of ids that resolve.
async fn available_and_resolvable(
    catalog: GeneratedCatalog,
) -> Vec<(BTreeSet<String>, BTreeSet<String>)> {
    let t = Tenants::new();
    let mut resolver = t.resolver();
    if catalog.partner_self_managed {
        resolver = resolver.with_self_managed(t.partner);
    }
    if catalog.customer_self_managed {
        resolver = resolver.with_self_managed(t.customer);
    }
    let h = harness(resolver).await;
    let slots = t
        .all()
        .into_iter()
        .flat_map(|tenant| GENERATED_SLUGS.map(|slug| (tenant, slug)));
    for (slot, (tenant, slug)) in slots.enumerate() {
        let Some((shared, enabled)) = catalog.providers[slot] else {
            continue;
        };
        let provider = h.provider(tenant, slug, sharing(shared), enabled).await;
        for (n, name) in GENERATED_NAMES.into_iter().enumerate() {
            let Some((shared, state)) = catalog.models[slot * 2 + n] else {
                continue;
            };
            let state = match state {
                0 => ApprovalState::Pending,
                1 => ApprovalState::Approved,
                2 => ApprovalState::Rejected,
                _ => ApprovalState::Revoked,
            };
            h.model(tenant, provider, name, sharing(shared), state)
                .await;
        }
    }
    let mut out = Vec::new();
    for tenant in t.all() {
        let mut resolvable = BTreeSet::new();
        for slug in GENERATED_SLUGS {
            for name in GENERATED_NAMES {
                let id = format!("{slug}::{name}");
                if h.resolve(tenant, &id).await.is_ok() {
                    resolvable.insert(id);
                }
            }
        }
        out.push((h.available(tenant).await, resolvable));
    }
    out
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    /// `list_available_models` returns exactly the canonical ids for which
    /// `resolve_model` succeeds, over shadowing, sharing, approval,
    /// enablement, and barriers.
    #[test]
    fn available_list_equals_resolvable_set_for_any_catalog(catalog in generated_catalog()) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (tenant, (available, resolvable)) in runtime
            .block_on(available_and_resolvable(catalog))
            .into_iter()
            .enumerate()
        {
            proptest::prop_assert_eq!(available, resolvable, "tenant #{}", tenant);
        }
    }
}

// ---------------------------------------------------------------------------
// Isolation barriers (ADR-0001)
// ---------------------------------------------------------------------------

/// Root → Island (self-managed) → Below. The resolver double holds the full
/// chains and cuts them at the barrier only under `BarrierMode::Respect`.
#[tokio::test]
async fn inheritance_stops_at_a_self_managed_tenant() {
    let root = Uuid::new_v4();
    let island = Uuid::new_v4();
    let below = Uuid::new_v4();
    let h = harness(
        StaticTenantResolver::new()
            .with_chain(root, vec![])
            .with_chain(island, vec![root])
            .with_chain(below, vec![island, root])
            .with_self_managed(island),
    )
    .await;
    let root_openai = h.provider(root, "openai", SharingMode::Shared, true).await;
    let root_gpt = h
        .model(
            root,
            root_openai,
            "gpt-4o",
            SharingMode::Shared,
            ApprovalState::Approved,
        )
        .await;
    let island_mistral = h
        .provider(island, "mistral", SharingMode::Shared, true)
        .await;
    for (name, mode) in [
        ("large", SharingMode::Shared),
        ("small", SharingMode::Tenant),
    ] {
        h.model(island, island_mistral, name, mode, ApprovalState::Approved)
            .await;
    }

    // The self-managed tenant inherits nothing, `shared` or not.
    assert_eq!(
        h.available(island).await,
        ids(&["mistral::large", "mistral::small"])
    );
    // Its descendant inherits its `shared` entries and nothing from above it.
    assert_eq!(h.available(below).await, ids(&["mistral::large"]));
    for tenant in [island, below] {
        assert!(
            matches!(
                h.resolve(tenant, "openai::gpt-4o").await,
                Err(DomainError::ModelNotFound { .. })
            ),
            "resolve across the barrier"
        );
        assert!(
            h.managed_models(tenant)
                .await
                .iter()
                .all(|e| e.model.tenant_id == island),
            "admin list across the barrier"
        );
        assert!(matches!(
            h.service.get_model(&ctx(tenant), root_gpt).await,
            Err(DomainError::ModelNotFound { .. })
        ));
        assert!(matches!(
            h.service.get_provider(&ctx(tenant), root_openai).await,
            Err(DomainError::ProviderNotFound { .. })
        ));
        let providers = h
            .service
            .list_providers(&ctx(tenant), &ODataQuery::default())
            .await
            .unwrap();
        assert!(providers.items.iter().all(|e| e.provider.slug == "mistral"));
        // A write under the ancestor's provider is not found, not inherited.
        assert!(matches!(
            h.service
                .create_model(
                    &ctx(tenant),
                    model_req(
                        root_openai,
                        "x",
                        SharingMode::Tenant,
                        ApprovalState::Approved
                    )
                )
                .await,
            Err(DomainError::ProviderNotFound { .. })
        ));
    }
    // Ancestors see nothing of the self-managed subtree.
    assert_eq!(h.available(root).await, ids(&["openai::gpt-4o"]));
    assert!(
        h.managed_models(root)
            .await
            .iter()
            .all(|e| e.model.tenant_id == root)
    );
}

/// A PDP grant through `InTenantSubtree` denies every operation, whatever
/// the subtree root (DESIGN §4 Authorization rule 3).
#[tokio::test]
async fn subtree_tenant_constraints_deny() {
    let t = Tenants::new();
    let roots = [t.root, t.customer, t.team];
    for root in roots {
        let authz = StaticAuthz::with(move |_| {
            Ok(permit_with(vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                    "owner_tenant_id",
                    root,
                ))],
            }]))
        });
        let h = harness_with(
            t.resolver(),
            authz,
            schemas(),
            ModelRegistryConfig::default(),
        )
        .await;
        let query = ODataQuery::default();
        assert!(matches!(
            h.service.list_available_models(&ctx(t.team), &query).await,
            Err(DomainError::NotPermitted { .. })
        ));
        assert!(matches!(
            h.service.list_models(&ctx(t.team), &query).await,
            Err(DomainError::NotPermitted { .. })
        ));
        assert!(matches!(
            h.service.list_providers(&ctx(t.team), &query).await,
            Err(DomainError::NotPermitted { .. })
        ));
        assert!(matches!(
            h.service
                .create_provider(
                    &ctx(t.team),
                    provider_req("openai", SharingMode::Tenant, true)
                )
                .await,
            Err(DomainError::NotPermitted { .. })
        ));
    }
}

/// Writes take the PDP decision without reading the ancestor chain, and a
/// denied management read makes no Tenant Resolver call.
#[tokio::test]
async fn pdp_decision_precedes_the_chain() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let provider = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let model = h
        .model(
            t.root,
            provider,
            "gpt-4o",
            SharingMode::Shared,
            ApprovalState::Approved,
        )
        .await;
    h.service
        .update_model(&ctx(t.root), model, model_update(|_| {}))
        .await
        .unwrap();
    h.service
        .delete_model(&ctx(t.root), model, None)
        .await
        .unwrap();
    h.service
        .delete_provider(&ctx(t.root), provider, None)
        .await
        .unwrap();
    assert_eq!(h.resolver.calls(), 0, "own-row writes read no chain");

    let denied = harness_with(
        t.resolver(),
        StaticAuthz::deny_all(),
        schemas(),
        ModelRegistryConfig::default(),
    )
    .await;
    let query = ODataQuery::default();
    assert!(matches!(
        denied.service.list_models(&ctx(t.team), &query).await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert!(matches!(
        denied.service.get_provider(&ctx(t.team), provider).await,
        Err(DomainError::NotPermitted { .. })
    ));
    assert_eq!(
        denied.resolver.calls(),
        0,
        "a denial makes no resolver call"
    );
}

// ---------------------------------------------------------------------------
// Chain cache, audit, updates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chain_cache_serves_the_warm_path_and_respects_barriers() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = seed(&h, &t, true).await;
    h.resolve(t.team, "openai::gpt-4o").await.unwrap();
    let calls = h.resolver.calls();
    h.resolve(t.team, "openai::gpt-4o").await.unwrap();
    h.available(t.team).await;
    h.managed_models(t.team).await;
    assert_eq!(
        h.resolver.calls(),
        calls,
        "warm chain makes no resolver call"
    );
    assert_eq!(h.resolver.last_barrier_mode(), Some(BarrierMode::Respect));

    h.clock.advance(chrono::Duration::seconds(301));
    h.resolve(t.team, "openai::gpt-4o").await.unwrap();
    assert_eq!(h.resolver.calls(), calls + 1, "expired entry is refreshed");
    let _ = c;

    let cfg = ModelRegistryConfig {
        chain_cache_ttl_seconds: 0,
        ..Default::default()
    };
    let h = harness_with(
        t.resolver(),
        StaticAuthz::permit_own_tenant(),
        schemas(),
        cfg,
    )
    .await;
    let before = h.resolver.calls();
    h.available(t.root).await;
    h.available(t.root).await;
    assert_eq!(h.resolver.calls(), before + 2, "TTL 0 disables the cache");
}

#[tokio::test]
async fn audit_columns_use_the_clock_and_the_subject() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let creator = Uuid::new_v4();
    let created = h
        .service
        .create_provider(
            &security_context_for(t.root, creator),
            provider_req("openai", SharingMode::Tenant, true),
        )
        .await
        .unwrap()
        .provider;
    let t0 = h.clock.now_for_test();
    assert_eq!((created.created_by, created.updated_by), (creator, creator));
    assert_eq!((created.created_at, created.updated_at), (t0, t0));

    let editor = Uuid::new_v4();
    h.clock.advance(chrono::Duration::hours(1));
    let updated = h
        .service
        .update_provider(
            &security_context_for(t.root, editor),
            created.id,
            provider_update(|r| {
                r.name = Some("Renamed".into());
                r.metadata = Some(Some(serde_json::json!({ "k": 1 })));
            }),
        )
        .await
        .unwrap()
        .provider;
    assert_eq!(updated.name, "Renamed");
    assert_eq!(updated.metadata, Some(serde_json::json!({ "k": 1 })));
    assert_eq!((updated.created_by, updated.created_at), (creator, t0));
    assert_eq!(
        (updated.updated_by, updated.updated_at),
        (editor, h.clock.now_for_test())
    );
    assert_eq!(updated.slug, "openai", "immutable");

    let cleared = h
        .service
        .update_provider(
            &ctx(t.root),
            created.id,
            provider_update(|r| r.metadata = Some(None)),
        )
        .await
        .unwrap()
        .provider;
    assert_eq!(cleared.metadata, None);
    assert_eq!(cleared.name, "Renamed", "absent fields stay");
}

#[tokio::test]
async fn model_update_replaces_whole_values_and_keeps_promoted_columns_in_sync() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let mut req = model_req(p, "gpt-4o", SharingMode::Shared, ApprovalState::Approved);
    req.capabilities.streaming = true;
    req.capabilities.vision.enabled = true;
    req.capabilities.tools.enabled = true;
    req.base_url = Some("https://api.openai.com/v1".into());
    req.secret_ref = Some("openai-key".into());
    let id = h
        .service
        .create_model(&ctx(t.root), req)
        .await
        .unwrap()
        .model
        .id;
    let c = ctx(t.other);

    let q = QueryBuilder::<ModelSchema>::new()
        .filter(model_registry_sdk::odata::MODEL_STREAMING.eq(true))
        .build();
    let listed = h.service.list_available_models(&c, &q).await.unwrap().items;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].version, 1);
    let listed = serde_json::to_value(&listed[0]).unwrap();
    for key in ["base_url", "secret_ref", "provider_settings", "cost"] {
        assert!(
            listed["info"].get(key).is_some(),
            "{key} is returned in full"
        );
    }
    assert_eq!(listed["canonical_id"], "openai::gpt-4o");
    assert_eq!(listed["name"], "gpt-4o");
    let resolved = h.resolve(t.other, "openai::gpt-4o").await.unwrap();
    assert_eq!(
        resolved.info.base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(resolved.info.secret_ref.as_deref(), Some("openai-key"));

    let mut disabled = model_registry_sdk::DisabledCapabilities::none();
    disabled.streaming = true;
    let updated = h
        .service
        .update_model(
            &ctx(t.root),
            id,
            model_update(|r| {
                r.disabled_capabilities = Some(disabled);
                r.base_url = Some(None);
            }),
        )
        .await
        .unwrap();
    assert!(updated.model.info.capabilities.streaming, "declared stays");
    assert!(updated.model.info.disabled_capabilities.streaming);
    assert_eq!(updated.model.info.base_url, None);
    assert_eq!(updated.model.info.secret_ref.as_deref(), Some("openai-key"));
    assert!(
        h.service
            .list_available_models(&c, &q)
            .await
            .unwrap()
            .items
            .is_empty(),
        "effective capability is filtered"
    );
    for field in [
        model_registry_sdk::odata::MODEL_VISION,
        model_registry_sdk::odata::MODEL_TOOLS,
    ] {
        let q = QueryBuilder::<ModelSchema>::new()
            .filter(field.eq(true))
            .build();
        assert_eq!(
            h.service
                .list_available_models(&c, &q)
                .await
                .unwrap()
                .items
                .len(),
            1
        );
    }
    assert_eq!(
        h.resolve(t.other, "openai::gpt-4o")
            .await
            .unwrap()
            .info
            .base_url,
        None,
        "absent stays absent"
    );
}

trait ClockForTest {
    fn now_for_test(&self) -> chrono::DateTime<chrono::Utc>;
}

impl ClockForTest for ManualClock {
    fn now_for_test(&self) -> chrono::DateTime<chrono::Utc> {
        model_registry::domain::clock::Clock::now(self)
    }
}

// ---------------------------------------------------------------------------
// Versions and preconditions (ADR-0004)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn versions_increment_and_stale_preconditions_change_nothing() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = ctx(t.root);
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let created = h
        .service
        .create_model(
            &c,
            model_req(p, "gpt-4o", SharingMode::Shared, ApprovalState::Approved),
        )
        .await
        .unwrap()
        .model;
    assert_eq!(created.version, 1);

    let renamed = h
        .service
        .update_model(
            &c,
            created.id,
            model_update(|r| {
                r.expected_version = Some(1);
                r.display_name = Some("GPT-4o".into());
            }),
        )
        .await
        .unwrap()
        .model;
    assert_eq!(renamed.version, 2);

    let revoked = h
        .service
        .update_model(
            &c,
            created.id,
            model_update(|r| {
                r.expected_version = Some(2);
                r.approval_state = Some(ApprovalState::Revoked);
            }),
        )
        .await
        .unwrap()
        .model;
    assert_eq!(revoked.version, 3);

    // A stale form cannot re-approve the revoked model.
    let err = h
        .service
        .update_model(
            &c,
            created.id,
            model_update(|r| {
                r.expected_version = Some(2);
                r.approval_state = Some(ApprovalState::Approved);
            }),
        )
        .await
        .unwrap_err();
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
    let current = h.service.get_model(&c, created.id).await.unwrap().model;
    assert_eq!(current.approval.state, ApprovalState::Revoked);
    assert_eq!(current.version, 3);
    assert_eq!(current.info.display_name, "GPT-4o");

    // An explicit unconditional update applies.
    let unconditional = h
        .service
        .update_model(
            &c,
            created.id,
            model_update(|r| r.approval_state = Some(ApprovalState::Approved)),
        )
        .await
        .unwrap()
        .model;
    assert_eq!(unconditional.version, 4);
    assert_eq!(unconditional.approval.state, ApprovalState::Approved);

    // Deletes honour the precondition too.
    assert!(matches!(
        h.service.delete_model(&c, created.id, Some(3)).await,
        Err(DomainError::OptimisticLockFailure { .. })
    ));
    assert!(h.service.get_model(&c, created.id).await.is_ok());
    h.service
        .delete_model(&c, created.id, Some(4))
        .await
        .unwrap();
    assert!(matches!(
        h.service.get_model(&c, created.id).await,
        Err(DomainError::ModelNotFound { .. })
    ));

    // Providers carry a version of their own.
    let provider = h.service.get_provider(&c, p).await.unwrap().provider;
    assert_eq!(provider.version, 1);
    let updated = h
        .service
        .update_provider(
            &c,
            p,
            provider_update(|r| {
                r.expected_version = Some(1);
                r.enabled = Some(false);
            }),
        )
        .await
        .unwrap()
        .provider;
    assert_eq!(updated.version, 2);
    let err = h
        .service
        .update_provider(
            &c,
            p,
            provider_update(|r| {
                r.expected_version = Some(1);
                r.enabled = Some(true);
            }),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::OptimisticLockFailure {
                resource: Resource::Provider,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(
        !h.service
            .get_provider(&c, p)
            .await
            .unwrap()
            .provider
            .enabled,
        "a stale update cannot re-enable a disabled provider"
    );
    assert!(matches!(
        h.service.delete_provider(&c, p, Some(1)).await,
        Err(DomainError::OptimisticLockFailure { .. })
    ));
    h.service.delete_provider(&c, p, Some(2)).await.unwrap();
    assert!(matches!(
        h.service.get_provider(&c, p).await,
        Err(DomainError::ProviderNotFound { .. })
    ));
}

// ---------------------------------------------------------------------------
// Model identity: name vs provider_model_id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn model_name_is_the_identity_and_provider_model_id_is_mutable() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = ctx(t.root);
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;

    // Two models of one provider may share a provider model id.
    let mut eu = model_req(p, "gpt-4o-eu", SharingMode::Shared, ApprovalState::Approved);
    eu.provider_model_id = "gpt-4o".into();
    eu.region = Some("eu-west-1".into());
    eu.model_version = Some("2026-05".into());
    let eu = h.service.create_model(&c, eu).await.unwrap().model;
    assert_eq!(eu.name, "gpt-4o-eu");
    assert_eq!(eu.canonical_id, "openai::gpt-4o-eu");
    assert_eq!(eu.info.provider_model_id, "gpt-4o");
    let us = h
        .service
        .create_model(
            &c,
            model_req(p, "gpt-4o", SharingMode::Shared, ApprovalState::Approved),
        )
        .await
        .unwrap()
        .model;
    assert_eq!(us.canonical_id, "openai::gpt-4o");
    assert_eq!(us.info.provider_model_id, "gpt-4o");

    // The name is unique under the provider.
    let err = h
        .service
        .create_model(
            &c,
            model_req(p, "gpt-4o", SharingMode::Tenant, ApprovalState::Pending),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::ModelAlreadyExists { ref canonical_id } if canonical_id == "openai::gpt-4o")
    );

    // provider_model_id changes; the canonical id does not.
    let moved = h
        .service
        .update_model(
            &c,
            eu.id,
            model_update(|r| r.provider_model_id = Some("gpt-4o-2026-05-01".into())),
        )
        .await
        .unwrap()
        .model;
    assert_eq!(moved.canonical_id, "openai::gpt-4o-eu");
    assert_eq!(moved.info.provider_model_id, "gpt-4o-2026-05-01");
    let resolved = h.resolve(t.other, "openai::gpt-4o-eu").await.unwrap();
    assert_eq!(resolved.info.provider_model_id, "gpt-4o-2026-05-01");
    assert_eq!(resolved.info.model_version.as_deref(), Some("2026-05"));

    // `name` is filterable and orderable; `model_version` is filter-only.
    let o = ctx(t.other);
    let q = QueryBuilder::<ModelSchema>::new()
        .filter(MODEL_NAME.eq("gpt-4o-eu"))
        .build();
    let items = h.service.list_available_models(&o, &q).await.unwrap().items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, eu.id);
    let q = QueryBuilder::<ModelSchema>::new()
        .order_by(MODEL_NAME, SortDir::Desc)
        .build();
    let names: Vec<String> = h
        .service
        .list_available_models(&o, &q)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, vec!["gpt-4o-eu", "gpt-4o"]);
    let q = QueryBuilder::<ModelSchema>::new()
        .filter(MODEL_MODEL_VERSION.eq("2026-05"))
        .build();
    assert_eq!(
        h.service
            .list_available_models(&o, &q)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let q = QueryBuilder::<ModelSchema>::new()
        .order_by(MODEL_MODEL_VERSION, SortDir::Asc)
        .build();
    assert!(matches!(
        h.service.list_available_models(&o, &q).await,
        Err(DomainError::OData(_))
    ));
}

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cost_lives_on_the_model_and_is_replaced_whole() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = ctx(t.root);
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let mut req = model_req(p, "gpt-4o", SharingMode::Shared, ApprovalState::Approved);
    req.cost.input_per_1m_micro = Some(2_500_000);
    req.cost.output_per_1m_micro = Some(10_000_000);
    let created = h.service.create_model(&c, req.clone()).await.unwrap().model;
    assert_eq!(created.info.cost, req.cost);
    assert_eq!(
        h.resolve(t.other, "openai::gpt-4o")
            .await
            .unwrap()
            .info
            .cost,
        req.cost
    );
    let listed = h
        .service
        .list_available_models(&ctx(t.other), &ODataQuery::default())
        .await
        .unwrap()
        .items;
    assert_eq!(listed[0].info.cost, req.cost);

    let updated = h
        .service
        .update_model(
            &c,
            created.id,
            model_update(|r| {
                let mut cost = ModelCostV1::default();
                cost.input_per_1m_micro = Some(1);
                r.cost = Some(cost);
            }),
        )
        .await
        .unwrap()
        .model;
    assert_eq!(updated.info.cost.input_per_1m_micro, Some(1));
    assert_eq!(
        updated.info.cost.output_per_1m_micro, None,
        "the object is replaced whole"
    );
}

// ---------------------------------------------------------------------------
// Credential binding (ADR-0002)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn secret_ref_must_name_an_own_secret() {
    let t = Tenants::new();
    let secrets = StaticSecretRefSource::new()
        .owned("own-key")
        .with_ref("expired-key", OwnRowState::Expired)
        .with_ref("declared-key", OwnRowState::Declared)
        .with_ref("inherited-key", OwnRowState::NotOwned);
    let h = harness_full(
        t.resolver(),
        StaticAuthz::permit_own_tenant(),
        schemas(),
        secrets,
        ModelRegistryConfig::default(),
    )
    .await;
    let c = ctx(t.root);
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;

    let mut req = model_req(p, "own", SharingMode::Shared, ApprovalState::Approved);
    req.secret_ref = Some("own-key".into());
    let own = h.service.create_model(&c, req).await.unwrap().model;
    assert_eq!(h.secrets.calls(), 1);
    let mut req = model_req(p, "expired", SharingMode::Shared, ApprovalState::Approved);
    req.secret_ref = Some("expired-key".into());
    h.service.create_model(&c, req).await.unwrap();
    assert_eq!(h.secrets.calls(), 2, "an expired own secret is accepted");

    for (secret_ref, reason) in [
        ("declared-key", field::Reason::SecretRefNotOwned),
        ("inherited-key", field::Reason::SecretRefNotOwned),
        ("missing-key", field::Reason::SecretRefNotFound),
    ] {
        let mut req = model_req(p, "rejected", SharingMode::Shared, ApprovalState::Approved);
        req.secret_ref = Some(secret_ref.into());
        let err = h.service.create_model(&c, req).await.unwrap_err();
        assert_eq!(
            reasons(&err),
            vec![("secret_ref".to_owned(), reason)],
            "{secret_ref}"
        );
    }
    assert_eq!(
        h.managed_models(t.root).await.len(),
        2,
        "no row is written on rejection"
    );
    assert_eq!(h.secrets.calls(), 5);

    // Updates: a resent or cleared reference makes no call; a new one does.
    let calls = h.secrets.calls();
    h.service
        .update_model(
            &c,
            own.id,
            model_update(|r| {
                r.secret_ref = Some(Some("own-key".into()));
                r.display_name = Some("x".into());
            }),
        )
        .await
        .unwrap();
    let cleared = h
        .service
        .update_model(&c, own.id, model_update(|r| r.secret_ref = Some(None)))
        .await
        .unwrap()
        .model;
    assert_eq!(cleared.info.secret_ref, None);
    assert_eq!(h.secrets.calls(), calls, "resend and clear make no call");
    let err = h
        .service
        .update_model(
            &c,
            own.id,
            model_update(|r| r.secret_ref = Some(Some("inherited-key".into()))),
        )
        .await
        .unwrap_err();
    assert_eq!(
        reasons(&err),
        vec![("secret_ref".to_owned(), field::Reason::SecretRefNotOwned)]
    );
    let rebound = h
        .service
        .update_model(
            &c,
            own.id,
            model_update(|r| r.secret_ref = Some(Some("expired-key".into()))),
        )
        .await
        .unwrap()
        .model;
    assert_eq!(rebound.info.secret_ref.as_deref(), Some("expired-key"));
    assert_eq!(h.secrets.calls(), calls + 2);

    // Reads never call CredStore.
    let calls = h.secrets.calls();
    h.resolve(t.other, "openai::own").await.unwrap();
    h.available(t.other).await;
    h.managed_models(t.root).await;
    assert_eq!(h.secrets.calls(), calls);
    assert_eq!(
        h.secrets.seen().first().map(String::as_str),
        Some("own-key"),
        "the reference is read as sent"
    );

    // A denied or failed CredStore read refuses the write; writes without a
    // reference are unaffected.
    for (source, check) in [
        (
            StaticSecretRefSource::new()
                .failing_with(|| DomainError::not_permitted("credstore.get_record")),
            (|e: &DomainError| matches!(e, DomainError::NotPermitted { .. }))
                as fn(&DomainError) -> bool,
        ),
        (
            StaticSecretRefSource::new()
                .failing_with(|| DomainError::unavailable(Dependency::CredStore, "down")),
            (|e: &DomainError| {
                matches!(
                    e,
                    DomainError::DependencyUnavailable {
                        dependency: Dependency::CredStore,
                        ..
                    }
                )
            }) as fn(&DomainError) -> bool,
        ),
    ] {
        let h = harness_full(
            t.resolver(),
            StaticAuthz::permit_own_tenant(),
            schemas(),
            source,
            ModelRegistryConfig::default(),
        )
        .await;
        let p = h
            .provider(t.root, "openai", SharingMode::Shared, true)
            .await;
        let mut req = model_req(p, "with-ref", SharingMode::Shared, ApprovalState::Approved);
        req.secret_ref = Some("any".into());
        let err = h.service.create_model(&c, req).await.unwrap_err();
        assert!(check(&err), "{err:?}");
        h.service
            .create_model(
                &c,
                model_req(
                    p,
                    "without-ref",
                    SharingMode::Shared,
                    ApprovalState::Approved,
                ),
            )
            .await
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Provider type migration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn provider_type_migration_moves_every_model_or_nothing() {
    let t = Tenants::new();
    let h = harness(t.resolver()).await;
    let c = ctx(t.root);
    let p = h
        .provider(t.root, "openai", SharingMode::Shared, true)
        .await;
    let mut ok = model_req(p, "m1", SharingMode::Shared, ApprovalState::Approved);
    ok.provider_settings = serde_json::json!({ "oagw_alias": "a" });
    let m1 = h.service.create_model(&c, ok).await.unwrap().model;
    let m2 = h
        .service
        .create_model(
            &c,
            model_req(p, "m2", SharingMode::Shared, ApprovalState::Approved),
        )
        .await
        .unwrap()
        .model;
    assert_eq!(m2.info.provider_settings, serde_json::json!({}));

    // m2 fails the strict schema: nothing changes.
    let err = h
        .service
        .update_provider(
            &c,
            p,
            provider_update(|r| r.gts_type = Some(gts::GtsTypeId::new(STRICT))),
        )
        .await
        .unwrap_err();
    let r = reasons(&err);
    assert_eq!(
        r,
        vec![("gts_type".to_owned(), field::Reason::SchemaMismatch)]
    );
    let DomainError::Invalid { violations, .. } = &err else {
        unreachable!()
    };
    assert!(
        violations[0].description.starts_with("openai::m2:"),
        "{}",
        violations[0].description
    );
    let provider = h.service.get_provider(&c, p).await.unwrap().provider;
    assert_eq!(provider.gts_type.as_ref(), OPENAI);
    assert_eq!(provider.version, 1);
    for id in [m1.id, m2.id] {
        let m = h.service.get_model(&c, id).await.unwrap().model;
        assert_eq!(m.info.gts_type.as_ref(), OPENAI);
        assert_eq!(m.version, 1);
    }

    // Unknown or non-provider types are rejected before anything is read.
    for (gts_type, reason) in [
        (
            gts_id!("cf.genai.model.provider.v1~cf.genai._.unknown.v1~"),
            field::Reason::GtsTypeNotRegistered,
        ),
        (
            gts_id!("cf.toolkit.authz.permission.v1~cf.genai._.openai.v1~"),
            field::Reason::NotAProviderType,
        ),
    ] {
        let err = h
            .service
            .update_provider(
                &c,
                p,
                provider_update(|r| r.gts_type = Some(gts::GtsTypeId::new(gts_type))),
            )
            .await
            .unwrap_err();
        assert_eq!(reasons(&err)[0].1, reason, "{gts_type}");
    }

    // Fix m2, then migrate: provider and every model switch together.
    h.service
        .update_model(
            &c,
            m2.id,
            model_update(|r| r.provider_settings = Some(serde_json::json!({ "oagw_alias": "b" }))),
        )
        .await
        .unwrap();
    let migrated = h
        .service
        .update_provider(
            &c,
            p,
            provider_update(|r| {
                r.expected_version = Some(1);
                r.gts_type = Some(gts::GtsTypeId::new(STRICT));
                r.name = Some("Strict OpenAI".into());
            }),
        )
        .await
        .unwrap()
        .provider;
    assert_eq!(migrated.gts_type.as_ref(), STRICT);
    assert_eq!(migrated.version, 2);
    assert_eq!(migrated.name, "Strict OpenAI");
    let m1_after = h.service.get_model(&c, m1.id).await.unwrap().model;
    let m2_after = h.service.get_model(&c, m2.id).await.unwrap().model;
    assert_eq!(m1_after.info.gts_type.as_ref(), STRICT);
    assert_eq!(m2_after.info.gts_type.as_ref(), STRICT);
    assert_eq!((m1_after.version, m2_after.version), (2, 3));
    assert_eq!(m1_after.canonical_id, "openai::m1");
    assert_eq!(m2_after.id, m2.id);
    assert_eq!(
        h.resolve(t.other, "openai::m2")
            .await
            .unwrap()
            .info
            .gts_type
            .as_ref(),
        STRICT
    );

    // Resending the stored type is a plain update; a stale version fails.
    let plain = h
        .service
        .update_provider(
            &c,
            p,
            provider_update(|r| {
                r.gts_type = Some(gts::GtsTypeId::new(STRICT));
                r.managed = Some(true);
            }),
        )
        .await
        .unwrap()
        .provider;
    assert_eq!(plain.version, 3);
    assert_eq!(
        h.service.get_model(&c, m1.id).await.unwrap().model.version,
        2,
        "no model is touched by a plain update"
    );
    assert!(matches!(
        h.service
            .update_provider(
                &c,
                p,
                provider_update(|r| {
                    r.expected_version = Some(1);
                    r.gts_type = Some(gts::GtsTypeId::new(OPENAI));
                }),
            )
            .await,
        Err(DomainError::OptimisticLockFailure { .. })
    ));
    assert_eq!(
        h.service
            .get_provider(&c, p)
            .await
            .unwrap()
            .provider
            .gts_type
            .as_ref(),
        STRICT
    );
}
