// Created: 2026-08-26 by Constructor Tech
//! Declaration read handlers.
//!
//! Each obtains its `AccessScope` from the enforcement point before it touches
//! the service, so authorization is not something a handler can forget: the
//! scope is the argument every read needs, and there is no way to get one except
//! by asking the policy decision point.

use std::sync::Arc;

use axum::extract::Path;
use axum::response::Response;
use axum::{Extension, Json};
use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::authz::{self, resource};
use crate::api::rest::declaration_dto::DeclarationDto;
use crate::domain::declaration::{DeclarationRepository, DeclarationService};
use crate::domain::error::DomainError;

/// The concrete service the routes carry.
pub type ConcreteDeclarationService =
    DeclarationService<crate::infra::storage::declaration_repo::DeclarationRepo>;

/// The action every read is authorized as.
const READ: &str = "read";

/// `GET /settings-service/v1/declarations/{id}`
///
/// # Errors
/// `403` when the caller is not entitled to read declarations, `404` when no
/// such declaration exists **or** it falls outside the caller's administrative
/// domain.
pub async fn get_declaration<R: DeclarationRepository>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<DeclarationService<R>>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-2
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-3
    let scope =
        authz::access_scope(&enforcer, &ctx, &resource::DECLARATION, READ, Some(id)).await?;
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-3
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-2

    let conn = db.conn().map_err(|err| DomainError::Internal {
        diagnostic: err.to_string(),
    })?;

    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-7
    // The service applies the visibility rule and answers not-found for a
    // declaration outside the caller's domain, so this handler cannot leak an
    // existence signal by handling the two cases differently -- it receives one
    // error for both.
    let declaration = svc.get(&conn, &scope, id).await?;
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-7

    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-9
    let dto = DeclarationDto::from(declaration);
    let etag = dto.etag.clone();
    Ok(([(axum::http::header::ETAG, etag)], Json(dto)))
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-9
}

