// Created: 2026-09-07 by Constructor Tech
//! Handlers for the administrative read surface over effective values.

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::header;
use axum::{Extension, Json};
use serde::Deserialize;
use settings_service_sdk::SettingKey;
use settings_service_sdk::odata::SettingFilterField;
use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_odata::ODataQuery;
use toolkit_odata::ast::{CompareOperator, Expr, Value as ODataValue};
use toolkit_odata::filter::convert_expr_to_filter_node;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::api::authz::{self, resource};
use crate::api::rest::setting_dto::{
    AuditRecordDto, EffectiveValueDto, SettingItemDto, render, render_flagged, render_record,
};
use crate::domain::category::visibility;
use crate::domain::error::DomainError;
use crate::domain::resolution::ScopeTarget;
use crate::field;
use crate::gear::ConcreteResolver;

const READ: &str = "read";
/// The entitlement that unmasks `pii` values on the administrative surface.
const READ_UNMASKED: &str = "read_unmasked";

/// The optional `tenant` query parameter: the target scope as a tenant id.
#[derive(Debug, Default, Deserialize)]
pub struct TenantParam {
    /// Omitted, the caller's own tenant — for a platform administrator the
    /// root, and therefore platform scope.
    #[serde(default)]
    pub tenant: Option<String>,
}

fn parse_tenant(raw: Option<&str>) -> Result<Option<Uuid>, DomainError> {
    match raw {
        None | Some("") => Ok(None),
        Some(raw) => Uuid::parse_str(raw)
            .map(Some)
            .map_err(|_| DomainError::Validation {
                field: "tenant".to_owned(),
                code: field::TENANT_PARAM,
                message: format!("`{raw}` is not a tenant id"),
            }),
    }
}

fn parse_key(raw: &str) -> Result<SettingKey, DomainError> {
    SettingKey::parse(raw).map_err(|e| DomainError::Validation {
        field: "key".to_owned(),
        code: field::VALIDATION,
        message: e.to_string(),
    })
}

fn conn_error(err: &toolkit_db::DbError) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

/// The target of an administrative read, gated to the caller's subtree.
///
/// The requested tenant, or the caller's own; it must be the caller itself or
/// a descendant that is not standalone. Anything else is a denial: an
/// administrator above a standalone tenant may not read its values, and a
/// sibling or an ancestor is not the caller's to read at all.
async fn gate_target(
    resolver: &ConcreteResolver,
    ctx: &SecurityContext,
    requested: Option<Uuid>,
) -> Result<ScopeTarget, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-1
    let root = resolver.root_tenant().await?;
    let caller = ctx.subject_tenant_id();
    let target = requested.unwrap_or(caller);
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-4
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-4
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-2
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-3
    if target != caller {
        let hierarchy = resolver.hierarchy();
        let within = hierarchy.is_within_subtree(caller, target).await?;
        if !within || hierarchy.is_standalone(target).await? {
            return Err(DomainError::Unauthorized {
                resource: settings_service_sdk::gts::VALUE_SCHEMA,
            });
        }
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-3
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-2
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-4
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-4
    Ok(if target == root {
        ScopeTarget::Platform
    } else {
        ScopeTarget::Tenant(target)
    })
}

/// Whether the caller may see `pii` values unmasked: a separate decision on
/// the value resource, asked only when a `pii` value is on the page.
async fn may_read_pii(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &SecurityContext,
) -> bool {
    authz::access_scope(enforcer, ctx, &resource::VALUE, READ_UNMASKED, None)
        .await
        .is_ok()
}

// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-rest-read-surface:p1
/// `GET /settings-service/v1/settings/{key}?tenant={tenant_id}`
///
/// # Errors
/// 400 on a malformed key or `tenant`; 403 when the caller may not read
/// values or the target is outside its subtree or standalone; 404 when no
/// declaration exists at the key or it is outside the caller's administrative
/// domain; 410 when the declaration is retired; 503 when a dependency of the
/// walk cannot answer.
pub async fn get_setting(
    Extension(ctx): Extension<SecurityContext>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-1
    let key = parse_key(&key)?;
    let requested = parse_tenant(params.tenant.as_deref())?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-2
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-3
    // A denied or unobtainable decision is a denial: `access_scope` maps both
    // to the unauthorized outcome, which answers 403.
    let scope = authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-3
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-2
    let target = gate_target(&resolver, &ctx, requested).await?;
    let conn = db.conn().map_err(|e| conn_error(&e))?;
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-6
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-7
    // The resolver answers not-found, the distinct retired outcome, or the
    // value — cache first, with the trail recorded on the way.
    let effective = resolver.resolve(&conn, &key, target).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-7
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-6
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-5
    // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-3
    // Absent, never forbidden: a declaration outside the caller's
    // administrative domain, and a setting whose effective access for the
    // target tenant is `hidden`, both answer 404 so existence is not disclosed.
    // For a `global` setting this is the visibility rule that gates whether a
    // tenant is served the platform value at all.
    // Whose access decides is the caller's: an administrator above a tenant it
    // restricted still sees the setting; the restricted tenant does not.
    let caller =
        ScopeTarget::Tenant(ctx.subject_tenant_id()).normalize(resolver.root_tenant().await?);
    let hidden = resolver
        .effective_access(&conn, effective.declaration_id, caller)
        .await?
        .is_hidden();
    if hidden
        || !visibility::is_visible(
            &visibility::domain_visibility(&scope),
            effective.domain_affinity.as_deref(),
        )
    {
        return Err(DomainError::NotFound {
            resource: "declaration",
        }
        .into());
    }
    // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-3
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-5
    let pii = effective.data_classification == "pii" && may_read_pii(&enforcer, &ctx).await;
    let dto: EffectiveValueDto = render(&effective, pii);
    let etag = dto.etag.clone();
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-9
    Ok(([(header::ETAG, etag)], Json(dto)))
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-9
}

/// What a browse filter asks for, once interpreted.
#[derive(Debug, Default)]
struct BrowseFilter {
    /// `needs_review eq true`: list flagged rows instead of resolving.
    needs_review: bool,
    /// `key in (…)` or `key eq …`: the named key set, for per-key outcomes.
    keys: Option<Vec<String>>,
    /// The remainder, which selects declarations: `category_id`, `key`.
    declarations: Option<Expr>,
}

fn unsupported(message: impl Into<String>) -> DomainError {
    DomainError::Validation {
        field: "$filter".to_owned(),
        code: field::ODATA_QUERY,
        message: message.into(),
    }
}

/// Interpret the browse filter.
///
/// Every field and operator is checked against the declared surface first, so
/// an unmapped field or an unsupported operator is refused rather than
/// ignored; then the expression is split into what selects declarations and
/// the `needs_review` switch.
fn interpret(filter: Option<&Expr>) -> Result<BrowseFilter, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-5
    let Some(expr) = filter else {
        return Ok(BrowseFilter::default());
    };
    convert_expr_to_filter_node::<SettingFilterField>(expr)
        .map_err(|e| unsupported(e.to_string()))?;
    let mut out = BrowseFilter::default();
    out.declarations = split(expr, &mut out)?;
    Ok(out)
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-5
}

