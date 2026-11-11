//! The handler of the activation read.

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::{Extension, Json};
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::authz::{self, resource};
use crate::api::rest::activation_dto::ActivationDto;
use crate::api::rest::setting_handlers::{parse_key, parse_tenant};
use crate::domain::error::DomainError;
use crate::field;
use crate::gear::{ConcreteRecorder, ConcreteResolver};

const READ: &str = "read";

/// The one query parameter the route takes. Every other — an `OData` option, a
/// `cursor`, a `limit`, anything else — is refused by the same rule.
const TENANT: &str = "tenant";

/// The `tenant` of the request, when named, refusing every parameter the read
/// does not take and a `tenant` named twice.
fn tenant_of(params: &[(String, String)]) -> Result<Option<&str>, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-2
    if let Some((name, _)) = params.iter().find(|(name, _)| name != TENANT) {
        return Err(DomainError::Validation {
            field: name.clone(),
            code: field::ODATA_UNSUPPORTED_OPTION,
            message: format!("the activation read takes `tenant` only, not `{name}`"),
        });
    }
    if params.len() > 1 {
        return Err(DomainError::Validation {
            field: TENANT.to_owned(),
            code: field::TENANT_PARAM,
            message: "`tenant` is named more than once".to_owned(),
        });
    }
    Ok(params.first().map(|(_, value)| value.as_str()))
    // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-2
}

// @cpt-dod:cpt-cf-settings-service-dod-activation-read-route:p1
// @cpt-dod:cpt-cf-settings-service-dod-activation-read-authorization:p1
// @cpt-dod:cpt-cf-settings-service-dod-activation-read-response:p1
/// `GET /settings-service/v1/settings/{key}/activation?tenant={tenant_id}`
///
/// # Errors
/// 400 on a malformed key or `tenant`, or on any other query parameter; 403
/// when the caller may not read values, whatever key and tenant it names; 404
/// for a tenant outside the caller's subtree, an unknown key, a declaration
/// outside the caller's administrative domain and a key hidden from the caller;
/// 503 when the tenant resolver or the database cannot answer.
pub async fn get_activation(
    Extension(ctx): Extension<SecurityContext>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(recorder): Extension<Arc<ConcreteRecorder>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<Vec<(String, String)>>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-1
    let key = parse_key(&key)?;
    let requested = parse_tenant(tenant_of(&params)?)?;
    // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-1
    // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-4
    // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-5
    // The decision of a value read, before the key or the tenant is looked at:
    // a denial and an undecidable answer are one `403`, the same for every key
    // and tenant, declared or not.
    let scope = authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-5
    // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-4
    let read = recorder
        .activation_status(&resolver, &ctx, &scope, &key, requested)
        .await?;
    // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-12
    Ok(Json(ActivationDto::from(read)))
    // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-12
}
