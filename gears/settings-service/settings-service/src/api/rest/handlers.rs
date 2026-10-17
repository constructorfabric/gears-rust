// Created: 2026-08-13 by Virtuozzo International GmbH
//! Category read handlers.
//!
//! Each obtains its `AccessScope` from the enforcement point **before** it
//! touches the service, so authorization is not something a handler can forget:
//! the scope is the argument every read needs, and there is no way to get one
//! except by asking the policy decision point.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::Path;
use axum::{Extension, Json};
use toolkit::api::canonical_prelude::*;
use toolkit_db::secure::DBRunner;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::authz::{self, resource};
use crate::api::rest::dto::CategoryDto;
use crate::api::rest::if_match;
use crate::api::rest::page_dto::PageDto;
use crate::audit::AuditSink;
use crate::domain::category::service::Actor;
use crate::domain::category::{CategoryRepository, CategoryService};
use crate::domain::declaration::CategoryTally;
use crate::domain::error::DomainError;
use crate::gear::ConcreteResolver;

/// The concrete service the routes carry.
pub type ConcreteCategoryService = CategoryService<
    crate::infra::storage::category_repo::CategoryRepo,
    crate::infra::storage::audit_store::AuditStore,
>;

/// The action names authorization decisions are made against.
const READ: &str = "read";
const CREATE: &str = "create";
const UPDATE: &str = "update";
const DELETE: &str = "delete";

// @cpt-dod:cpt-cf-settings-service-dod-category-management-counts:p1
/// How many settings each of `category_ids` holds for the caller, or nothing
/// when the caller may not read values.
///
/// The counts are an aggregate over settings, so they are gated as the
/// settings are — one `read` decision on the value resource, whose
/// constraints become the scope of the count — and counted under exactly
/// what the caller's browse of a category pages: its administrative-domain
/// visibility and the settings `hidden` for it on its own root-to-self
/// chain. A category's number therefore agrees with the table under it, and
/// a setting the caller may not see is in neither. A caller without the
/// value read gets the categories and no counts, rather than counts of what
/// it could not open.
///
/// # Errors
/// [`DomainError`] when the caller's chain or the tallies cannot be read; a
/// denied or unobtainable decision is not an error here but an absent count.
async fn category_tallies<C: DBRunner>(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &SecurityContext,
    resolver: &ConcreteResolver,
    conn: &C,
    category_ids: &[Uuid],
) -> Result<Option<HashMap<Uuid, CategoryTally>>, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-10
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-9
    let Ok(scope) = authz::access_scope(enforcer, ctx, &resource::VALUE, READ, None).await else {
        return Ok(None);
    };
    let caller_chain = resolver.chain_of(ctx.subject_tenant_id()).await?;
    let tallies = resolver
        .tally_declarations(conn, &scope, &caller_chain, category_ids)
        .await?;
    Ok(Some(tallies))
    // @cpt-end:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-9
    // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-10
}

/// A category rendered with its counts, when the caller may see them; a
/// category with nothing counted holds zeros, not nothing.
fn render(
    category: crate::domain::category::Category,
    tallies: Option<&HashMap<Uuid, CategoryTally>>,
) -> CategoryDto {
    let id = category.id;
    let dto = CategoryDto::from(category);
    match tallies {
        Some(tallies) => dto.with_tally(tallies.get(&id).copied().unwrap_or_default()),
        None => dto,
    }
}

/// `GET /settings-service/v1/categories/{id}`
///
/// # Errors
/// `403` when the caller is not entitled to read categories, `404` when no such
/// category exists **or** it falls outside the caller's administrative domain.
pub async fn get_category<R: CategoryRepository + 'static, S: AuditSink + 'static>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R, S>>>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-2
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-3
    let scope = authz::access_scope(&enforcer, &ctx, &resource::CATEGORY, READ, Some(id)).await?;
    // @cpt-end:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-3
    // @cpt-end:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-2

    let conn = db.conn().map_err(|err| DomainError::Internal {
        diagnostic: err.to_string(),
    })?;

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-4
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-5
    // The service applies the visibility rule and answers not-found for a
    // category outside the caller's domain, so this handler cannot leak an
    // existence signal by handling the two cases differently — it receives one
    // error for both.
    let category = svc.get(&conn, &scope, id).await?;
    // @cpt-end:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-5
    // @cpt-end:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-4

    // The same counts the listing carries, so a client that reads one
    // category back after the rail sees the number it saw there.
    let tallies = category_tallies(&enforcer, &ctx, &resolver, &conn, &[id]).await?;

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-8
    // The tag travels as a header, never in the body: one source, so a client
    // cannot echo back a stale copy it read from the wrong place.
    let etag = category.etag.as_str().to_owned();
    Ok((
        [(axum::http::header::ETAG, super::etag_header(&etag))],
        Json(render(category, tallies.as_ref())),
    ))
    // @cpt-end:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-8
}