/// Split one conjunction: returns the declaration-selecting remainder.
fn split(expr: &Expr, out: &mut BrowseFilter) -> Result<Option<Expr>, DomainError> {
    match expr {
        Expr::And(left, right) => {
            let left = split(left, out)?;
            let right = split(right, out)?;
            Ok(match (left, right) {
                (Some(l), Some(r)) => Some(Expr::And(Box::new(l), Box::new(r))),
                (Some(one), None) | (None, Some(one)) => Some(one),
                (None, None) => None,
            })
        }
        Expr::Compare(left, CompareOperator::Eq, right) => match (&**left, &**right) {
            (Expr::Identifier(name), ODataValueExpr(ODataValue::Bool(flag)))
                if name.eq_ignore_ascii_case("needs_review") =>
            {
                if !flag {
                    return Err(unsupported(
                        "`needs_review eq false` is not a listing; omit the filter to browse",
                    ));
                }
                out.needs_review = true;
                Ok(None)
            }
            (Expr::Identifier(name), ODataValueExpr(ODataValue::String(key)))
                if name.eq_ignore_ascii_case("key") =>
            {
                out.keys = Some(vec![key.clone()]);
                Ok(Some(expr.clone()))
            }
            (Expr::Identifier(name), ODataValueExpr(ODataValue::Uuid(_)))
                if name.eq_ignore_ascii_case("category_id") =>
            {
                Ok(Some(expr.clone()))
            }
            _ => Err(unsupported(
                "only `category_id eq`, `key eq`, `key in (...)` and `needs_review eq true` \
                 are supported, joined by `and`",
            )),
        },
        Expr::In(left, values) => match &**left {
            Expr::Identifier(name) if name.eq_ignore_ascii_case("key") => {
                let keys = values
                    .iter()
                    .filter_map(|v| match v {
                        ODataValueExpr(ODataValue::String(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                out.keys = Some(keys);
                Ok(Some(expr.clone()))
            }
            _ => Err(unsupported("`in` is supported on `key` only")),
        },
        _ => Err(unsupported(
            "only `category_id eq`, `key eq`, `key in (...)` and `needs_review eq true` \
             are supported, joined by `and`",
        )),
    }
}

use Expr::Value as ODataValueExpr;

/// `GET /settings-service/v1/settings?tenant={tenant_id}` with `OData`
/// `$filter`, `$orderby` and cursor pagination.
///
/// # Errors
/// 400 on a malformed `tenant`, an unmapped field or an unsupported operator;
/// 403 when the caller may not read values or the target is outside its
/// subtree or standalone; 503 when a dependency cannot answer. A key that
/// fails on its own is reported in its entry, never as a request failure.
#[allow(clippy::too_many_lines)]
pub async fn browse_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Query(params): Query<TenantParam>,
    OData(query): OData,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-1
    let requested = parse_tenant(params.tenant.as_deref())?;
    crate::domain::odata::reject_unsupported_options(&query, "settings")?;
    let filter = interpret(query.filter.as_deref())?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-2
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-3
    // One decision on the value resource for the whole page. Its constraints
    // are the narrowed grant: pushed into the declarations query as the
    // secure scope, so a setting the caller may not read is absent from the
    // page and from the count and the page still comes back full — the
    // candidate batch, its evaluation and the refill happen in the query.
    let scope: AccessScope =
        authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-3
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-2
    let target = gate_target(&resolver, &ctx, requested).await?;
    let root = resolver.root_tenant().await?;
    let conn = db.conn().map_err(|e| conn_error(&e))?;

    let declarations_query = ODataQuery {
        filter: filter.declarations.clone().map(Box::new),
        order: query.order.clone(),
        limit: query.limit,
        cursor: query.cursor.clone(),
        filter_hash: query.filter_hash.clone(),
        select: None,
    };
    let mut page = resolver
        .list_declarations(&conn, &scope, &declarations_query)
        .await?;
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-6
    // A setting hidden from the target tenant leaves the page silently — and
    // the count with it, since the page is what is counted — never marked.
    let ids: Vec<Uuid> = page.items.iter().map(|d| d.id).collect();
    let caller = ScopeTarget::Tenant(ctx.subject_tenant_id()).normalize(root);
    let access = resolver.effective_access_for(&conn, &ids, caller).await?;
    page.items
        .retain(|d| !access.get(&d.id).is_some_and(|a| a.is_hidden()));
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-6
    let has_pii = page.items.iter().any(|d| d.data_classification == "pii");
    let pii = has_pii && may_read_pii(&enforcer, &ctx).await;

    let mut items: Vec<SettingItemDto> = if filter.needs_review {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-7
        // Rows, not resolved values: the flagged overrides of the page's
        // declarations whose tenant lies in the caller's subtree, standalone
        // descendants excluded by the hierarchy.
        let target_tenant = target.tenant_id(root);
        let mut tenants = resolver.hierarchy().descendants(target_tenant).await?;
        tenants.push(target_tenant);
        let ids: Vec<Uuid> = page.items.iter().map(|d| d.id).collect();
        let rows = resolver.flagged_overrides(&conn, &ids, &tenants).await?;
        rows.iter()
            .filter_map(|row| {
                let declaration = page.items.iter().find(|d| d.id == row.declaration_id)?;
                Some(
                    SettingItemDto::flagged(render_flagged(&declaration.key, row, root, pii))
                        .with_mode(&declaration.mode),
                )
            })
            .collect()
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-7
    } else {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-8
        let outcomes = resolver
            .resolve_declarations(&conn, &page.items, target)
            .await;
        page.items
            .iter()
            .zip(outcomes)
            .map(|(declaration, outcome)| match outcome {
                Ok(effective) => {
                    SettingItemDto::resolved(render(&effective, pii)).with_mode(&declaration.mode)
                }
                Err(err) => {
                    SettingItemDto::failed(&declaration.key, &err).with_mode(&declaration.mode)
                }
            })
            .collect()
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-8
    };

    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-9
    // A named key with no declaration, or one the caller may not see, gets its
    // own entry — on the final page, where its absence is known to be final.
    if let Some(keys) = &filter.keys
        && page.page_info.next_cursor.is_none()
    {
        for key in keys {
            if !items.iter().any(|item| &item.key == key) {
                items.push(SettingItemDto::not_found(key));
            }
        }
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-9

    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-10
    Ok(Json(toolkit_odata::Page {
        items,
        page_info: page.page_info,
    }))
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-10
}

#[cfg(test)]
#[path = "setting_handlers_tests.rs"]
mod setting_handlers_tests;

// @cpt-dod:cpt-cf-settings-service-dod-audit-store-history-read:p1
/// `GET /settings-service/v1/settings/{key}/history?tenant={tenant_id}` with
/// `limit` and `cursor`.
///
/// # Errors
/// 400 on a malformed key or `tenant`, or on `$filter`, `$orderby` or `$select`,
/// which the history does not take; 403 when the caller may not read values or
/// the target is outside its subtree or standalone; 404 when no declaration
/// exists at the key or it is outside the caller's administrative domain; 503
/// when the store cannot answer.
pub async fn get_history(
    Extension(ctx): Extension<SecurityContext>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    OData(query): OData,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-1
    let key = parse_key(&key)?;
    let requested = parse_tenant(params.tenant.as_deref())?;
    if query.filter.is_some() || !query.order.0.is_empty() || query.select.is_some() {
        return Err(unsupported(
            "history takes `limit` and `cursor` only; it is always newest first",
        )
        .into());
    }
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-1
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-2
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-3
    let scope = authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-3
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-2
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-4
    // A caller that cannot read a tenant's values cannot read their history.
    let target = gate_target(&resolver, &ctx, requested).await?;
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-4
    let root = resolver.root_tenant().await?;
    let conn = db.conn().map_err(|e| conn_error(&e))?;
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-5
    // A retired declaration keeps its history and is read like an active one;
    // absence, and a declaration outside the caller's administrative domain,
    // are 404.
    let declaration = resolver
        .find_declaration(&conn, &key)
        .await?
        .filter(|d| {
            visibility::is_visible(
                &visibility::domain_visibility(&scope),
                d.domain_affinity.as_deref(),
            )
        })
        .ok_or(DomainError::NotFound {
            resource: "declaration",
        })?;
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-5
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-6
    // Hidden from the target tenant: 404 rather than 403, so a hidden setting's
    // existence is not disclosed through its history either.
    let caller = ScopeTarget::Tenant(ctx.subject_tenant_id()).normalize(root);
    if resolver
        .effective_access(&conn, declaration.id, caller)
        .await?
        .is_hidden()
    {
        return Err(DomainError::NotFound {
            resource: "declaration",
        }
        .into());
    }
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-6
    let page = crate::infra::storage::audit_store::AuditStore
        .history(&conn, &scope, key.as_str(), target.tenant_id(root), &query)
        .await?;
    let values_are_pii = declaration.data_classification == "pii";
    let needs_entitlement = values_are_pii
        || page
            .items
            .iter()
            .any(|r| r.actor_classification == crate::audit::ActorClassification::Pii);
    let pii = needs_entitlement && may_read_pii(&enforcer, &ctx).await;
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-9
    let items: Vec<AuditRecordDto> = page
        .items
        .iter()
        .map(|r| render_record(r, values_are_pii, pii))
        .collect();
    Ok(Json(toolkit_odata::Page {
        items,
        page_info: page.page_info,
    }))
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-9
}
