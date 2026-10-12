//! Pricing gear lifecycle, database capability and reserved REST prefix.

use crate::config::BssPricingConfig;
use anyhow::{Context, Result};
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use axum::Router;
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::config::ConfigError;
use toolkit::contracts::{DatabaseCapability, RestApiCapability};
use toolkit::{Gear, GearCtx};

struct PricingRuntime {
    enforcer: Arc<authz_resolver_sdk::PolicyEnforcer>,
    state: Arc<crate::api::rest::authoring::AuthoringState>,
}

#[toolkit::gear(name = "bss-pricing", capabilities = [db, rest, stateful], deps = [types_registry, authz_resolver], lifecycle(entry = "serve", stop_timeout = "30s"))]
pub struct BssPricingGear {
    runtime: ArcSwapOption<PricingRuntime>,
}

impl Default for BssPricingGear {
    fn default() -> Self {
        Self {
            runtime: ArcSwapOption::from(None),
        }
    }
}

impl BssPricingGear {
    /// Spawn the reference recovery task and cancel in-flight work on shutdown. Its ticker runs
    /// every second: the plan switch duty first, on the first tick and every
    /// `reference_ticker::SWITCH_EVERY` (60) after it (D-450), then at most 100 due reference ops,
    /// and a reconciliation every 10 ticks. The knobs are fixed here; the ticker has no config key.
    /// The outbox pipeline is stopped however the task ends, a panic included; the task's
    /// failure is returned after that (PS-36).
    pub(crate) async fn serve(self: Arc<Self>, cancel: CancellationToken) -> Result<()> {
        let Some(runtime) = self.runtime.load_full() else {
            cancel.cancelled().await;
            return Ok(());
        };
        let child = cancel.child_token();
        let state = runtime.state.clone();
        let task = tokio::spawn(async move {
            let mut ticker = crate::infra::reference_ticker::Ticker::new(
                state,
                Arc::new(crate::infra::reference_work::WallClock),
                100,
                10,
            );
            let mut interval = tick_interval();
            loop {
                tokio::select! { biased;
                    () = child.cancelled() => break,
                    _ = interval.tick() => {
                        tokio::select! { biased;
                            () = child.cancelled() => break,
                            result = ticker.tick() => if let Err(error) = result { tracing::warn!(error=%error, diagnostic=error.diagnostic().unwrap_or_default(), "pricing reference ticker failed"); }
                        }
                    }
                }
            }
        });
        let ended = task.await;
        runtime.state.stop().await;
        ended.context("pricing reference ticker stopped unexpectedly")
    }
}

