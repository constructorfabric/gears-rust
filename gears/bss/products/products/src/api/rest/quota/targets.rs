//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-intake:p1
//! The adapter target endpoints (P-D-268, P-D-273): bind a discovery plugin instance and a target
//! key in the caller's tenant, list the tenant's targets, and read one.
//!
//! A create asks `author` on `quota_adapter_target`, anchored to the caller's tenant, before it
//! reads the body; the reads ask `read`. The plugin instance resolves through types-registry as the
//! caller, before the write's transaction, which writes the target, its audit row and the replay
//! receipt (P-D-198).
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use axum::extract::{Path, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{Extension, Json, Router};
use bss_products_sdk::quota::gts::DISCOVERY_PLUGIN_TYPE;
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::odata::OData;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit_db::secure::{AccessScope, DBRunner, TxConfig};
use toolkit_odata::{Error as ODataError, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{ProductsQuotaAdapterTarget, ProductsQuotaAdapterTargetRequest};
use super::{On, QuotaState, TARGETS, registry_unavailable};
use crate::api::rest::sku_list::{RawQuery, cursor_hash, params};
use crate::api::rest::{TxError, contention_db_err, replay, require_authenticated};
use crate::authz::{access_scope, actions, labels, resource_types};
use crate::domain::error::DomainError;
use crate::domain::quota::{NewQuotaTarget, QuotaTarget};
use crate::domain::validation::ValidationReport;
use crate::infra::storage::repo::quota_intake_repo as store;
use crate::infra::storage::repo::{self, AuditCommon, LifecycleMove};
use crate::infra::storage::{RepoError, RepoRefusal, stored_now};

/// The audit action of a target create.
pub const ACTION_CREATE: &str = "quota_adapter_target.create";
/// The audit subject kind of a target.
pub const SUBJECT_TARGET: &str = "quota_adapter_target";
/// The plugin instance is not one types-registry holds under the discovery specification.
pub const QUOTA_TARGET_PLUGIN_UNKNOWN: &str = "QUOTA_TARGET_PLUGIN_UNKNOWN";
/// The tenant has bound this plugin instance and target key.
pub const QUOTA_TARGET_TAKEN: &str = "QUOTA_TARGET_TAKEN";
/// The most characters a target's name holds (P-D-225).
const MAX_NAME_CHARS: usize = 200;
/// The most bytes a target key holds (P-D-225).
const MAX_KEY_BYTES: usize = 255;
pub(super) const TAG: &str = "Quota catalog";

/// Register the target operations.
pub(super) fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post(TARGETS)
        .operation_id("bss_products.create_quota_adapter_target")
        .summary("Bind a quota discovery adapter target")
        .description(
            "Binds, in the caller's tenant, one discovery plugin instance and the adapter's opaque \
             target key (P-D-268). Asks `author` on `quota_adapter_target`. Refusals: 400 \
             VALIDATION on a blank name or an empty key or one outside printable ASCII, \
             FIELD_TOO_LONG on a name over 200 characters or a key over 255 bytes, \
             QUOTA_TARGET_PLUGIN_UNKNOWN on `plugin_instance_id` when types-registry holds no \
             instance of the quota discovery plugin specification under it; 409 \
             QUOTA_TARGET_TAKEN when the tenant has bound the plugin instance and key, \
             IDEMPOTENCY_CONFLICT, IDEMPOTENCY_KEY_IN_FLIGHT; 503 QUOTA_REGISTRY_UNAVAILABLE \
             when types-registry does not answer.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<ProductsQuotaAdapterTargetRequest>(
            openapi,
            "plugin_instance_id, target_key, name",
        )
        .param(replay::param())
        .handler(create_target)
        .json_response_with_schema::<ProductsQuotaAdapterTarget>(
            openapi,
            StatusCode::CREATED,
            "The target.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get(TARGETS)
        .operation_id("bss_products.list_quota_adapter_targets")
        .summary("List quota discovery adapter targets")
        .description(
            "One page of the caller's tenant's targets by name, then id. `$top` (alias `limit`; \
             default 50, at most 200) and `cursor` (alias `$skiptoken`) from `page_info`; the \
             cursor binds the caller's tenant, so another tenant's reuse of it is 400. Asks \
             `read` on `quota_adapter_target`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "limit",
            false,
            "Page size, alias of $top (default 50, at most 200)",
            "integer",
        )
        .query_param_typed(
            "cursor",
            false,
            "Continuation from page_info (alias $skiptoken)",
            "string",
        )
        .handler(list_targets)
        .json_response_with_schema::<Page<ProductsQuotaAdapterTarget>>(
            openapi,
            StatusCode::OK,
            "One page of targets.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::get(format!("{TARGETS}/{{id}}"))
        .operation_id("bss_products.get_quota_adapter_target")
        .summary("Read a quota discovery adapter target")
        .description(
            "One target of the caller's tenant; 404 for an unknown or foreign one. Asks `read` on \
             `quota_adapter_target`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "The target's id")
        .handler(get_target)
        .json_response_with_schema::<ProductsQuotaAdapterTarget>(
            openapi,
            StatusCode::OK,
            "The target.",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// `action` on `quota_adapter_target`, anchored to the caller's tenant for a write.
pub(super) async fn target_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    action: &str,
) -> Result<AccessScope, CanonicalError> {
    let anchor = (action != actions::READ).then(|| ctx.subject_tenant_id());
    access_scope(
        enforcer,
        ctx,
        &resource_types::QUOTA_ADAPTER_TARGET,
        action,
        anchor,
    )
    .await
    .map_err(|e| {
        crate::api::rest::authz_error_to_canonical(e, |reason| {
            crate::infra::error_mapping::permission_denied(labels::QUOTA_ADAPTER_TARGET, reason)
        })
    })
}

/// The 404 of a target the tenant does not hold, or of a path that is not a UUID.
pub(super) fn target_not_found(id: Uuid) -> CanonicalError {
    DomainError::NotFound {
        what: "quota_adapter_target",
        id,
    }
    .into()
}

/// The tenant's target `id` under `scope`, or its 404; a stored target that does not read is
/// refused on the asking endpoint's resource (`on`).
pub(super) async fn load_target(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    path: &str,
    on: On,
) -> Result<QuotaTarget, CanonicalError> {
    let Ok(id) = Uuid::parse_str(path) else {
        return Err(target_not_found(Uuid::nil()));
    };
    store::find_target(runner, scope, tenant, id)
        .await
        .map_err(|e| on.stored(&e))?
        .ok_or_else(|| target_not_found(id))
}

/// The body's caps (P-D-225): a name of 1 to 200 characters, not blank, and a key of 1 to 255
/// printable ASCII bytes.
fn checked(body: ProductsQuotaAdapterTargetRequest) -> Result<NewQuotaTarget, DomainError> {
    let mut report = ValidationReport::new();
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        report.violate("VALIDATION", "name", "the name is blank");
    } else if name.chars().count() > MAX_NAME_CHARS {
        report.violate(
            "FIELD_TOO_LONG",
            "name",
            format!("at most {MAX_NAME_CHARS} characters"),
        );
    }
    let key = body.target_key;
    if key.is_empty() {
        report.violate("VALIDATION", "target_key", "the target key is empty");
    } else if key.len() > MAX_KEY_BYTES {
        report.violate(
            "FIELD_TOO_LONG",
            "target_key",
            format!("at most {MAX_KEY_BYTES} bytes"),
        );
    } else if !key.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        report.violate(
            "VALIDATION",
            "target_key",
            "the target key is printable ASCII only",
        );
    }
    if !report.is_empty() {
        return Err(DomainError::Validation(report));
    }
    Ok(NewQuotaTarget {
        plugin_instance_id: body.plugin_instance_id,
        target_key: key,
        name,
    })
}

/// The plugin instance is one types-registry holds under the discovery plugin specification, read
/// as the caller.
async fn check_plugin(
    state: &QuotaState,
    ctx: &SecurityContext,
    plugin_instance_id: &str,
) -> Result<(), CanonicalError> {
    let instance = state
        .registry
        .instance(ctx, plugin_instance_id)
        .await
        .map_err(|e| registry_unavailable(&e.0))?;
    if instance.is_some_and(|i| i.type_id == DISCOVERY_PLUGIN_TYPE) {
        return Ok(());
    }
    let mut report = ValidationReport::new();
    report.violate(
        QUOTA_TARGET_PLUGIN_UNKNOWN,
        "plugin_instance_id",
        format!("types-registry holds no instance of {DISCOVERY_PLUGIN_TYPE} under this id"),
    );
    Err(On::TARGET.refuse(DomainError::Validation(report)))
}

/// The target's audit row, in the create's transaction.
async fn audit(
    tx: &impl DBRunner,
    scope: &AccessScope,
    target: &QuotaTarget,
) -> Result<(), TxError> {
    repo::write_eventless_act_audit(
        tx,
        scope,
        AuditCommon {
            audit_id: Uuid::now_v7(),
            tenant_id: target.tenant_id,
            actor_ref: target.created_by,
            action: ACTION_CREATE.to_owned(),
            subject_kind: SUBJECT_TARGET.to_owned(),
            reason: None,
            correlation_id: None,
            written_at: target.created_at,
            lifecycle: LifecycleMove::NONE,
        },
        target.id,
        None,
    )
    .await
    .map_err(TxError::Repo)
}

// @cpt-flow:cpt-cf-bss-products-flow-quota-catalog-intake:p1
async fn create_target(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-intake:p1:inst-quota-target
    let tenant = ctx.subject_tenant_id();
    let actor = ctx.subject_id();
    let scope = target_scope(&enforcer, &ctx, actions::AUTHOR).await?;
    let payload = On::TARGET.body(body)?;
    let claim = On::TARGET.input(&state.api, &headers, TARGETS.into(), &payload)?;
    if let Some(response) = On::TARGET
        .replayed(&state.api, tenant, claim.as_ref())
        .await?
    {
        return Ok(response);
    }
    let body: ProductsQuotaAdapterTargetRequest = On::TARGET.read(payload)?;
    let new = checked(body).map_err(|e| On::TARGET.refuse(e))?;
    check_plugin(&state, &ctx, &new.plugin_instance_id).await?;
    let now = stored_now();
    state
        .api
        .db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            TxConfig::default(),
            contention_db_err,
            move |tx| {
                let (scope, new, claim) = (scope.clone(), new.clone(), claim.clone());
                Box::pin(async move {
                    if let Some(response) = replay::begin(tx, tenant, claim.as_ref()).await? {
                        return Ok(response);
                    }
                    let target = store::insert_target(tx, &scope, tenant, new, actor, now)
                        .await
                        .map_err(|e| match e {
                            RepoError::Refused(RepoRefusal::QuotaTargetTaken) => {
                                TxError::Refused(DomainError::Conflict {
                                    code: QUOTA_TARGET_TAKEN,
                                    detail: "the tenant has bound this plugin instance and \
                                             target key"
                                        .into(),
                                })
                            }
                            other => TxError::Repo(other),
                        })?;
                    audit(tx, &scope, &target).await?;
                    replay::finish(
                        tx,
                        tenant,
                        claim.as_ref(),
                        StatusCode::CREATED,
                        &ProductsQuotaAdapterTarget::from(target),
                    )
                    .await
                })
            },
        )
        .await
        .map_err(|e| On::TARGET.tx(e))
    // @cpt-end:cpt-cf-bss-products-flow-quota-catalog-intake:p1:inst-quota-target
}

async fn list_targets(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    query: RawQuery,
    odata: Result<OData, CanonicalError>,
) -> Result<Json<Page<ProductsQuotaAdapterTarget>>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = target_scope(&enforcer, &ctx, actions::READ).await?;
    params(query, &["limit", "cursor"], Some(&["$top", "$skiptoken"]))?;
    let OData(mut odata) = odata?;
    let tenant = ctx.subject_tenant_id();
    // The cursor names this list and the caller's tenant: another list's, or another tenant's,
    // is refused, not misread.
    let hash = cursor_hash(&serde_json::json!({
        "quota_adapter_targets": {"tenant": tenant.to_string()}
    }));
    if let Some(cursor) = &odata.cursor
        && cursor.f.as_deref() != Some(hash.as_str())
    {
        return Err(ODataError::FilterMismatch.into());
    }
    odata.filter_hash = Some(hash);
    let conn = On::TARGET.conn(&state.api)?;
    let page = store::list_targets(&conn, &scope, tenant, &odata)
        .await
        .map_err(|e| On::TARGET.list(e))?;
    let mut page = Page {
        items: page
            .items
            .into_iter()
            .map(ProductsQuotaAdapterTarget::from)
            .collect::<Vec<_>>(),
        page_info: page.page_info,
    };
    // P-D-262: the page's creators in one lookup.
    state.api.actor_names.fill(&ctx, &mut page.items).await;
    Ok(Json(page))
}

async fn get_target(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<String>,
) -> Result<Json<ProductsQuotaAdapterTarget>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = target_scope(&enforcer, &ctx, actions::READ).await?;
    let conn = On::TARGET.conn(&state.api)?;
    let target = load_target(&conn, &scope, ctx.subject_tenant_id(), &id, On::TARGET).await?;
    let mut body = ProductsQuotaAdapterTarget::from(target);
    state.api.actor_names.fill(&ctx, &mut body).await;
    Ok(Json(body))
}
