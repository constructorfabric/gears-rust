//! @cpt-dod:cpt-cf-bss-products-dod-derived-usage-type-doors:p1
//! The derived usage type doors (P-D-229, P-D-231): create a type with its version 1, add a version,
//! list the tenant's types, read one with its versions' headers, and read one version with what a
//! pricing author copies into a usage policy.
//!
//! - **Writes** ask `author` on `derived_usage_type`, anchored to the caller's tenant, and take an
//!   optional `Idempotency-Key`. A version is append-only, with no approval of its own (O-1): a usage
//!   SKU adopts one only at its own approved first publish (Run 3).
//! - **Reads** ask `sku:read` (O-3). The decision stays that grant (403 / 503). The SQL filter is
//!   `tenant_only()` of it, beside the caller's tenant, so a SKU `resource_id` never selects a derived row.
//! - **The order of the checks** (P-D-202): the caller, the PDP, the JSON body, the replay store,
//!   the request's shape, the identity and the declaration rules (and, for a new version, its
//!   type), then each input through the usage-type catalog as the caller; then one transaction
//!   writes the rows and their audit rows.
//! - **The digest** is taken once, at the write, over the SDK's canonical bytes and stored; every
//!   read answers the stored one (decision 3).
use super::{
    ApiState, TxError, contention_db_err,
    dto::{
        ProductsDerivedDeclaration, ProductsDerivedMeterRef, ProductsDerivedUsageType,
        ProductsDerivedUsageTypeItem, ProductsDerivedUsageTypeRequest,
        ProductsDerivedUsageTypeVersion, ProductsDerivedUsageTypeVersionRequest,
        ProductsDerivedVersionHeader,
    },
    json_body, replay, repo_error_to_canonical, require_authenticated,
    sku_list::{RawQuery, cursor_hash, params},
    tx_to_canonical,
};
use crate::{
    authz::{access_scope, actions, labels, resource_types},
    domain::{
        derived::{
            self, ACTION_CREATE, ACTION_VERSION, CODE_TAKEN, DerivedPin, DerivedUsageType,
            DerivedUsageTypeVersion, NewDerivedType, NewDerivedVersion, SUBJECT_KIND,
        },
        error::DomainError,
    },
    infra::storage::{
        RepoError, RepoRefusal,
        repo::{self, SkuListError, derived_usage_type_repo as store},
    },
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Json, Router,
    extract::{Path, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bss_products_sdk::derived::{DerivedUsageDeclaration, MeterId};
use bss_rest::conditional_get::{PRIVATE_REVALIDATE, respond};
use std::sync::Arc;
use toolkit::api::{
    OpenApiRegistry,
    canonical_prelude::{CanonicalError, resource_error},
    odata::OData,
    operation_builder::OperationBuilder,
};
use toolkit_db::secure::{AccessScope, DBRunner, TxConfig};
use toolkit_odata::{Error as ODataError, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

pub(crate) const DERIVED: &str = "/bss-products/v1/derived-usage-types";
const TAG: &str = "Derived usage types";
#[resource_error(gts_id!("cf.bss.products.derived_usage_type.v1~"))]
struct DerivedUsageTypeResource;
#[resource_error(gts_id!("cf.bss.products.sku.v1~"))]
struct SkuResource;

/// The refusals every write shares, as its served text names them.
const WRITE_REFUSALS: &str = "400 DERIVED_DECLARATION_INVALID when the declaration breaks a rule, \
     its detail led by the rule's name (`empty_unit`, `unit_too_long`, `scale_too_large`, \
     `too_few_inputs`, `invalid_input_name`, `duplicate_input`, `empty_input_ref`, \
     `input_ref_too_long`, `derived_input`, `hold_missing`, `hold_not_allowed`, \
     `hold_out_of_range`, `unknown_input`, `unused_input`, `division_by_zero`, `too_few_operands`, \
     `too_deep`, `too_many_nodes`, and the shape's `unknown_granularity`, `unknown_fold`, \
     `unknown_round_mode`, `unknown_operator`, `invalid_decimal`, `malformed_expression`); 400 \
     USAGE_TYPE_UNRESOLVED on each input the usage-type catalog does not know; 403 \
     USAGE_TYPE_FORBIDDEN when the catalog refuses the caller (it is read as the caller, \
     P-D-207); 409 IDEMPOTENCY_CONFLICT for a key replayed with another body, \
     IDEMPOTENCY_KEY_IN_FLIGHT while its first request runs; 503 USAGE_TYPE_UNAVAILABLE when the \
     catalog does not answer or none is configured";

/// Register the five operations.
pub(crate) fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post(DERIVED)
        .operation_id("bss_products.create_derived_usage_type")
        .summary("Create a derived usage type")
        .description(format!(
            "A derived usage type and its version 1 (P-D-229, P-D-231): a `code` unique in the \
             tenant (`^[a-z0-9][a-z0-9._-]{{0,63}}$`), a `name`, and a `declaration` — at least \
             one raw GTS usage-type input, the formula one granule's folded inputs give, and the \
             output's unit, scale and rounding. Each input resolves through the usage-type \
             catalog. The version is append-only and needs no approval (O-1); pricing names it \
             as `products.derived/<code>@1`. Asks `author` on `derived_usage_type`. Refusals: 400 \
             VALIDATION on a code off its pattern or a blank name, FIELD_TOO_LONG on a code over \
             64 or a name over 200 characters (P-D-225); {WRITE_REFUSALS}; 409 \
             DERIVED_CODE_TAKEN when the tenant has the code."
        ))
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<ProductsDerivedUsageTypeRequest>(openapi, "code, name, declaration")
        .param(replay::param())
        .handler(create_derived_usage_type)
        .json_response_with_schema::<ProductsDerivedUsageTypeVersion>(
            openapi,
            StatusCode::CREATED,
            "The type's version 1, with its meter reference and accrual policy version.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(Router::new(), openapi);
    let router = OperationBuilder::post(format!("{DERIVED}/{{code}}/versions"))
        .operation_id("bss_products.create_derived_usage_type_version")
        .summary("Add a version to a derived usage type")
        .description(format!(
            "The type's next version, n + 1, with its own declaration; every earlier version \
             stays as it was (P-D-231). A published usage SKU keeps the version it was published \
             on: a new version is sold through a new SKU (M1). Asks `author` on \
             `derived_usage_type`. Refusals: 404 when the tenant has no type with the code; \
             {WRITE_REFUSALS}; 409 CONTENDED when a concurrent write took the number first."
        ))
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("code", "The derived usage type's code")
        .json_request::<ProductsDerivedUsageTypeVersionRequest>(openapi, "declaration")
        .param(replay::param())
        .handler(create_derived_usage_type_version)
        .json_response_with_schema::<ProductsDerivedUsageTypeVersion>(
            openapi,
            StatusCode::CREATED,
            "The new version, with its meter reference and accrual policy version.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get(DERIVED)
        .operation_id("bss_products.list_derived_usage_types")
        .summary("List derived usage types")
        .description(
            "One page of the tenant's derived usage types by code (tie-break id). Each item \
             carries `latest_version` and `latest`, the latest version in the version-read \
             shape: `version`, `declaration` (inputs and formula), `digest`, `meter_ref`, \
             `canonical_unit`, `accrual_policy_version`, `created_by` and `created_at`. `$top` \
             (alias `limit`; default 50, clamped at 200) and `cursor` (alias `$skiptoken`) from \
             `page_info`, as the SKU list pages. Asks `sku:read` (O-3). Any other key, \
             `$filter`, `$orderby` and `$select` are 400 UNSUPPORTED_QUERY_PARAM; a malformed \
             cursor, or one another list cut, is 400. A matching If-None-Match is 304 with an empty \
             body; the 200 carries a weak ETag of its JSON and Cache-Control private, no-cache: \
             the page names its creators, so the browser revalidates it (P-D-261).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
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
        .param(super::preconditions::if_none_match_param())
        .handler(list_derived_usage_types)
        .json_response_with_schema::<Page<ProductsDerivedUsageTypeItem>>(
            openapi,
            StatusCode::OK,
            "One page of derived usage types, by code.",
        )
        .response_header(super::preconditions::weak_etag_header())
        .response_header(super::preconditions::revalidate_header())
        .no_content_response(
            StatusCode::NOT_MODIFIED,
            "The If-None-Match tag matches this body",
        )
        .response_header(super::preconditions::weak_etag_header())
        .response_header(super::preconditions::revalidate_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get(format!("{DERIVED}/{{code}}"))
        .operation_id("bss_products.get_derived_usage_type")
        .summary("Read a derived usage type")
        .description(
            "The type with its versions' headers, oldest first: each version's digest, meter \
             reference and accrual policy version. Asks `sku:read` (O-3). 404 when the tenant has \
             no type with the code.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("code", "The derived usage type's code")
        .handler(get_derived_usage_type)
        .json_response_with_schema::<ProductsDerivedUsageType>(
            openapi,
            StatusCode::OK,
            "The type and its versions.",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get(format!("{DERIVED}/{{code}}/versions/{{n}}"))
        .operation_id("bss_products.get_derived_usage_type_version")
        .summary("Read one version of a derived usage type")
        .description(
            "The version's declaration, its stored digest, and what a pricing usage policy names \
             (O-2): `meter_ref` `{usage_type_id: \"products.derived/<code>@<n>\", version: \
             \"<n>\"}`, `canonical_unit` (the declaration's output unit) and \
             `accrual_policy_version` (`derived-v1:<digest>`). Asks `sku:read` (O-3). 404 when \
             the tenant has no type with the code, or the type no such version; an `n` that is \
             not a canonical decimal names no version.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("code", "The derived usage type's code")
        .path_param("n", "The version, a canonical decimal from 1")
        .handler(get_derived_usage_type_version)
        .json_response_with_schema::<ProductsDerivedUsageTypeVersion>(
            openapi,
            StatusCode::OK,
            "The version.",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router.layer(Extension(state))
}

/// `author` on `derived_usage_type`, anchored to the caller's tenant.
async fn author_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<AccessScope, CanonicalError> {
    access_scope(
        enforcer,
        ctx,
        &resource_types::DERIVED_USAGE_TYPE,
        actions::AUTHOR,
        Some(ctx.subject_tenant_id()),
    )
    .await
    .map_err(|e| {
        super::authz_error_to_canonical(e, |reason| {
            crate::infra::error_mapping::permission_denied(labels::DERIVED_USAGE_TYPE, reason)
        })
    })
}

/// A stored derived meter row that does not read. `Internal` hides its description, so this is a data-loss 500:
/// status 500, and the wire detail is the fixed sentence. The cause is logged and is not on the wire.
pub(crate) fn corrupt_row(err: &RepoError) -> CanonicalError {
    tracing::error!(error = %err, "bss-products: a stored derived meter row does not read");
    DerivedUsageTypeResource::data_loss("a stored derived meter row does not read")
        .with_resource("derived usage type")
        .create()
}

/// A derived-row failure. `CorruptRow` stays 500 with a fixed detail; every other repository failure keeps the
/// shared mapping.
pub(crate) fn stored_row_error(err: &RepoError) -> CanonicalError {
    if matches!(err, RepoError::CorruptRow(_)) {
        corrupt_row(err)
    } else {
        repo_error_to_canonical(err)
    }
}

/// `sku:read` (O-3): the decision is this scope (403 denied, 503 unreachable). SQL reads of a derived row use
/// [`AccessScope::tenant_only`] of it, beside the caller's tenant (P-D-231 amended). A SKU `resource_id` is not a
/// derived type id.
pub(crate) async fn read_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<AccessScope, CanonicalError> {
    access_scope(enforcer, ctx, &resource_types::SKU, actions::READ, None)
        .await
        .map_err(|e| {
            super::authz_error_to_canonical(e, |reason| {
                SkuResource::permission_denied()
                    .with_reason(reason)
                    .create()
            })
        })
}

/// The 404 of a code the tenant does not hold, or of a path that is not a meter id. The detail is
/// fixed, so a path is never copied into the body (P-D-231).
fn not_found() -> CanonicalError {
    DerivedUsageTypeResource::not_found("derived usage type")
        .with_resource("derived usage type")
        .create()
}

/// A path code the meter id accepts. Anything else is the same 404 as a missing type.
fn path_code(code: &str) -> Result<(), CanonicalError> {
    if MeterId::new(code, 1).is_err() {
        return Err(not_found());
    }
    Ok(())
}

/// The meter id of a stored version; a stored code the id cannot carry is a corrupt row.
fn meter(t: &DerivedUsageType, version: u32) -> Result<ProductsDerivedMeterRef, RepoError> {
    let (usage_type_id, version) = MeterId::new(t.code.as_str(), version)
        .map_err(|e| RepoError::CorruptRow(format!("derived usage type {} meter id: {e}", t.id)))?
        .meter_ref();
    Ok(ProductsDerivedMeterRef {
        usage_type_id,
        version,
    })
}

fn declaration_of(
    t: &DerivedUsageType,
    v: &DerivedUsageTypeVersion,
) -> Result<ProductsDerivedDeclaration, RepoError> {
    serde::Deserialize::deserialize(&v.declaration_json).map_err(|e| {
        RepoError::CorruptRow(format!(
            "derived usage type {} version {} declaration: {e}",
            t.id, v.version
        ))
    })
}

/// A stored version as the doors answer it; its digest is the stored one.
fn version_dto(
    t: &DerivedUsageType,
    v: &DerivedUsageTypeVersion,
) -> Result<ProductsDerivedUsageTypeVersion, RepoError> {
    let declaration = declaration_of(t, v)?;
    let canonical_unit = declaration.output_unit.clone();
    Ok(ProductsDerivedUsageTypeVersion {
        id: t.id,
        code: t.code.clone(),
        name: t.name.clone(),
        version: v.version,
        declaration,
        digest: v.digest.clone(),
        meter_ref: meter(t, v.version)?,
        canonical_unit,
        accrual_policy_version: v.accrual_policy_version(),
        created_by: v.created_by,
        created_by_name: None,
        created_at: v.created_at,
    })
}

/// The declaration a write carries, through the shape and the SDK's rules, then its inputs through
/// the catalog as the caller: what the version stores, and its digest.
async fn judged(
    state: &ApiState,
    ctx: &SecurityContext,
    wire: &ProductsDerivedDeclaration,
) -> Result<(serde_json::Value, String), CanonicalError> {
    let declaration = DerivedUsageDeclaration::try_from(wire)?;
    derived::validate(&declaration)?;
    derived::resolve_inputs(state.usage_type_catalog.as_ref(), ctx, &declaration).await?;
    let stored = serde_json::to_value(ProductsDerivedDeclaration::from(&declaration))
        .map_err(|e| stored_row_error(&RepoError::Db(format!("declaration: {e}"))))?;
    Ok((stored, derived::digest_hex(&declaration)))
}

/// The audit row of a create or a version, in the write's transaction (P-D-193): the type's id is
/// the subject and the version its revision.
async fn audit(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    actor_ref: Uuid,
    action: &str,
    v: &DerivedUsageTypeVersion,
) -> Result<(), TxError> {
    repo::write_eventless_act_audit(
        tx,
        scope,
        repo::AuditCommon {
            audit_id: Uuid::now_v7(),
            tenant_id,
            actor_ref,
            action: action.to_owned(),
            subject_kind: SUBJECT_KIND.to_owned(),
            reason: None,
            correlation_id: None,
            written_at: v.created_at,
            lifecycle: repo::LifecycleMove::NONE,
        },
        v.type_id,
        Some(i64::from(v.version)),
    )
    .await
    .map_err(TxError::Repo)
}

/// A write's replayed answer, read before the body is judged or the catalog asked.
async fn replayed(
    state: &ApiState,
    tenant: Uuid,
    claim: Option<&crate::infra::idempotency::IdempotencyClaimInput>,
) -> Result<Option<Response>, CanonicalError> {
    replay::lookup(
        &state.db.conn().map_err(|e| tx_to_canonical(e.into()))?,
        tenant,
        claim,
    )
    .await
    .map_err(tx_to_canonical)
}

/// @cpt-cf-bss-products-fr-derived-usage-type
async fn create_derived_usage_type(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = ctx.subject_tenant_id();
    let actor = ctx.subject_id();
    let scope_tx = author_scope(&enforcer, &ctx).await?;
    let payload = json_body(body)?;
    let claim = replay::input(&state, &headers, DERIVED.into(), &payload)?;
    if let Some(response) = replayed(&state, tenant_id, claim.as_ref()).await? {
        return Ok(response);
    }
    let body: ProductsDerivedUsageTypeRequest = serde_json::from_value(payload)
        .map_err(|e| CanonicalError::from(super::governance::validation("body", e.to_string())))?;
    let new_tx = NewDerivedType {
        code: body.code.trim().to_owned(),
        name: body.name.trim().to_owned(),
    };
    let report = derived::check_identity(&new_tx);
    if !report.is_empty() {
        return Err(DomainError::Validation(report).into());
    }
    let (declaration_json, digest) = judged(&state, &ctx, &body.declaration).await?;
    let now = crate::infra::storage::stored_now();
    state
        .db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            TxConfig::default(),
            contention_db_err,
            move |tx| {
                let scope = scope_tx.clone();
                let (new, claim) = (new_tx.clone(), claim.clone());
                let (declaration_json, digest) = (declaration_json.clone(), digest.clone());
                Box::pin(async move {
                    if let Some(response) = replay::begin(tx, tenant_id, claim.as_ref()).await? {
                        return Ok(response);
                    }
                    let t = store::create_type(tx, &scope, tenant_id, new, actor, now)
                        .await
                        .map_err(write_error)?;
                    let v = store::insert_version(
                        tx,
                        &scope,
                        tenant_id,
                        NewDerivedVersion {
                            type_id: t.id,
                            version: 1,
                            declaration_json,
                            digest,
                            created_by: actor,
                            created_at: now,
                        },
                    )
                    .await
                    .map_err(write_error)?;
                    audit(tx, &scope, tenant_id, actor, ACTION_CREATE, &v).await?;
                    replay::finish(
                        tx,
                        tenant_id,
                        claim.as_ref(),
                        StatusCode::CREATED,
                        &version_dto(&t, &v).map_err(TxError::Repo)?,
                    )
                    .await
                })
            },
        )
        .await
        .map_err(tx_to_canonical)
}

/// @cpt-cf-bss-products-fr-derived-usage-type
async fn create_derived_usage_type_version(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(code): Path<String>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = ctx.subject_tenant_id();
    let actor = ctx.subject_id();
    let scope_tx = author_scope(&enforcer, &ctx).await?;
    path_code(&code)?;
    let payload = json_body(body)?;
    let claim = replay::input(
        &state,
        &headers,
        format!("{DERIVED}/{code}/versions"),
        &payload,
    )?;
    if let Some(response) = replayed(&state, tenant_id, claim.as_ref()).await? {
        return Ok(response);
    }
    let body: ProductsDerivedUsageTypeVersionRequest = serde_json::from_value(payload)
        .map_err(|e| CanonicalError::from(super::governance::validation("body", e.to_string())))?;
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    if store::find_type(&conn, &scope_tx, tenant_id, &code)
        .await
        .map_err(|e| stored_row_error(&e))?
        .is_none()
    {
        return Err(not_found());
    }
    let (declaration_json, digest) = judged(&state, &ctx, &body.declaration).await?;
    let now = crate::infra::storage::stored_now();
    state
        .db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            TxConfig::default(),
            contention_db_err,
            move |tx| {
                let scope = scope_tx.clone();
                let (code, claim) = (code.clone(), claim.clone());
                let (declaration_json, digest) = (declaration_json.clone(), digest.clone());
                Box::pin(async move {
                    if let Some(response) = replay::begin(tx, tenant_id, claim.as_ref()).await? {
                        return Ok(response);
                    }
                    let t = store::find_type(tx, &scope, tenant_id, &code)
                        .await
                        .map_err(TxError::Repo)?
                        .ok_or(TxError::DerivedTypeMissing)?;
                    let latest = store::latest_versions(tx, &scope, tenant_id, &[t.id])
                        .await
                        .map_err(TxError::Repo)?
                        .get(&t.id)
                        .map(|row| row.version)
                        .ok_or_else(|| {
                            TxError::Repo(RepoError::CorruptRow(format!(
                                "derived usage type {} has no version",
                                t.id
                            )))
                        })?;
                    let version = latest.checked_add(1).ok_or_else(|| {
                        TxError::Repo(RepoError::CorruptRow(format!(
                            "derived usage type {} has no version after {latest}",
                            t.id
                        )))
                    })?;
                    let v = store::insert_version(
                        tx,
                        &scope,
                        tenant_id,
                        NewDerivedVersion {
                            type_id: t.id,
                            version,
                            declaration_json,
                            digest,
                            created_by: actor,
                            created_at: now,
                        },
                    )
                    .await
                    .map_err(write_error)?;
                    audit(tx, &scope, tenant_id, actor, ACTION_VERSION, &v).await?;
                    replay::finish(
                        tx,
                        tenant_id,
                        claim.as_ref(),
                        StatusCode::CREATED,
                        &version_dto(&t, &v).map_err(TxError::Repo)?,
                    )
                    .await
                })
            },
        )
        .await
        .map_err(|e| match e {
            // The type went between the check and the write: the same 404, by its code.
            TxError::DerivedTypeMissing => not_found(),
            other => tx_to_canonical(other),
        })
}

/// The refusals of this module's writes; any other repository failure stays one.
fn write_error(e: RepoError) -> TxError {
    match e {
        RepoError::Refused(RepoRefusal::DerivedCodeTaken) => {
            TxError::Refused(DomainError::Conflict {
                code: CODE_TAKEN,
                detail: "the tenant has a derived usage type with this code".into(),
            })
        }
        RepoError::Refused(RepoRefusal::DerivedVersionTaken) => {
            TxError::Refused(DomainError::Conflict {
                code: "CONTENDED",
                detail: "a concurrent write took this version number; retry".into(),
            })
        }
        other => TxError::Repo(other),
    }
}

/// @cpt-cf-bss-products-fr-derived-usage-type
async fn list_derived_usage_types(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    query: RawQuery,
    odata: Result<OData, CanonicalError>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // Authorization first, then the query (a 403 before a 400).
    let scope = read_scope(&enforcer, &ctx).await?.tenant_only();
    params(query, &["limit", "cursor"], Some(&["$top", "$skiptoken"]))?;
    let OData(mut odata) = odata?;
    // The cursor names this list: one another list cut is refused, not misread.
    let hash = cursor_hash(&serde_json::json!({ "derived_usage_types": null }));
    if let Some(cursor) = &odata.cursor
        && cursor.f.as_deref() != Some(hash.as_str())
    {
        return Err(ODataError::FilterMismatch.into());
    }
    odata.filter_hash = Some(hash);
    let tenant = ctx.subject_tenant_id();
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    let page = store::list(&conn, &scope, tenant, &odata)
        .await
        .map_err(|e| match e {
            SkuListError::Query(e) => CanonicalError::from(e),
            SkuListError::Repo(e) => stored_row_error(&e),
        })?;
    let ids: Vec<Uuid> = page.items.iter().map(|t| t.id).collect();
    let latest = store::latest_versions(&conn, &scope, tenant, &ids)
        .await
        .map_err(|e| stored_row_error(&e))?;
    let mut items: Vec<ProductsDerivedUsageTypeItem> = page
        .items
        .into_iter()
        .map(|t| {
            let row = latest.get(&t.id).ok_or_else(|| {
                RepoError::CorruptRow(format!("derived usage type {} has no version", t.id))
            })?;
            let latest_version = row.version;
            let latest_row = version_dto(&t, row)?;
            Ok(ProductsDerivedUsageTypeItem {
                id: t.id,
                code: t.code,
                name: t.name,
                latest_version,
                latest: latest_row,
                created_by: t.created_by,
                created_by_name: None,
                created_at: t.created_at,
            })
        })
        .collect::<Result<_, RepoError>>()
        .map_err(|e| stored_row_error(&e))?;
    // P-D-262: the page's creators in one lookup; the weak tag covers their names.
    state.actor_names.fill(&ctx, &mut items).await;
    Ok(respond(
        &headers,
        &Page {
            items,
            page_info: page.page_info,
        },
        PRIVATE_REVALIDATE,
    ))
}

/// @cpt-cf-bss-products-fr-derived-usage-type
async fn get_derived_usage_type(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(code): Path<String>,
) -> Result<Json<ProductsDerivedUsageType>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = read_scope(&enforcer, &ctx).await?.tenant_only();
    path_code(&code)?;
    let tenant = ctx.subject_tenant_id();
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    let t = store::find_type(&conn, &scope, tenant, &code)
        .await
        .map_err(|e| stored_row_error(&e))?
        .ok_or_else(not_found)?;
    let versions = store::list_versions(&conn, &scope, tenant, t.id)
        .await
        .map_err(|e| stored_row_error(&e))?
        .iter()
        .map(|v| {
            Ok(ProductsDerivedVersionHeader {
                version: v.version,
                digest: v.digest.clone(),
                meter_ref: meter(&t, v.version)?,
                accrual_policy_version: v.accrual_policy_version(),
                created_by: v.created_by,
                created_by_name: None,
                created_at: v.created_at,
            })
        })
        .collect::<Result<_, RepoError>>()
        .map_err(|e| stored_row_error(&e))?;
    let mut body = ProductsDerivedUsageType {
        id: t.id,
        code: t.code,
        name: t.name,
        created_by: t.created_by,
        created_by_name: None,
        created_at: t.created_at,
        versions,
    };
    // P-D-262: the type's and its versions' creators in one lookup.
    state.actor_names.fill(&ctx, &mut body).await;
    Ok(Json(body))
}

/// @cpt-cf-bss-products-fr-derived-usage-type
async fn get_derived_usage_type_version(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path((code, n)): Path<(String, String)>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = read_scope(&enforcer, &ctx).await?.tenant_only();
    path_code(&code)?;
    let Ok(version) = MeterId::parse_version(&n) else {
        return Err(not_found());
    };
    let tenant = ctx.subject_tenant_id();
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    let t = store::find_type(&conn, &scope, tenant, &code)
        .await
        .map_err(|e| stored_row_error(&e))?
        .ok_or_else(not_found)?;
    let v = store::find_version(&conn, &scope, tenant, t.id, version)
        .await
        .map_err(|e| stored_row_error(&e))?
        .ok_or_else(not_found)?;
    let mut body = version_dto(&t, &v).map_err(|e| stored_row_error(&e))?;
    // P-D-262: the version's creator.
    state.actor_names.fill(&ctx, &mut body).await;
    Ok(Json(body).into_response())
}

/// The tenant's stored version that `meter` names, read under `scope`, with the declaration as the doors serve it: what
/// the SKU binding ([`pin`], P-D-232) and the meter-semantics provider (P-D-233) read. `None` when the tenant holds no
/// such code or version. The usage-type catalog is never asked: a derived usage type is this gear's own data.
///
/// # Errors
/// A storage failure, or a stored row that does not read (a corrupt row).
pub(crate) async fn stored_version(
    conn: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    meter: &MeterId,
) -> Result<Option<(DerivedUsageTypeVersion, ProductsDerivedDeclaration)>, RepoError> {
    let Some(t) = store::find_type(conn, scope, tenant, meter.code()).await? else {
        return Ok(None);
    };
    let Some(v) = store::find_version(conn, scope, tenant, t.id, meter.version()).await? else {
        return Ok(None);
    };
    let declaration = declaration_of(&t, &v)?;
    Ok(Some((v, declaration)))
}

/// The tenant's stored declaration that `reference` names, through the same read as [`pin`]
/// ([`stored_version`], under the caller's tenant scope). `None` when the ref is not a canonical
/// meter id or the tenant holds no such version. A stored row that does not read is a corrupt row.
///
/// # Errors
/// A storage failure, or a stored declaration that does not read.
pub(crate) async fn stored_declaration(
    conn: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    reference: &str,
) -> Result<Option<DerivedUsageDeclaration>, RepoError> {
    let Ok(meter) = MeterId::parse(reference) else {
        return Ok(None);
    };
    let Some((_, declaration)) = stored_version(conn, scope, tenant, &meter).await? else {
        return Ok(None);
    };
    DerivedUsageDeclaration::try_from(&declaration)
        .map(Some)
        .map_err(|error| RepoError::CorruptRow(format!("derived usage type {reference}: {error}")))
}

/// The proposed version when a change might be a wrap (P-D-251): [`pin`]'s tenant-scoped read, or
/// `None` when the current ref is not raw or the proposed ref is not derived. A missing version
/// is `None`.
///
/// # Errors
/// A storage failure, a stored row that does not read, or a `sku:read` denial.
pub(super) async fn wrap_declaration(
    state: &ApiState,
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    current_ref: Option<&str>,
    proposed_ref: Option<&str>,
) -> Result<Option<DerivedUsageDeclaration>, CanonicalError> {
    let Some(reference) = proposed_ref else {
        return Ok(None);
    };
    if !derived::wrap_candidate(current_ref, Some(reference)) {
        return Ok(None);
    }
    let scope = read_scope(enforcer, ctx).await?.tenant_only();
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    stored_declaration(&conn, &scope, ctx.subject_tenant_id(), reference)
        .await
        .map_err(|e| stored_row_error(&e))
}

/// A usage SKU's derived `reference` as the tenant's store holds it (P-D-232): the version its meter
/// id names and that version's output unit, or `None` when the ref is not canonical or the tenant
/// holds no such code or version. The read is `tenant_only()` of `sku:read`, beside the caller's tenant, the same
/// scope the meter provider uses. The usage-type catalog is never asked: a derived usage type is this gear's own data.
///
/// # Errors
/// A storage failure, or a stored declaration that does not read (a corrupt row): 500.
pub(super) async fn pin(
    state: &ApiState,
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    reference: &str,
) -> Result<Option<DerivedPin>, CanonicalError> {
    let Ok(meter) = MeterId::parse(reference) else {
        return Ok(None);
    };
    // The same tenant scope the provider reads under, so the pin and the answer see the same rows.
    let scope = read_scope(enforcer, ctx).await?.tenant_only();
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    let stored = stored_version(&conn, &scope, ctx.subject_tenant_id(), &meter)
        .await
        .map_err(|e| stored_row_error(&e))?;
    Ok(stored.map(|(_, declaration)| DerivedPin {
        meter: meter.format(),
        output_unit: declaration.output_unit,
    }))
}

#[cfg(test)]
#[path = "derived_usage_types_tests.rs"]
mod derived_usage_types_tests;