/// The ticker's one-second interval. A tick that overruns (a slow Products under a hundred
/// drives) delays the next by a full second: the missed ticks are not fired back to back, which
/// would repeat the scans and the Products calls while Products is slow (PS-12).
pub(crate) fn tick_interval() -> tokio::time::Interval {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

#[cfg(test)]
#[path = "module_tests.rs"]
mod module_tests;

#[async_trait]
impl Gear for BssPricingGear {
    async fn init(&self, ctx: &GearCtx) -> Result<()> {
        let config = match ctx.config::<BssPricingConfig>() {
            Ok(config) => config,
            Err(ConfigError::MissingConfigSection { .. }) => BssPricingConfig::default(),
            Err(ConfigError::GearNotFound { .. }) => return Ok(()),
            Err(error) => return Err(error).context("bss-pricing: invalid config"),
        };
        let db = ctx
            .db_required()
            .context("bss-pricing: database is required")?;
        let authz_client = match ctx
            .client_hub()
            .get::<dyn authz_resolver_sdk::AuthZResolverApi>()
        {
            Ok(client) => client,
            Err(hub) => {
                return Err(toolkit_canonical_errors::CanonicalError::from(
                    crate::infra::commercial_terms::errors::UnconfiguredDependency {
                        dependency: "AuthZResolverApi",
                    },
                ))
                .context(format!(
                    "bss-pricing: AuthZResolverApi absent from ClientHub ({hub}); \
                     authz-resolver module must be registered"
                ));
            }
        };
        let enforcer = Arc::new(authz_resolver_sdk::PolicyEnforcer::new(authz_client));

        // Register the authz-label stub schemas so RBAC role definitions
        // targeting the catalog labels pass target-type validation. Mandatory:
        // without them no custom catalog role can be defined, and the labels
        // deliberately sit outside `gts.cf.resources.*` where no built-in role
        // would cover them either — a silent skip would leave the whole
        // authoring surface ungrantable.
        let registry = ctx
            .client_hub()
            .get::<dyn types_registry_sdk::TypesRegistryClient>()
            .context(
                "bss-pricing: TypesRegistryClient absent from ClientHub; \
                 types-registry module must be registered",
            )?;
        let results = registry
            .register(crate::authz::authz_label_type_schemas())
            .await
            .context("bss-pricing: register authz label schemas")?;
        for result in results {
            if let types_registry_sdk::RegisterResult::Err { gts_id, error } = result {
                anyhow::bail!(
                    "bss-pricing: failed to register authz label {}: {error}",
                    gts_id.as_deref().unwrap_or("?")
                );
            }
        }

        let state =
            Arc::new(crate::api::rest::authoring::AuthoringState::new(db, ctx.client_hub()).await?);
        // D-428, P-D-197: Products' SKU reads carry pricing's usage through this port, which
        // Products resolves at each read (the two gears boot in either order).
        ctx.client_hub()
            .register::<dyn bss_products_sdk::sku_usage::SkuUsageV1>(Arc::new(
                crate::api::sku_usage::PricingSkuUsage::new(state.clone(), (*enforcer).clone()),
            ));
        ctx.client_hub()
            .register::<dyn bss_pricing_sdk::read::PricingReadV1>(Arc::new(
                crate::api::pricing_read::PricingReadProvider::new(state.clone(), enforcer.clone()),
            ));
        let commercial = Arc::new(crate::infra::commercial_terms::CommercialTermsService::new(
            state.clone(),
            enforcer.clone(),
            Arc::new(crate::infra::clock::WallClock),
            config.seller_hold_policy,
        ));
        ctx.client_hub()
            .register::<dyn bss_pricing_sdk::acceptance::SellabilityV1>(Arc::new(
                crate::api::sellability::SellabilityProvider::new(commercial.clone()),
            ));
        ctx.client_hub()
            .register::<dyn bss_pricing_sdk::acceptance::PricingAcceptanceV1>(Arc::new(
                crate::api::pricing_acceptance::PricingAcceptanceProvider::new(commercial),
            ));
        // D-490: the approvals inbox reads and votes on pricing's units through this source, as
        // the caller, under the gear's own doors.
        ctx.client_hub()
            .register_scoped::<dyn bss_approvals_sdk::ApprovalSourceV1>(
                toolkit::client_hub::ClientScope::new(
                    crate::api::rest::authoring::inbox_source::SOURCE,
                ),
                Arc::new(
                    crate::api::rest::authoring::inbox_source::PricingApprovalSource::new(
                        state.clone(),
                        (*enforcer).clone(),
                    ),
                ),
            );
        self.runtime
            .store(Some(Arc::new(PricingRuntime { enforcer, state })));
        Ok(())
    }
}

impl DatabaseCapability for BssPricingGear {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        let mut migrations = crate::infra::storage::migrations::Migrator::migrations();
        match toolkit_db::outbox::outbox_migrations_with_prefix(
            crate::infra::events::OUTBOX_TABLE_PREFIX,
        ) {
            Ok(outbox) => migrations.extend(outbox),
            Err(error) => migrations.push(Box::new(InvalidOutboxMigration(error.to_string()))),
        }
        migrations.extend(event_broker_sdk::producer_registration_migrations());
        migrations
    }
}

impl RestApiCapability for BssPricingGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<Router> {
        let inner = Router::new();
        let inner = if let Some(runtime) = self.runtime.load_full() {
            let layered = inner
                .merge(crate::api::rest::authoring::router(
                    runtime.state.clone(),
                    openapi,
                ))
                .merge(crate::api::rest::read_contract::router(
                    runtime.state.clone(),
                    openapi,
                ));
            crate::api::rest::authoring::with_caller_layers(layered, (*runtime.enforcer).clone())
        } else {
            inner
        };
        Ok(router.merge(inner))
    }
}

