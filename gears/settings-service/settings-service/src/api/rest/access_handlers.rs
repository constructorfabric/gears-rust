// Created: 2026-09-07 by Constructor Tech
//! Handlers of the tenant access restriction surface.

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, header};
use axum::{Extension, Json};
use settings_service_sdk::SettingKey;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::authz::{self, resource};
use crate::api::rest::access_dto::{
    AccessReadDto, RestrictionDto, SetRestrictionRequest, render_readout, render_restriction,
};
use crate::api::rest::setting_handlers::TenantParam;
use crate::domain::access::{AccessActor, AccessReadout, TenantAccess};
use crate::domain::error::DomainError;
use crate::field;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::audit_store::AuditStore;
use crate::infra::storage::declaration_repo::DeclarationRepo;

/// The service over the concrete repositories and the audit store.
pub type ConcreteAccessService =
    crate::domain::access::AccessService<DeclarationRepo, AccessRepo, AuditStore>;

const READ: &str = "read";
/// Restricting a descendant is a distinct power from writing its values.
const DELEGATE: &str = "delegate";

fn parse_key(raw: &str) -> Result<SettingKey, DomainError> {
    SettingKey::parse(raw).map_err(|e| DomainError::Validation {
        field: "key".to_owned(),
        code: field::VALIDATION,
        message: e.to_string(),
    })
}

/// `tenant` is the tenant being restricted or asked about, never the caller;
/// on this surface it is required.
fn parse_target(raw: Option<&str>) -> Result<Uuid, DomainError> {
    raw.filter(|s| !s.is_empty())
        .ok_or_else(|| DomainError::Validation {
            field: "tenant".to_owned(),
            code: field::TENANT_PARAM,
            message: "`tenant` names the tenant the restriction is about and is required"
                .to_owned(),
        })
        .and_then(|raw| {
            Uuid::parse_str(raw).map_err(|_| DomainError::Validation {
                field: "tenant".to_owned(),
                code: field::TENANT_PARAM,
                message: format!("`{raw}` is not a tenant id"),
            })
        })
}

fn actor(ctx: &SecurityContext, headers: &HeaderMap) -> AccessActor {
    AccessActor {
        ctx: ctx.clone(),
        request_id: toolkit::api::error_layer::extract_trace_id(headers)
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
    }
}

fn if_match(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().trim_matches('"').to_owned())
}

fn conn_error(err: &toolkit_db::DbError) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

fn with_etag(readout: &AccessReadout) -> ([(header::HeaderName, String); 1], Json<AccessReadDto>) {
    let dto = render_readout(readout);
    ([(header::ETAG, dto.etag.clone())], Json(dto))
}

/// `GET /settings-service/v1/settings/{key}/permissions?tenant={tenant_id}`
///
/// # Errors
/// 400 for a malformed key or a missing `tenant`; 403 when the caller may not
/// read or the target is outside its subtree or standalone; 404 when the
/// declaration is absent or hidden from the caller.
pub async fn read_access(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<ConcreteAccessService>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-1
    let key = parse_key(&key)?;
    let target = parse_target(params.tenant.as_deref())?;
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-1
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-2
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-2
    let conn = db.conn().map_err(|e| conn_error(&e))?;
    let readout = service
        .read(&conn, &actor(&ctx, &headers), &key, target)
        .await?;
    Ok(with_etag(&readout))
}

// Axum extractors, one per dependency; bundling them would hide what the
// handler needs.
#[allow(clippy::too_many_arguments)]
/// `PUT /settings-service/v1/settings/{key}/permissions?tenant={tenant_id}`
///
/// # Errors
/// 400 for a malformed key, a missing `tenant` or an `access` other than
/// `read_only` or `hidden`; 403 when the caller lacks `delegate` or the target
/// is not a reachable strict descendant; 404 when the declaration is absent or
/// hidden; 428 without `If-Match`, 412 when it is stale.
pub async fn set_access(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<ConcreteAccessService>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    headers: HeaderMap,
    Json(body): Json<SetRestrictionRequest>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-1
    let key = parse_key(&key)?;
    let target = parse_target(params.tenant.as_deref())?;
    let access = TenantAccess::parse(&body.access).ok_or_else(|| DomainError::Validation {
        field: "access".to_owned(),
        code: field::VALIDATION,
        message: format!("`{}` is not `read_only` or `hidden`", body.access),
    })?;
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-1
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-2
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-3
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, DELEGATE, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-3
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-2
    let actor = actor(&ctx, &headers);
    let if_match = if_match(&headers);
    let readout = {
        let service = Arc::clone(&service);
        let actor = actor.clone();
        let key = key.clone();
        db.db()
            .transaction_ref_mapped::<_, AccessReadout, DomainError>(move |tx| {
                Box::pin(async move {
                    service
                        .set(tx, &actor, &key, target, access, if_match.as_deref())
                        .await
                })
            })
            .await?
    };
    service.evict(&key, target).await?;
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-13
    Ok(with_etag(&readout))
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-13
}

/// `DELETE /settings-service/v1/settings/{key}/permissions?tenant={tenant_id}`
///
/// # Errors
/// As [`set_access`], without the access validation.
pub async fn clear_access(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<ConcreteAccessService>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-1
    let key = parse_key(&key)?;
    let target = parse_target(params.tenant.as_deref())?;
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-1
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-2
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, DELEGATE, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-2
    let actor = actor(&ctx, &headers);
    let if_match = if_match(&headers);
    let readout = {
        let service = Arc::clone(&service);
        let actor = actor.clone();
        let key = key.clone();
        db.db()
            .transaction_ref_mapped::<_, AccessReadout, DomainError>(move |tx| {
                Box::pin(async move {
                    service
                        .clear(tx, &actor, &key, target, if_match.as_deref())
                        .await
                })
            })
            .await?
    };
    service.evict(&key, target).await?;
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-9
    Ok(with_etag(&readout))
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-9
}

/// `GET /settings-service/v1/settings/{key}/permissions/all`
///
/// # Errors
/// 400 for a malformed key; 403 when the caller may not read; 404 when the
/// declaration is absent or hidden from the caller.
pub async fn list_access(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<ConcreteAccessService>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-1
    let key = parse_key(&key)?;
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-1
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-2
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-2
    let conn = db.conn().map_err(|e| conn_error(&e))?;
    let rows = service.list(&conn, &actor(&ctx, &headers), &key).await?;
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-5
    let items: Vec<RestrictionDto> = rows.iter().map(render_restriction).collect();
    let limit = u64::try_from(items.len()).unwrap_or(u64::MAX).max(1);
    Ok(Json(toolkit_odata::Page {
        items,
        page_info: toolkit_odata::PageInfo {
            next_cursor: None,
            prev_cursor: None,
            limit,
        },
    }))
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-5
}
