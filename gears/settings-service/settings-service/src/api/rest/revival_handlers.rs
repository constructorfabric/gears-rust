//! The revival report: what a revive of a retired declaration would do to the
//! values it retained, answered without doing it.
//!
//! The handler is the orchestration, in the order the contract fixes: the
//! `limit`, the two authorizations, the create's own checks, the read gates the
//! single read applies, the revive's remaining checks, and only then the walk
//! and the rows. Nothing is stored, no step-up is asked, no audit record is
//! written and no cache is touched.

use std::sync::Arc;

use axum::extract::Query;
use axum::extract::rejection::JsonRejection;
use axum::response::Response;
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::Value;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::authz::{self, resource};
use crate::api::rest::declaration_dto::{
    CreateDeclarationRequest, NeedsReviewDto, RevivalReportDto, RevivalValueDto,
};
use crate::api::rest::declaration_handlers::ConcreteDeclarationAdmin;
use crate::api::rest::setting_dto::mask;
use crate::api::rest::setting_handlers::{conn_error, may_read_pii};
use crate::domain::declaration::{
    CreateDeclaration, DataClassification, Declaration, RevivalReadGate, Walk,
    strictest_classification,
};
use crate::domain::error::DomainError;
use crate::domain::resolution::{MASK_TOKEN, SUBTREE_BUDGET, scope_path};
use crate::field;
use crate::gear::ConcreteResolver;
use toolkit_security::AccessScope;
use uuid::Uuid;

/// The values listed when `limit` is not given.
const DEFAULT_LIMIT: usize = 100;
/// The most values a report lists; a larger `limit` is clamped to it.
const MAX_LIMIT: usize = 500;

/// The report's query string.
#[derive(Debug, Deserialize)]
pub struct RevivalParams {
    /// How many values to list; kept as text so a malformed one is answered as
    /// a violation of `limit` rather than as an extractor's refusal.
    pub limit: Option<String>,
}

/// The listing size: absent is the default, anything above the bound —
/// however many digits — is clamped to it, and only a non-numeric string, zero
/// or a negative is refused.
fn parse_limit(raw: Option<&str>) -> Result<usize, DomainError> {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Ok(DEFAULT_LIMIT);
    };
    let refused = || DomainError::Validation {
        field: "limit".to_owned(),
        code: field::VALIDATION,
        message: format!("`limit` must be an integer of at least 1, got `{raw}`"),
    };
    if !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(refused());
    }
    // Digits only: too many for the integer type is simply above the bound.
    match raw.parse::<u64>() {
        Ok(0) => Err(refused()),
        Ok(n) => Ok(usize::try_from(n).unwrap_or(MAX_LIMIT).min(MAX_LIMIT)),
        Err(_) => Ok(MAX_LIMIT),
    }
}

/// The caller's view of a retired declaration through the single read's gates:
/// the grant under the value-read scope, the administrative domain and
/// `hidden` on the caller's chain, by the read the browse uses for a named
/// key. The licence predicate joins in R2, as for the single read.
struct VisibleToCaller<'a, C> {
    resolver: &'a ConcreteResolver,
    conn: &'a C,
    scope: &'a AccessScope,
    chain: &'a [Uuid],
}

#[async_trait::async_trait]
impl<C: toolkit_db::secure::DBRunner + Sync> RevivalReadGate for VisibleToCaller<'_, C> {
    async fn may_read(&self, declaration: &Declaration) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-4
        Ok(!self
            .resolver
            .visible_keys(
                self.conn,
                self.scope,
                self.chain,
                std::slice::from_ref(&declaration.key),
            )
            .await?
            .is_empty())
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-4
    }
}