// The capability cannot return Result. Preserve a prefix error as a failing migration
// rather than panicking or silently omitting delivery tables.
struct InvalidOutboxMigration(String);
impl sea_orm_migration::MigrationName for InvalidOutboxMigration {
    fn name(&self) -> &'static str {
        "invalid_pricing_outbox_prefix"
    }
}
#[async_trait]
impl MigrationTrait for InvalidOutboxMigration {
    async fn up(&self, _: &sea_orm_migration::SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Migration(self.0.clone()))
    }
    async fn down(&self, _: &sea_orm_migration::SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Migration(self.0.clone()))
    }
}

// Run-3 route contract: method | path | resource:action | If-Match | Idempotency-Key
// POST /price-books price_book:author false true
// GET /price-books price_book:read false false
// GET /price-books/{id} price_book:read false false
// PATCH /price-books/{id} price_book:author true false
// GET /price-books/{id}/entries price_book_entry:read false false
// GET /price-books/{id}/export price_book:read false false
// GET /settings config:read false false
// PUT /settings config:settings true false
// GET /dimension-keys config:read false false
// PUT /dimension-keys config:settings true false

// POST /price-books/{id}/entries price_book_entry:author false true
// GET /price-book-entries/{id} price_book_entry:read false false
// PATCH /price-book-entries/{id} price_book_entry:author true false
// DELETE /price-book-entries/{id} price_book_entry:author false false

// GET /reference-ops config:settings false false

// Run-4 prices: method | path | resource:action | If-Match | Idempotency-Key
// POST /price-book-entries/{id}/prices price:author false true
// PATCH /prices/{id} price:author true false
// DELETE /prices/{id} price:author true false
// POST /prices/{id}/cancel price:author false true
// POST /prices/{id}/end price:author false true

// Run-4 approvals: method | path | resource:action | If-Match | Idempotency-Key
// POST /prices/{id}/submit price:submit false true
// GET /price-books/{id}/publish-changes price_book:read false false
// POST /price-books/{id}/publish-changes price_book:submit false true
// GET /approval-units approval_unit:read false false
// GET /approval-units/{id} approval_unit:read false false
// POST /approval-units/{id}/approve approval_unit:approve false true
// POST /approval-units/{id}/reject approval_unit:approve false true
// POST /approval-units/{id}/withdraw approval_unit:submit false true
// GET /approval-policy config:read false false
// PUT /approval-policy config:settings true false

// Run 3.3 plans: method | path | resource:action | If-Match | Idempotency-Key
// POST /plans plan:author (then price_book:read, D-456) false true
// GET /plans plan:read false false
// GET /plans/{id} plan:read false false
// PATCH /plans/{id} plan:author true false
// POST /plans/{id}/revisions plan:author false true
// GET /plan-revisions/{id} plan:read (then price_book:read for the sale-date price, D-480) false false
// PATCH /plan-revisions/{id} plan:author (then price_book:read when it names a book, D-456) true false
// DELETE /plan-revisions/{id} plan:author false false

// Run 3.3 items and checks: method | path | resource:action | If-Match | Idempotency-Key
// POST /plan-revisions/{id}/items plan:author false true
// PATCH /plan-items/{id} plan:author true false
// DELETE /plan-items/{id} plan:author false false
// GET /plan-revisions/checks plan:read false false
// GET /plan-revisions/{id}/checks plan:read false false

// Run 3.4 plan approvals: method | path | resource:action | If-Match | Idempotency-Key
// POST /plan-revisions/{id}/submit plan:submit false true
// POST /plans/{id}/clone plan:author (then price_book:read, D-456) false true

// Run 4.3 read contract: method | path | resource:action | If-Match | Idempotency-Key
// GET /resolve plan:read false false
// GET /prices/{id} price:read false false

// Run 9.6 (D-480, D-481): method | path | resource:action | If-Match | Idempotency-Key
// GET /plan-revisions/{id}/reservations plan:read false false
// GET /approval-policy/{kind}/effective price_book_entry:read for prices, plan:read for plan_revision false false
