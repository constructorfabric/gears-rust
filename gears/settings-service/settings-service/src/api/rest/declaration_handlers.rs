// Created: 2026-08-26 by Constructor Tech
//! Declaration read handlers.
//!
//! Each obtains its `AccessScope` from the enforcement point before it touches
//! the service, so authorization is not something a handler can forget: the
//! scope is the argument every read needs, and there is no way to get one except
//! by asking the policy decision point.

use std::sync::Arc;

use axum::extract::Path;
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
    Ok(Json(DeclarationDto::from(declaration)))
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