/// `POST /settings-service/v1/declarations/revival-preview?limit={n}`
///
/// # Errors
/// `400` on a `limit` below 1 or non-numeric, and on a body of invalid
/// syntax or a create's own field violation; `415` and `422` as the create's
/// JSON extractor answers a wrong content type or a body of the wrong shape;
/// `403` without `create` on declarations or `read` on values; `404` for a
/// missing category, and `404 nothing_to_revive` when the body would not
/// revive a retired declaration or the caller may not read the retired one;
/// `409` as the create answers, and `leaf_name_taken`; `503` when the Type
/// Validator or a dependency of the walk cannot answer.
// Axum extractors, one per dependency; bundling them would hide what the
// handler needs.
#[allow(clippy::too_many_arguments)]
pub async fn revival_report(
    Extension(ctx): Extension<SecurityContext>,
    Extension(admin): Extension<Arc<ConcreteDeclarationAdmin>>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Query(params): Query<RevivalParams>,
    body: Result<Json<CreateDeclarationRequest>, JsonRejection>,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-1
    let limit = parse_limit(params.limit.as_deref())?;
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-1
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-2
    // Both decisions come before the body is answered, so the answer does
    // not depend on what exists.
    let declaration_scope =
        authz::access_scope(&enforcer, &ctx, &resource::DECLARATION, "create", None).await?;
    let value_scope = authz::access_scope(&enforcer, &ctx, &resource::VALUE, "read", None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-2
    let conn = db.conn().map_err(|err| conn_error(&err))?;
    // The caller's chain is read before anything else is asked, so a tenant
    // resolver that is down answers the same whatever exists.
    let caller = ctx.subject_tenant_id();
    let caller_chain = resolver.chain_of(caller).await?;
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-3
    // The body is already parsed by the extractor the create uses; only its
    // answer is deferred. A body that is malformed, of the wrong content type
    // or of the wrong shape is answered, only now and after both decisions,
    // with the very response the create gives.
    let request = match body {
        Ok(Json(request)) => request,
        Err(rejection) => return Ok(rejection.into_response()),
    };
    let request = CreateDeclaration::try_from(request)?;
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-3

    let gate = VisibleToCaller {
        resolver: &resolver,
        conn: &conn,
        scope: &value_scope,
        chain: &caller_chain,
    };
    // The gate the single read applies is the plan's last check: a retired
    // declaration the caller may not read is answered as one that is not there.
    let plan = admin
        .plan_revival(&conn, &declaration_scope, &request, &gate)
        .await?;

    let walker = Arc::clone(&resolver);
    let report = admin
        .revival_report(
            &conn,
            &value_scope,
            plan,
            &request,
            limit,
            move || async move {
                // The caller's own tenant first, then its descendants in
                // breadth-first order; standalone subtrees are left out by
                // the walk itself.
                // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-7
                let (descendants, truncated) = walker
                    .hierarchy()
                    .descendants_bfs(caller, SUBTREE_BUDGET)
                    .await?;
                let mut tenants = Vec::with_capacity(descendants.len() + 1);
                tenants.push(caller);
                tenants.extend(descendants);
                Ok(Walk { tenants, truncated })
                // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-7
            },
        )
        .await?;

    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-10
    let root = resolver.root_tenant().await?;
    // Each listed value is masked by the strictest of its row's own class, the
    // declaration's and the body's derived one.
    let classes: Vec<DataClassification> = report
        .rows
        .iter()
        .map(|row| strictest_classification([row.data_classification, report.data_classification]))
        .collect();
    let pii = classes.contains(&DataClassification::Pii) && may_read_pii(&enforcer, &ctx).await;
    let values: Vec<RevivalValueDto> = report
        .rows
        .iter()
        .zip(&classes)
        .map(|(row, class)| {
            let stored = row
                .value
                .clone()
                .unwrap_or_else(|| Value::String(MASK_TOKEN.to_owned()));
            let (value, masked) = mask(&stored, class.as_str(), pii);
            // The detail quotes the value: withheld whenever the value is.
            let masked = masked || row.value.is_none();
            RevivalValueDto {
                tenant_id: row.tenant_id,
                scope: scope_path(row.tenant_id, root),
                value,
                masked,
                review: NeedsReviewDto {
                    needs_review: row.needs_review,
                    needs_review_detail: if masked {
                        None
                    } else {
                        row.needs_review_detail.clone()
                    },
                },
            }
        })
        .collect();
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-10
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-11
    Ok(Json(RevivalReportDto {
        key: report.key,
        declaration_id: report.declaration_id,
        values,
        total: report.total as u64,
        total_needs_review: report.total_needs_review as u64,
        scanned: report.scanned as u64,
        complete: report.complete,
    })
    .into_response())
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-11
}