/// `GET /settings-service/v1/categories`
///
/// # Errors
/// `403` when the caller is not entitled, `400` when the query names an
/// unmapped field, uses an unsupported option, or carries an undecodable
/// cursor.
pub async fn list_categories<R: CategoryRepository + 'static, S: AuditSink + 'static>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R, S>>>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    OData(query): OData,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-2
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-3
    let scope = authz::access_scope(&enforcer, &ctx, &resource::CATEGORY, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-3
    // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-2

    // Steps 4, 5 and 8 are the `OData` extractor's: it binds `$filter`,
    // `$orderby`, the page size and the cursor off the URL and rejects a
    // malformed expression or an undecodable cursor before this body runs. The
    // unmapped-field rejection happens a layer deeper, when the parsed tree is
    // resolved against the declared `CategoryFilterField` surface.

    let conn = db.conn().map_err(|err| DomainError::Internal {
        diagnostic: err.to_string(),
    })?;

    let page = svc.list(&conn, &scope, &query).await?;
    // One decision and one statement for the whole page: the counts of the
    // categories on it, under the caller's own view of the settings.
    let ids: Vec<Uuid> = page.items.iter().map(|c| c.id).collect();
    let tallies = category_tallies(&enforcer, &ctx, &resolver, &conn, &ids).await?;
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-9
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-11
    Ok(Json(PageDto::counted(page, |category| {
        render(category, tallies.as_ref())
    })))
    // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-11
    // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-9
}

/// `POST /settings-service/v1/categories`
///
/// # Errors
/// `400` on a malformed key, `403` when not entitled, `409` when the key or
/// name is taken.
pub async fn create_category<R: CategoryRepository + 'static, S: AuditSink + 'static>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R, S>>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<crate::api::rest::dto::CreateCategoryRequest>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-2
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-3
    // A denial and an unobtainable decision are one branch: `access_scope`
    // fails closed, so neither can reach the write below.
    let scope = authz::access_scope(&enforcer, &ctx, &resource::CATEGORY, CREATE, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-3
    // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-2

    // Validated before anything is authorized against it or written.
    let draft = body.into_draft()?;

    // One transaction for the row and its audit record: the future owns its
    // inputs because the transaction lifetime is the database's to pick.
    let request_id = super::audit_request_id(&headers);
    let created = db
        .db()
        .transaction_ref_mapped::<_, _, DomainError>(move |tx| {
            Box::pin(async move {
                svc.create(
                    tx,
                    &scope,
                    draft,
                    Actor {
                        ctx: &ctx,
                        request_id: &request_id,
                    },
                )
                .await
            })
        })
        .await?;

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-12
    // Location as well as ETag: a creator that must immediately re-read has the
    // canonical URL without reconstructing it from the id.
    let etag = created.etag.as_str().to_owned();
    let location = format!("/settings-service/v1/categories/{}", created.id);
    Ok((
        StatusCode::CREATED,
        [
            (axum::http::header::ETAG, super::etag_header(&etag)),
            (axum::http::header::LOCATION, location),
        ],
        Json(CategoryDto::from(created)),
    ))
    // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-12
}

/// `PATCH /settings-service/v1/categories/{id}`
///
/// # Errors
/// `400` on a malformed key, `403` when not entitled, `404` when not visible,
/// `409` on a key or name collision, `412` on a stale `If-Match`, `428` when
/// the header is absent.
pub async fn update_category<R: CategoryRepository + 'static, S: AuditSink + 'static>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R, S>>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<crate::api::rest::dto::UpdateCategoryRequest>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-2
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-3
    let scope = authz::access_scope(&enforcer, &ctx, &resource::CATEGORY, UPDATE, Some(id)).await?;
    // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-3
    // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-2
    let patch = body.into_patch()?;

    let request_id = super::audit_request_id(&headers);
    let if_match = if_match(&headers).map(str::to_owned);
    let updated = db
        .db()
        .transaction_ref_mapped::<_, _, DomainError>(move |tx| {
            Box::pin(async move {
                svc.update(
                    tx,
                    &scope,
                    id,
                    if_match.as_deref(),
                    patch,
                    Actor {
                        ctx: &ctx,
                        request_id: &request_id,
                    },
                )
                .await
            })
        })
        .await?;

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-15
    let etag = updated.etag.as_str().to_owned();
    Ok((
        [(axum::http::header::ETAG, super::etag_header(&etag))],
        Json(CategoryDto::from(updated)),
    ))
    // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-15
}

/// `DELETE /settings-service/v1/categories/{id}`
///
/// # Errors
/// `403` when not entitled, `404` when not visible, `409` while any declaration
/// references it, `412` on a stale `If-Match`, `428` when the header is absent.
pub async fn delete_category<R: CategoryRepository + 'static, S: AuditSink + 'static>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R, S>>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-2
    // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-3
    let scope = authz::access_scope(&enforcer, &ctx, &resource::CATEGORY, DELETE, Some(id)).await?;
    // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-3
    // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-2

    let request_id = super::audit_request_id(&headers);
    let if_match = if_match(&headers).map(str::to_owned);
    db.db()
        .transaction_ref_mapped::<_, _, DomainError>(move |tx| {
            Box::pin(async move {
                svc.delete(
                    tx,
                    &scope,
                    id,
                    if_match.as_deref(),
                    Actor {
                        ctx: &ctx,
                        request_id: &request_id,
                    },
                )
                .await
            })
        })
        .await?;

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-13
    Ok(StatusCode::NO_CONTENT)
    // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-13
}
