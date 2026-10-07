//! `GET /skus/{id}/history` (P-D-213): the SKU's audit rows and its units' audit rows, in the
//! order the acts wrote them (`audit_id`, a UUID v7 minted in the act's transaction), on the
//! toolkit's pager — `$top` (alias `limit`; default 50, clamped at 200) and `cursor` (alias
//! `$skiptoken`); no `$filter`, `$orderby` or `$select`. Read under `sku × read`, like the card;
//! a SKU the caller's tenant does not hold, or a deleted draft, is 404.
use super::{
    ApiState, authz_error_to_canonical,
    dto::ProductsSkuHistoryEntry,
    require_authenticated,
    sku_list::{RawQuery, UNSUPPORTED, cursor_hash, params, refused},
    tx_to_canonical,
};
use crate::{
    authz::{access_scope, actions, resource_types},
    infra::storage::repo::{self, SkuListError},
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{Extension, Json, Router, extract::Path, http::StatusCode};
use std::sync::Arc;
use toolkit::api::{
    OpenApiRegistry,
    canonical_prelude::{CanonicalError, resource_error},
    odata::OData,
    operation_builder::OperationBuilder,
};
use toolkit_db::secure::AccessScope;
use toolkit_odata::{Error as ODataError, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

#[resource_error(gts_id!("cf.bss.products.sku.v1~"))]
struct SkuResource;

/// Register the history beside the SKU card.
pub(crate) fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    OperationBuilder::get("/bss-products/v1/skus/{id}/history")
        .operation_id("bss_products.sku_history")
        .summary("Read a SKU's history")
        .description(
            "Every act on the SKU and on its approval units, in the order the acts wrote them \
             (P-D-213): `at` (the instant the act began), `actor` (the nil uuid for the system's \
             orphan-fence expiry), `action`, `from_lifecycle` and `to_lifecycle` (null on a row \
             written before the audit log carried them), `unit_id` and `unit_kind` for a unit's \
             act, and `note`. \
             `$top` (alias `limit`; default 50, clamped at 200) and `cursor` (alias `$skiptoken`) \
             from `page_info`; any other key is 400, and a cursor from another SKU's history is \
             400. A SKU of another tenant, or a deleted draft, is 404.",
        )
        .tag("SKUs")
        .authenticated()
        .no_license_required()
        .path_param("id", "SKU id")
        .query_param_typed(
            "limit",
            false,
            "Page size, alias of $top (default 50, clamped at 200)",
            "integer",
        )
        .query_param_typed(
            "cursor",
            false,
            "Continuation from page_info (alias $skiptoken)",
            "string",
        )
        .handler(sku_history)
        .json_response_with_schema::<Page<ProductsSkuHistoryEntry>>(
            openapi,
            StatusCode::OK,
            "One page of the SKU's history.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// Read under `sku × read`, as every SKU read.
async fn read_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<AccessScope, CanonicalError> {
    access_scope(enforcer, ctx, &resource_types::SKU, actions::READ, None)
        .await
        .map_err(|e| {
            authz_error_to_canonical(e, |reason| {
                SkuResource::permission_denied()
                    .with_reason(reason)
                    .create()
            })
        })
}

/// @cpt-cf-bss-products-fr-read-model
async fn sku_history(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    query: RawQuery,
    odata: Result<OData, CanonicalError>,
) -> Result<Json<Page<ProductsSkuHistoryEntry>>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // Authorization first, then the query (a 403 before a 400).
    let scope = read_scope(&enforcer, &ctx).await?;
    params(query, &["limit", "cursor"], Some(&["$top", "$skiptoken"]))?;
    let OData(mut odata) = odata?;
    if odata.select.is_some() {
        refused(&[(
            "$select",
            "a history entry is not projected; drop `$select`".to_owned(),
            UNSUPPORTED,
        )])?;
    }
    // A cursor belongs to one SKU's history.
    let hash = cursor_hash(&serde_json::json!({ "history": id }));
    if let Some(cursor) = &odata.cursor
        && cursor.f.as_deref() != Some(hash.as_str())
    {
        return Err(ODataError::FilterMismatch.into());
    }
    odata.filter_hash = Some(hash);
    let tenant = ctx.subject_tenant_id();
    // The SKU's orphan fence expires first, as on every SKU read (P-D-189), with its audit row.
    super::governance::touch(&state, &scope, tenant, id).await?;
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    super::governance::find(&conn, &scope, tenant, id)
        .await
        .map_err(tx_to_canonical)?;
    let page = repo::page_sku_history(&conn, tenant, id, &odata)
        .await
        .map_err(|e| match e {
            SkuListError::Query(e) => CanonicalError::from(e),
            SkuListError::Repo(e) => super::repo_error_to_canonical(&e),
        })?;
    let mut items: Vec<ProductsSkuHistoryEntry> = page.items.into_iter().map(Into::into).collect();
    // P-D-262: the page's actors in one lookup; the system's acts read "System".
    state.actor_names.fill(&ctx, &mut items).await;
    Ok(Json(Page {
        items,
        page_info: page.page_info,
    }))
}