/// `GET /settings-service/v1/declarations`
///
/// # Errors
/// `403` when the caller is not entitled, `400` when the query names an
/// unmapped field, uses an unsupported option, or carries an undecodable
/// cursor.
pub async fn list_declarations<R: DeclarationRepository>(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<DeclarationService<R>>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    OData(query): OData,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-2
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-3
    let scope = authz::access_scope(&enforcer, &ctx, &resource::DECLARATION, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-3
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-2

    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-5
    // The `OData` extractor binds `$filter`, `$orderby`, the page size and the
    // cursor off the URL and rejects a malformed expression or an undecodable
    // cursor before this body runs. The unmapped-field rejection happens a layer
    // deeper, when the parsed tree is resolved against the declared
    // `DeclarationFilterField` surface.
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-5

    let conn = db.conn().map_err(|err| DomainError::Internal {
        diagnostic: err.to_string(),
    })?;

    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-9
    let page = svc.list(&conn, &scope, &query).await?;
    let items: Vec<DeclarationDto> = page.items.into_iter().map(DeclarationDto::from).collect();
    Ok(Json(toolkit_odata::Page {
        items,
        page_info: page.page_info,
    }))
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-9
}

/// The concrete administrative service the mutation routes carry.
pub type ConcreteDeclarationAdmin = crate::domain::declaration::DeclarationAdmin<
    crate::infra::storage::declaration_repo::DeclarationRepo,
    crate::infra::storage::category_repo::CategoryRepo,
    crate::infra::storage::value_repo::ValueRepo,
    crate::infra::storage::audit_store::AuditStore,
>;

/// The action a create is authorized as.
const CREATE: &str = "create";
/// The action a metadata edit is authorized as.
const UPDATE: &str = "update";
/// The action a retire is authorized as.
const DELETE: &str = "delete";

/// `POST /settings-service/v1/declarations`
///
/// # Errors
/// `403` when the caller is not entitled or a revive lacks step-up, `404` when
/// the category does not exist, `400` on a key segment, classification,
/// default or scope class the rules refuse, `409` on an active declaration at
/// the key, a leaf name held in the category, or a revive that changes what a
/// revive may not, `401` with a challenge when step-up is required.
pub async fn create_declaration(
    Extension(ctx): Extension<SecurityContext>,
    Extension(admin): Extension<Arc<ConcreteDeclarationAdmin>>,
    Extension(svc): Extension<Arc<ConcreteDeclarationService>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<crate::api::rest::declaration_dto::CreateDeclarationRequest>,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-1
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-2
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-3
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-1
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-2
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-3
    let scope = authz::access_scope(&enforcer, &ctx, &resource::DECLARATION, CREATE, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-3
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-2
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-1
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-3
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-2
    let actor = crate::api::rest::value_handlers::actor(&ctx, &headers);
    let request = crate::domain::declaration::CreateDeclaration::from(body);
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-1
    let admin_for_tx = Arc::clone(&admin);
    let outcome = db
        .db()
        .transaction_ref_mapped::<_, crate::domain::declaration::Created, DomainError>(move |tx| {
            Box::pin(async move { admin_for_tx.create(tx, &scope, request, &actor).await })
        })
        .await;
    let created = match outcome {
        Ok(created) => created,
        Err(err @ DomainError::StepUpRequired { .. }) => {
            return Ok(crate::api::rest::value_handlers::step_up_challenge(err));
        }
        Err(err) => return Err(err.into()),
    };
    if created.reactivated {
        admin.evict(&created.declaration.key);
    }
    let location = format!(
        "/settings-service/v1/declarations/{}",
        created.declaration.id
    );
    let rendered = svc.render_one(created.declaration).await;
    let dto = crate::api::rest::declaration_dto::CreatedDeclarationDto {
        declaration: DeclarationDto::from(rendered),
        reactivated: created.reactivated,
    };
    let etag = dto.declaration.etag.clone();
    // A revive answers `200`: the resource was already there, and the body says
    // so with `reactivated`. Only a genuinely new row is a `201`.
    let status = if created.reactivated {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        [
            (axum::http::header::ETAG, etag),
            (axum::http::header::LOCATION, location),
        ],
        Json(dto),
    )
        .into_response())
}

/// `PATCH /settings-service/v1/declarations/{id}`
///
/// # Errors
/// `403` when the caller is not entitled, `404` when no such declaration
/// exists or it is outside the caller's domain, `409` on a contributed
/// declaration, `428`/`412` on `If-Match`, `400` on an immutable or unknown
/// field, `401` with a challenge when a field needs step-up.
// Axum extractors, one per dependency; bundling them would hide what the
// handler needs.
#[allow(clippy::too_many_arguments)]
pub async fn update_declaration(
    Extension(ctx): Extension<SecurityContext>,
    Extension(admin): Extension<Arc<ConcreteDeclarationAdmin>>,
    Extension(svc): Extension<Arc<ConcreteDeclarationService>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Map<String, serde_json::Value>>,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-1
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-2
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-3
    let scope =
        authz::access_scope(&enforcer, &ctx, &resource::DECLARATION, UPDATE, Some(id)).await?;
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-3
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-2
    let actor = crate::api::rest::value_handlers::actor(&ctx, &headers);
    let if_match = crate::api::rest::value_handlers::if_match(&headers).map(str::to_owned);
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-1
    let outcome = db
        .db()
        .transaction_ref_mapped::<_, crate::domain::declaration::Declaration, DomainError>(
            move |tx| {
                Box::pin(async move {
                    admin
                        .update(tx, &scope, id, if_match.as_deref(), &body, &actor)
                        .await
                })
            },
        )
        .await;
    let updated = match outcome {
        Ok(updated) => updated,
        Err(err @ DomainError::StepUpRequired { .. }) => {
            return Ok(crate::api::rest::value_handlers::step_up_challenge(err));
        }
        Err(err) => return Err(err.into()),
    };
    let dto = DeclarationDto::from(svc.render_one(updated).await);
    let etag = dto.etag.clone();
    Ok(([(axum::http::header::ETAG, etag)], Json(dto)).into_response())
}

/// `DELETE /settings-service/v1/declarations/{id}`
///
/// # Errors
/// As [`update_declaration`], and `401` with a challenge whenever step-up is
/// missing: retire always requires it.
// Axum extractors, one per dependency; bundling them would hide what the
// handler needs.
#[allow(clippy::too_many_arguments)]
pub async fn retire_declaration(
    Extension(ctx): Extension<SecurityContext>,
    Extension(admin): Extension<Arc<ConcreteDeclarationAdmin>>,
    Extension(svc): Extension<Arc<ConcreteDeclarationService>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-1
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-2
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-3
    let scope =
        authz::access_scope(&enforcer, &ctx, &resource::DECLARATION, DELETE, Some(id)).await?;
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-3
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-2
    let actor = crate::api::rest::value_handlers::actor(&ctx, &headers);
    let if_match = crate::api::rest::value_handlers::if_match(&headers).map(str::to_owned);
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-1
    let admin_for_tx = Arc::clone(&admin);
    let outcome = db
        .db()
        .transaction_ref_mapped::<_, crate::domain::declaration::Declaration, DomainError>(
            move |tx| {
                Box::pin(async move {
                    admin_for_tx
                        .retire(tx, &scope, id, if_match.as_deref(), &actor)
                        .await
                })
            },
        )
        .await;
    let retired = match outcome {
        Ok(retired) => retired,
        Err(err @ DomainError::StepUpRequired { .. }) => {
            return Ok(crate::api::rest::value_handlers::step_up_challenge(err));
        }
        Err(err) => return Err(err.into()),
    };
    // Evicted again now the status is durable: a reader between the in-transaction
    // eviction and the commit could have re-populated the entry.
    admin.evict(&retired.key);
    let dto = DeclarationDto::from(svc.render_one(retired).await);
    let etag = dto.etag.clone();
    Ok(([(axum::http::header::ETAG, etag)], Json(dto)).into_response())
}
