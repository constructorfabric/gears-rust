// Created: 2026-08-13 by Constructor Tech
//! Category read handlers.
//!
//! Each obtains its `AccessScope` from the enforcement point **before** it
//! touches the service, so authorization is not something a handler can forget:
//! the scope is the argument every read needs, and there is no way to get one
//! except by asking the policy decision point.

use std::sync::Arc;

use axum::extract::Path;
use axum::{Extension, Json};
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::authz::{self, resource};
use crate::api::rest::dto::CategoryDto;
use crate::domain::category::service::Actor;
use crate::domain::category::{CategoryRepository, CategoryService};
use crate::domain::error::DomainError;

/// The concrete service the routes carry.
pub type ConcreteCategoryService =
    CategoryService<crate::infra::storage::category_repo::CategoryRepo>;

/// The action names authorization decisions are made against.
const READ: &str = "read";
const CREATE: &str = "create";
const UPDATE: &str = "update";
const DELETE: &str = "delete";

/// `GET /settings-service/v1/categories/{id}`
///
/// # Errors
/// `403` when the caller is not entitled to read categories, `404` when no such
/// category exists **or** it falls outside the caller's administrative domain.
pub async fn get_category<R: CategoryRepository>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R>>>,
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

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-8
    // The tag travels as a header, never in the body: one source, so a client
    // cannot echo back a stale copy it read from the wrong place.
    let etag = category.etag.as_str().to_owned();
    Ok((
        [(axum::http::header::ETAG, etag)],
        Json(CategoryDto::from(category)),
    ))
    // @cpt-end:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-8
}

/// `GET /settings-service/v1/categories`
///
/// # Errors
/// `403` when the caller is not entitled, `400` when the query names an
/// unmapped field, uses an unsupported option, or carries an undecodable
/// cursor.
pub async fn list_categories<R: CategoryRepository>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R>>>,
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

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-9
    let page = svc.list(&conn, &scope, &query).await?;
    let items: Vec<CategoryDto> = page.items.into_iter().map(CategoryDto::from).collect();
    Ok(Json(toolkit_odata::Page {
        items,
        page_info: page.page_info,
    }))
    // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-9
}

/// Read the `If-Match` header, if the client sent one.
///
/// Returned as `Option` rather than defaulted: an absent header and an empty
/// one are different answers, and only the precondition evaluator may decide
/// which is which.
fn if_match(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
}

/// The id correlating a mutation's audit record with its problem document.
///
/// Taken from the same headers the canonical error middleware reads, so an
/// audit entry and the response a caller saw carry one id, not two.
fn request_id(headers: &axum::http::HeaderMap) -> String {
    toolkit::api::error_layer::extract_trace_id(headers).unwrap_or_default()
}

/// `POST /settings-service/v1/categories`
///
/// # Errors
/// `400` on a malformed key, `403` when not entitled, `409` when the key or
/// name is taken.
pub async fn create_category<R: CategoryRepository>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R>>>,
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

    let conn = db.conn().map_err(|err| DomainError::Internal {
        diagnostic: err.to_string(),
    })?;
    let created = svc
        .create(
            &conn,
            &scope,
            draft,
            Actor {
                ctx: &ctx,
                request_id: &request_id(&headers),
            },
        )
        .await?;

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-12
    // Location as well as ETag: a creator that must immediately re-read has the
    // canonical URL without reconstructing it from the id.
    let etag = created.etag.as_str().to_owned();
    let location = format!("/settings-service/v1/categories/{}", created.id);
    Ok((
        StatusCode::CREATED,
        [
            (axum::http::header::ETAG, etag),
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
pub async fn update_category<R: CategoryRepository>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R>>>,
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

    let conn = db.conn().map_err(|err| DomainError::Internal {
        diagnostic: err.to_string(),
    })?;
    let updated = svc
        .update(
            &conn,
            &scope,
            id,
            if_match(&headers),
            patch,
            Actor {
                ctx: &ctx,
                request_id: &request_id(&headers),
            },
        )
        .await?;

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-15
    let etag = updated.etag.as_str().to_owned();
    Ok((
        [(axum::http::header::ETAG, etag)],
        Json(CategoryDto::from(updated)),
    ))
    // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-15
}

/// `DELETE /settings-service/v1/categories/{id}`
///
/// # Errors
/// `403` when not entitled, `404` when not visible, `409` while any declaration
/// references it, `412` on a stale `If-Match`, `428` when the header is absent.
pub async fn delete_category<R: CategoryRepository>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<CategoryService<R>>>,
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

    let conn = db.conn().map_err(|err| DomainError::Internal {
        diagnostic: err.to_string(),
    })?;
    svc.delete(
        &conn,
        &scope,
        id,
        if_match(&headers),
        Actor {
            ctx: &ctx,
            request_id: &request_id(&headers),
        },
    )
    .await?;

    // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-13
    Ok(StatusCode::NO_CONTENT)
    // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-13
}
