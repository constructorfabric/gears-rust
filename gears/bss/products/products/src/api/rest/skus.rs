//! SKU draft authoring, registry reference counts, version history and scoped search.
//! @cpt-dod:cpt-cf-bss-products-dod-list-search:p1
//! @cpt-dod:cpt-cf-bss-products-dod-card-with-references:p1
//! @cpt-dod:cpt-cf-bss-products-dod-sku-create-unique:p1
use super::closed_sets::{ProductsReferenceKind, ProductsReferenceState};
use super::{
    ApiState, ArchiveMove, TxError, authz_error_to_canonical, category_tx_config,
    contention_db_err,
    dto::{ReferencesDto, SkuCard, SkuDto, SkuPatchRequest, SkuRequest, SkuVersionDto},
    json_body,
    preconditions::{etag, if_match, if_match_param},
    replay, repo_error_to_canonical, require_authenticated,
    sku_list::{RawQuery, UNSUPPORTED, refused},
    tx_to_canonical,
};
use crate::{
    authz::{access_scope, actions, resource_types},
    domain::{
        concurrency::InternalRevision,
        derived,
        error::DomainError,
        recognized::UsageTypeAnswer,
        sku::{NewSku, SkuPatch, apply_patch, validate_new},
        validation::ValidationReport,
    },
    infra::storage::{
        RepoError, RepoRefusal,
        repo::{self, HeadWrite},
    },
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Json, Router,
    extract::rejection::{JsonRejection, QueryRejection},
    extract::{Path, Query},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use bss_products_sdk::models::{Lifecycle, Sku, SkuContent};
use std::sync::Arc;
use time::{Date, OffsetDateTime};
use toolkit::api::{
    OpenApiRegistry,
    canonical_prelude::{CanonicalError, resource_error},
    operation_builder::OperationBuilder,
};
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_odata::errors::OdataError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

const SKUS: &str = "/bss-products/v1/skus";
const TAG: &str = "SKUs";
#[resource_error(gts_id!("cf.bss.products.sku.v1~"))]
struct SkuResource;

#[toolkit_macros::api_dto(request)]
struct ReferenceQuery {
    #[serde(default)]
    include_released: bool,
}
#[toolkit_macros::api_dto(response)]
struct ReferenceDto {
    id: Uuid,
    /// The owning gear: a string, since no CHECK holds the column to a set (P-D-217).
    owner: String,
    kind: ProductsReferenceKind,
    ref_id: Uuid,
    state: ProductsReferenceState,
    #[serde(with = "time::serde::rfc3339")]
    reserved_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    released_at: Option<OffsetDateTime>,
    released_by: Option<Uuid>,
    forced: bool,
    release_reason: Option<String>,
}
impl TryFrom<repo::SkuReference> for ReferenceDto {
    type Error = RepoError;
    fn try_from(r: repo::SkuReference) -> Result<Self, RepoError> {
        Ok(Self {
            id: r.id,
            owner: r.owner_gear,
            kind: ProductsReferenceKind::stored(
                &r.ref_kind,
                &format_args!("reference {} ref_kind", r.id),
            )?,
            ref_id: r.ref_id,
            state: ProductsReferenceState::stored(
                &r.state,
                &format_args!("reference {} state", r.id),
            )?,
            reserved_at: r.reserved_at,
            released_at: r.released_at,
            released_by: r.released_by,
            forced: r.forced,
            release_reason: r.release_reason,
        })
    }
}
#[toolkit_macros::api_dto(response)]
struct ReferenceList {
    summary: ReferencesDto,
    items: Vec<ReferenceDto>,
}

/// Register the SKU operations and their concrete response schemas.
pub(crate) fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post(SKUS)
        .operation_id("bss_products.create_sku")
        .summary("Create a draft SKU")
        .description(
            "Creates a draft SKU of the tenant. Its texts have explicit caps, in characters \
             (P-D-225): code 64, name 200, description 2000, gl_code, tax_category and unit 64, \
             invoice_line_template 2000, usage_type_ref 512. A usage_type_ref of the form \
             `products.derived/<code>@<n>` names one version of the tenant's derived usage type, \
             read from this gear's store; the usage-type catalog is never asked for it (P-D-232). \
             Refusals: 400 VALIDATION, or 400 FIELD_TOO_LONG on a text over its cap; 400 \
             USAGE_TYPE_UNRESOLVED for a GTS ref the configured catalog does not know; 400 \
             DERIVED_USAGE_TYPE_UNKNOWN for a derived ref the tenant does not hold, or one not in \
             that form; 400 DERIVED_UNIT_MISMATCH for a unit other than that version's output \
             unit; 404 for a category the tenant does not hold; 409 SKU_CODE_TAKEN, SKU_NAME_TAKEN \
             or CATEGORY_RETIRED.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<SkuRequest>(openapi, "Draft business fields")
        .param(replay::param())
        .handler(create_sku)
        .json_response_with_schema::<SkuDto>(openapi, StatusCode::CREATED, "Create a draft SKU.")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(Router::new(), openapi);
    // The list and its counts (P-D-210, P-D-211).
    let router = super::sku_list::register(router, openapi);
    let router = OperationBuilder::get(format!("{SKUS}/{{id}}"))
        .operation_id("bss_products.get_sku")
        .summary("Read a SKU card")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "SKU id")
        .handler(get_sku)
        .json_response_with_schema::<SkuCard>(openapi, StatusCode::OK, "Read a SKU card.")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::patch(format!("{SKUS}/{{id}}"))
        .operation_id("bss_products.update_sku_draft")
        .summary("Edit a draft SKU")
        .description(
            "Edits a draft SKU under If-Match; a field the body leaves out is unchanged. The texts \
             it carries have the caps of the create (P-D-225). A draft never published may move \
             its derived usage type to another version (P-D-232). Refusals include 400 \
             VALIDATION, 400 FIELD_TOO_LONG on a text over its cap, 400 USAGE_TYPE_UNRESOLVED, \
             400 DERIVED_USAGE_TYPE_UNKNOWN and DERIVED_UNIT_MISMATCH as at the create, 404, and \
             409 NOT_A_DRAFT, ROW_LOCKED_PENDING, STALE_REVISION, SKU_NAME_TAKEN or \
             CATEGORY_RETIRED.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "SKU id")
        .param(if_match_param())
        .json_request::<SkuPatchRequest>(openapi, "Draft business fields")
        .handler(update_sku_draft)
        .json_response_with_schema::<SkuDto>(openapi, StatusCode::OK, "Edit a draft SKU.")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::delete(format!("{SKUS}/{{id}}"))
        .operation_id("bss_products.delete_sku_draft")
        .summary("Delete a never-published draft SKU")
        .description(
            "Deletes a draft that was never published, at the revision the caller read \
             (If-Match); only its author may (P-D-206). A draft is deleted, never retired. The \
             SKU's audit rows stay, and a rejected or withdrawn unit that named it stays readable. \
             Refusals: 403 NOT_DRAFT_AUTHOR; 400 for a missing or malformed If-Match; 404; 409 \
             SKU_NOT_DRAFT (published once, or not a draft), ROW_LOCKED_PENDING, SKU_REFERENCED \
             or STALE_REVISION.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "SKU id")
        .param(if_match_param())
        .handler(delete_sku_draft)
        .no_content_response(StatusCode::NO_CONTENT, "Deleted")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    // P-D-214: the history is always an array; the version in force on a date has its own path.
    let router = OperationBuilder::get(format!("{SKUS}/{{id}}/versions"))
        .operation_id("bss_products.sku_versions")
        .summary("Read a SKU's version history")
        .description(
            "Every published version of the SKU, oldest first, as an array (empty before the \
             first publication). The version in force on a date is \
             `GET /skus/{id}/versions/as-of?date=` (P-D-214); any query key here is 400.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "SKU id")
        .handler(sku_versions)
        .json_array_response_with_schema::<SkuVersionDto>(
            openapi,
            StatusCode::OK,
            "The SKU's versions, oldest first.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get(format!("{SKUS}/{{id}}/versions/as-of"))
        .operation_id("bss_products.sku_version_as_of")
        .summary("Read the SKU version in force on a date")
        .description(
            "The version with the greatest effective_from not after `date`, then the greatest \
             published_version (P-D-191, P-D-214). 404 with reason NO_VERSION_IN_FORCE before the \
             first version; a missing or malformed `date`, or any other key, is 400.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "SKU id")
        .query_param_typed(
            "date",
            true,
            "The date (YYYY-MM-DD) the version is in force on",
            "string",
        )
        .handler(sku_version_as_of)
        .json_response_with_schema::<SkuVersionDto>(
            openapi,
            StatusCode::OK,
            "The version in force on the date.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    // The SKU's history (P-D-213).
    let router = super::sku_history::register(router, openapi);
    let router = OperationBuilder::get(format!("{SKUS}/{{id}}/references"))
        .operation_id("bss_products.sku_references")
        .summary("Read reference details and optional released history")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "SKU id")
        .query_param_typed(
            "include_released",
            false,
            "Include released reference history (default false)",
            "boolean",
        )
        .handler(sku_references)
        .json_response_with_schema::<ReferenceList>(
            openapi,
            StatusCode::OK,
            "Read reference details and optional released history.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = archive_routes(router, openapi);
    router.layer(Extension(state))
}

/// The archive mark of a retired SKU (P-D-263): `archive` and `unarchive`.
fn archive_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post(format!("{SKUS}/{{id}}/archive"))
        .operation_id("bss_products.archive_sku")
        .summary("Archive a retired SKU")
        .description(
            "Marks a retired SKU archived at the revision the caller read (If-Match), with an \
             audit row: `archived_at` and `archived_by` are set and the revision moves (P-D-263). \
             The mark is not a lifecycle: the SKU stays retired. `GET /skus`, its counts and the \
             pickers leave an archived SKU out unless asked `archived eq true`; a read by id, the \
             browse, the consumer reads and the pinned facts ignore the mark. Archiving an \
             archived SKU answers it unchanged. Refusals: 403 without SKU author; 400 for a \
             missing or malformed If-Match; 404; 409 STALE_REVISION, or SKU_NOT_RETIRED for a SKU \
             whose lifecycle in force is not retired.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "SKU id")
        .param(if_match_param())
        .handler(archive_sku)
        .json_response_with_schema::<SkuDto>(
            openapi,
            StatusCode::OK,
            "The archived SKU; ETag carries its revision.",
        )
        .response_header(super::preconditions::etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::post(format!("{SKUS}/{{id}}/unarchive"))
        .operation_id("bss_products.unarchive_sku")
        .summary("Unarchive a SKU")
        .description(
            "Clears a SKU's archive mark at the revision the caller read (If-Match), with an \
             audit row; the SKU is listed again, still retired (P-D-263). Unarchiving a SKU that \
             is not archived answers it unchanged. Refusals: 403 without SKU author; 400 for a \
             missing or malformed If-Match; 404; 409 STALE_REVISION.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "SKU id")
        .param(if_match_param())
        .handler(unarchive_sku)
        .json_response_with_schema::<SkuDto>(
            openapi,
            StatusCode::OK,
            "The unarchived SKU; ETag carries its revision.",
        )
        .response_header(super::preconditions::etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}
/// Authorize reads without an owner hint and writes against the subject tenant. The call site names
/// the action it asks (`actions::READ` or `actions::AUTHOR`), not a `bool` (RS-54); a write
/// anchors to the subject's tenant.
async fn scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    action: &'static str,
) -> Result<AccessScope, CanonicalError> {
    access_scope(
        enforcer,
        ctx,
        &resource_types::SKU,
        action,
        (action != actions::READ).then(|| ctx.subject_tenant_id()),
    )
    .await
    .map_err(|e| {
        authz_error_to_canonical(e, |reason| {
            SkuResource::permission_denied()
                .with_reason(reason)
                .create()
        })
    })
}
fn response(status: StatusCode, s: Sku) -> Response {
    (
        status,
        [(header::ETAG, etag(InternalRevision::new(s.revision)))],
        Json(SkuDto::from(s)),
    )
        .into_response()
}
/// Translate the repository's refusals a SKU write can meet, retaining all driver errors. A SKU
/// without a category (P-D-196) resolves none, so only a named category can be missing. The
/// refusals a SKU write cannot meet stay a repository failure (RS-16: an exhaustive match).
fn write_error(e: RepoError, category_id: Option<Uuid>) -> TxError {
    let (code, detail) = match e {
        RepoError::Refused(RepoRefusal::CategoryNotFound) => {
            return match category_id {
                Some(id) => TxError::Refused(DomainError::NotFound {
                    what: "category",
                    id,
                }),
                None => TxError::Repo(e),
            };
        }
        RepoError::Refused(RepoRefusal::SkuCodeTaken) => {
            ("SKU_CODE_TAKEN", "a SKU with this code exists")
        }
        RepoError::Refused(RepoRefusal::SkuNameTaken) => {
            ("SKU_NAME_TAKEN", "a SKU with this name exists")
        }
        RepoError::Refused(RepoRefusal::CategoryRetired) => {
            ("CATEGORY_RETIRED", "the category is retired")
        }
        RepoError::Refused(
            RepoRefusal::CategoryCodeTaken
            | RepoRefusal::CategoryDefaultTaken
            | RepoRefusal::ReferenceExists
            | RepoRefusal::VersionOrder
            | RepoRefusal::DerivedCodeTaken
            | RepoRefusal::DerivedVersionTaken,
        )
        | RepoError::Db(_)
        | RepoError::Driver { .. }
        | RepoError::CorruptRow(_) => return TxError::Repo(e),
    };
    TxError::Refused(DomainError::Conflict {
        code,
        detail: detail.into(),
    })
}
/// P-D-184: a configured catalog's definite unknown refuses; silence allows draft save. A catalog
/// that refuses the caller (P-D-207) is not a verdict on the ref either, so the save proceeds; the
/// submit and the approve answer it 403 `USAGE_TYPE_FORBIDDEN`.
async fn resolve_draft_ref(
    state: &ApiState,
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    reference: Option<&str>,
    unit: Option<&str>,
) -> Result<(), CanonicalError> {
    // P-D-259: a raw ref is refused before any catalog is asked. A missing ref is a draft that
    // names its meter later.
    // PROBE-9-13-4: a raw ref is refused here, before the catalog.
    if reference.is_some_and(|reference| !derived::is_derived_ref(reference)) {
        return Err(derived::usage_type_required().into());
    }
    // P-D-232: a derived ref comes first, before the unconfigured catalog's early `Ok`: it is this
    // gear's own data, judged from its store, and the catalog is never asked for it.
    if let Some(reference) = reference.filter(|reference| derived::is_derived_ref(reference)) {
        let pin = super::derived_usage_types::pin(state, enforcer, ctx, reference).await?;
        let mut report = ValidationReport::new();
        derived::judge_binding(&mut report, reference, unit, pin.as_ref());
        return if report.is_empty() {
            Ok(())
        } else {
            Err(DomainError::Validation(report).into())
        };
    }
    if state.usage_type_catalog_source == crate::gear::USAGE_TYPE_SOURCE_UNCONFIGURED {
        return Ok(());
    }
    if let Some(reference) = reference
        && matches!(
            state.usage_type_catalog.resolve(ctx, reference).await,
            UsageTypeAnswer::Unresolved
        )
    {
        let mut report = ValidationReport::new();
        report.violate(
            "USAGE_TYPE_UNRESOLVED",
            "usage_type_ref",
            "the usage type catalog does not know this ref",
        );
        return Err(DomainError::Validation(report).into());
    }
    Ok(())
}
/// @cpt-cf-bss-products-fr-sku-define
async fn create_sku(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = ctx.subject_tenant_id();
    let actor = ctx.subject_id();
    let scope_tx = scope(&enforcer, &ctx, actions::AUTHOR).await?;
    let payload = json_body(body)?;
    let claim = replay::input(&state, &headers, "/bss-products/v1/skus".into(), &payload)?;
    if let Some(response) = replay::lookup(
        &state.db.conn().map_err(|e| tx_to_canonical(e.into()))?,
        tenant_id,
        claim.as_ref(),
    )
    .await
    .map_err(tx_to_canonical)?
    {
        return Ok(response);
    }
    let body: SkuRequest = serde_json::from_value(payload)
        .map_err(|e| CanonicalError::from(super::governance::validation("body", e.to_string())))?;
    let new_tx = NewSku::try_from(body).map_err(DomainError::Validation)?;
    let report = validate_new(&new_tx);
    if !report.is_empty() {
        return Err(DomainError::Validation(report).into());
    }
    resolve_draft_ref(
        &state,
        &enforcer,
        &ctx,
        new_tx.usage_type_ref.as_deref(),
        new_tx.unit.as_deref(),
    )
    .await?;
    let now = crate::infra::storage::stored_now();
    let created = state
        .db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            category_tx_config(&state),
            contention_db_err,
            move |tx| {
                let scope = scope_tx.clone();
                let new = new_tx.clone();
                let claim = claim.clone();
                Box::pin(async move {
                    if let Some(response) = replay::begin(tx, tenant_id, claim.as_ref()).await? {
                        return Ok(response);
                    }
                    let category = new.category_id;
                    let s = repo::insert_sku(tx, &scope, tenant_id, new, actor, now)
                        .await
                        .map_err(|e| write_error(e, category))?;
                    let created = repo::LifecycleMove {
                        from: None,
                        to: Some(s.lifecycle),
                    };
                    audit(tx, &scope, tenant_id, actor, "sku.create", &s, now, created).await?;
                    replay::finish(
                        tx,
                        tenant_id,
                        claim.as_ref(),
                        StatusCode::CREATED,
                        &SkuDto::from(s),
                    )
                    .await
                })
            },
        )
        .await
        .map_err(tx_to_canonical)?;
    Ok(created)
}
/// Read a head with a scoped 404 for absence or a foreign tenant.
async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<Sku, TxError> {
    repo::find_sku(runner, scope, tenant, id)
        .await
        .map_err(TxError::Repo)?
        .ok_or(TxError::Refused(DomainError::NotFound { what: "sku", id }))
}
/// Check editing eligibility before resolving a ref and again within the write transaction.
/// A draft belongs to its author (D-404): only its creator edits it.
fn editable(s: &Sku, expected: i64, actor: Uuid) -> Result<(), TxError> {
    if s.lifecycle != Lifecycle::Draft {
        return Err(TxError::Refused(DomainError::Conflict {
            code: "NOT_A_DRAFT",
            detail: format!("use POST /skus/{}/changes", s.id),
        }));
    }
    if s.pending_unit_id.is_some() {
        return Err(TxError::Refused(DomainError::Conflict {
            code: "ROW_LOCKED_PENDING",
            detail: "a pending approval unit locks this draft".into(),
        }));
    }
    if s.created_by != actor {
        return Err(TxError::Refused(DomainError::Forbidden {
            code: "NOT_DRAFT_AUTHOR",
            detail: "only the draft's author edits it".into(),
        }));
    }
    if s.revision != expected {
        return Err(TxError::Refused(DomainError::StaleRevision {
            expected,
            found: s.revision,
        }));
    }
    Ok(())
}
/// @cpt-cf-bss-products-fr-concurrency-idempotency
async fn update_sku_draft(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<SkuPatchRequest>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = ctx.subject_tenant_id();
    let actor = ctx.subject_id();
    let scope_tx = scope(&enforcer, &ctx, actions::AUTHOR).await?;
    let expected = if_match(&headers)?.get();
    let patch_tx = SkuPatch::try_from(json_body(body)?).map_err(DomainError::Validation)?;
    let mut report = ValidationReport::new();
    if patch_tx.name.as_deref() == Some("") {
        report.violate("VALIDATION", "name", "name must not be blank");
    }
    crate::domain::sku::check_patch(&mut report, &patch_tx);
    if patch_tx.lifecycle.is_some_and(|s| s != Lifecycle::Draft) {
        report.violate(
            "VALIDATION",
            "lifecycle",
            "a draft changes lifecycle through its submit door",
        );
    }
    if !report.is_empty() {
        return Err(DomainError::Validation(report).into());
    }
    // Resolve only a changed proposed ref outside the transaction. The revision check inside
    // guarantees this answer cannot be applied to a different head after a concurrent edit.
    let current = {
        super::governance::touch(&state, &scope_tx, ctx.subject_tenant_id(), id).await?;
        let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
        find(&conn, &scope_tx, tenant_id, id)
            .await
            .map_err(tx_to_canonical)?
    };
    editable(&current, expected, actor).map_err(tx_to_canonical)?;
    let proposed = apply_patch(&SkuContent::from(&current), &patch_tx);
    // P-D-259: a draft whose result still names a raw ref is refused, a name change included.
    // A patch onto a derived ref is the raw draft's way forward.
    if proposed.r#type == bss_products_sdk::models::SkuType::Usage
        && proposed
            .usage_type_ref
            .as_deref()
            .is_some_and(|reference| !derived::is_derived_ref(reference))
    {
        return Err(derived::usage_type_required().into());
    }
    // A derived ref is judged again when the unit it must match changes (P-D-232).
    let derived_unit_moved = proposed
        .usage_type_ref
        .as_deref()
        .is_some_and(derived::is_derived_ref)
        && proposed.unit != current.unit;
    if proposed.usage_type_ref != current.usage_type_ref || derived_unit_moved {
        resolve_draft_ref(
            &state,
            &enforcer,
            &ctx,
            proposed.usage_type_ref.as_deref(),
            proposed.unit.as_deref(),
        )
        .await?;
    }
    let now = crate::infra::storage::stored_now();
    let updated = state
        .db
        .db()
        .transaction_with_retry::<Sku, TxError, _, _>(
            category_tx_config(&state),
            contention_db_err,
            move |tx| {
                let scope = scope_tx.clone();
                let patch = patch_tx.clone();
                Box::pin(async move {
                    let current = find(tx, &scope, tenant_id, id).await?;
                    editable(&current, expected, actor)?;
                    let content = apply_patch(&SkuContent::from(&current), &patch);
                    let s = match repo::update_sku_draft(
                        tx, &scope, tenant_id, id, expected, &content, now,
                    )
                    .await
                    .map_err(|e| write_error(e, content.category_id))?
                    {
                        HeadWrite::Written(s) => s,
                        HeadWrite::Unmatched => {
                            let latest = find(tx, &scope, tenant_id, id).await?;
                            return Err(TxError::Refused(DomainError::StaleRevision {
                                expected,
                                found: latest.revision,
                            }));
                        }
                    };
                    let moved = repo::LifecycleMove::between(current.lifecycle, s.lifecycle);
                    audit(
                        tx,
                        &scope,
                        tenant_id,
                        actor,
                        "sku.draft_update",
                        &s,
                        now,
                        moved,
                    )
                    .await?;
                    Ok(s)
                })
            },
        )
        .await
        .map_err(tx_to_canonical)?;
    Ok(response(StatusCode::OK, updated))
}
/// P-D-206: only a never-published draft is deleted, as only it is edited: by its author, unlocked,
/// at the revision the caller read. The refusals come in the draft PATCH's order.
fn deletable(s: &Sku, expected: i64, actor: Uuid) -> Result<(), TxError> {
    if s.lifecycle != Lifecycle::Draft || s.published_version != 0 {
        return Err(TxError::Refused(DomainError::Conflict {
            code: "SKU_NOT_DRAFT",
            detail: "only a never-published draft is deleted; retire a published SKU".into(),
        }));
    }
    if s.pending_unit_id.is_some() {
        return Err(TxError::Refused(DomainError::Conflict {
            code: "ROW_LOCKED_PENDING",
            detail: "a pending approval unit locks this draft".into(),
        }));
    }
    if s.created_by != actor {
        return Err(TxError::Refused(DomainError::Forbidden {
            code: "NOT_DRAFT_AUTHOR",
            detail: "only the draft's author deletes it".into(),
        }));
    }
    if s.revision != expected {
        return Err(TxError::Refused(DomainError::StaleRevision {
            expected,
            found: s.revision,
        }));
    }
    Ok(())
}
/// @cpt-cf-bss-products-fr-sku-lifecycle
async fn delete_sku_draft(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = ctx.subject_tenant_id();
    let actor = ctx.subject_id();
    // Authorization first, then the precondition (as the draft PATCH).
    let scope_tx = scope(&enforcer, &ctx, actions::AUTHOR).await?;
    let expected = if_match(&headers)?.get();
    let now = crate::infra::storage::stored_now();
    state
        .db
        .db()
        .transaction_with_retry::<(), TxError, _, _>(
            category_tx_config(&state),
            contention_db_err,
            move |tx| {
                let scope = scope_tx.clone();
                Box::pin(async move {
                    let current = find(tx, &scope, tenant_id, id).await?;
                    deletable(&current, expected, actor)?;
                    // A draft admits no reservation, so no registry row can name it; were one to,
                    // its row would keep the SKU's key, and the delete is refused rather than
                    // dropping it.
                    if !repo::list_references(
                        tx,
                        &AccessScope::for_tenant(tenant_id),
                        tenant_id,
                        id,
                        true,
                    )
                    .await
                    .map_err(TxError::Repo)?
                    .is_empty()
                    {
                        return Err(TxError::Refused(DomainError::Conflict {
                            code: "SKU_REFERENCED",
                            detail: "the reference registry holds a row naming this SKU".into(),
                        }));
                    }
                    if !repo::delete_draft_sku(tx, &scope, tenant_id, id, expected)
                        .await
                        .map_err(TxError::Repo)?
                    {
                        // Lost to a concurrent writer after the read: say what it left.
                        let latest = find(tx, &scope, tenant_id, id).await?;
                        deletable(&latest, expected, actor)?;
                        return Err(TxError::Refused(DomainError::StaleRevision {
                            expected,
                            found: latest.revision,
                        }));
                    }
                    let deleted = repo::LifecycleMove {
                        from: Some(current.lifecycle),
                        to: None,
                    };
                    audit(
                        tx,
                        &scope,
                        tenant_id,
                        actor,
                        "sku.delete",
                        &current,
                        now,
                        deleted,
                    )
                    .await
                })
            },
        )
        .await
        .map_err(tx_to_canonical)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
/// `POST /skus/{id}/archive` (P-D-263).
async fn archive_sku(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    mark_archived(
        &state,
        &enforcer,
        extension_ctx,
        id,
        &headers,
        ArchiveMove::Archive,
    )
    .await
}
/// `POST /skus/{id}/unarchive` (P-D-263).
async fn unarchive_sku(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    mark_archived(
        &state,
        &enforcer,
        extension_ctx,
        id,
        &headers,
        ArchiveMove::Unarchive,
    )
    .await
}
/// Set ([`ArchiveMove::Archive`]) or clear a SKU's archive mark (P-D-263) under SKU author, at the revision the
/// caller read, with an audit row in the same transaction. Only a SKU whose lifecycle in force is
/// retired takes the mark (409 `SKU_NOT_RETIRED`); a stale tag is judged first, as on every write
/// of a head. A SKU already in the asked state is answered as it is, and nothing is written.
/// @cpt-cf-bss-products-fr-sku-lifecycle
async fn mark_archived(
    state: &Arc<ApiState>,
    enforcer: &PolicyEnforcer,
    extension_ctx: Option<Extension<SecurityContext>>,
    id: Uuid,
    headers: &HeaderMap,
    mark: ArchiveMove,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = ctx.subject_tenant_id();
    let actor = ctx.subject_id();
    // Authorization first, then the precondition (as the draft PATCH).
    let scope_tx = scope(enforcer, &ctx, actions::AUTHOR).await?;
    let expected = if_match(headers)?.get();
    let now = crate::infra::storage::stored_now();
    let marked = state
        .db
        .db()
        .transaction_with_retry::<Sku, TxError, _, _>(
            category_tx_config(state),
            contention_db_err,
            move |tx| {
                let scope = scope_tx.clone();
                Box::pin(async move {
                    let current = find(tx, &scope, tenant_id, id).await?;
                    if current.revision != expected {
                        return Err(TxError::Refused(DomainError::StaleRevision {
                            expected,
                            found: current.revision,
                        }));
                    }
                    if mark == ArchiveMove::Archive && current.lifecycle != Lifecycle::Retired {
                        return Err(TxError::Refused(DomainError::Conflict {
                            code: "SKU_NOT_RETIRED",
                            detail: "only a retired SKU is archived; retire it first".into(),
                        }));
                    }
                    if mark.already(current.archived_at.is_some()) {
                        return Ok(current);
                    }
                    let s = match repo::set_sku_archived(
                        tx,
                        &scope,
                        tenant_id,
                        id,
                        expected,
                        mark.archived_by(actor),
                        now,
                    )
                    .await
                    .map_err(TxError::Repo)?
                    {
                        HeadWrite::Written(s) => s,
                        HeadWrite::Unmatched => {
                            let latest = find(tx, &scope, tenant_id, id).await?;
                            return Err(TxError::Refused(DomainError::StaleRevision {
                                expected,
                                found: latest.revision,
                            }));
                        }
                    };
                    let action = match mark {
                        ArchiveMove::Archive => "sku.archive",
                        ArchiveMove::Unarchive => "sku.unarchive",
                    };
                    audit(
                        tx,
                        &scope,
                        tenant_id,
                        actor,
                        action,
                        &s,
                        now,
                        repo::LifecycleMove::NONE,
                    )
                    .await?;
                    Ok(s)
                })
            },
        )
        .await
        .map_err(tx_to_canonical)?;
    Ok(response(StatusCode::OK, marked))
}
/// Read a SKU and live reference counts from this gear's registry.
async fn get_sku(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = scope(&enforcer, &ctx, actions::READ).await?;
    super::governance::touch(&state, &scope, ctx.subject_tenant_id(), id).await?;
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    let s = find(&conn, &scope, ctx.subject_tenant_id(), id)
        .await
        .map_err(tx_to_canonical)?;
    let reference_scope = AccessScope::for_tenant(ctx.subject_tenant_id());
    let refs = repo::reference_summary(&conn, &reference_scope, ctx.subject_tenant_id(), id)
        .await
        .map_err(|e| repo_error_to_canonical(&e))?;
    // P-D-197: pricing's usage, or null; the card never fails for it.
    let usage = super::usage::of(&state, &ctx, &[s.id]).await.remove(&s.id);
    let tag = etag(InternalRevision::new(s.revision));
    let mut card = SkuCard {
        sku: s.into(),
        references: refs.into(),
        usage,
    };
    // P-D-262: the creator's name, in one lookup.
    state.actor_names.fill(&ctx, &mut card).await;
    Ok(([(header::ETAG, tag)], Json(card)).into_response())
}
/// Convert malformed query values into canonical 400 violations.
fn query<T>(q: Result<Query<T>, QueryRejection>) -> Result<T, CanonicalError> {
    q.map(|Query(q)| q).map_err(|e| {
        let mut r = ValidationReport::new();
        r.violate("VALIDATION", "query", e.body_text());
        DomainError::Validation(r).into()
    })
}
/// The date `GET /skus/{id}/versions/as-of` reads: `date`, given once, `YYYY-MM-DD`; no other key
/// (P-D-214). A missing, repeated or malformed `date` and any other key are 400, every offender
/// named.
fn as_of_date(query: RawQuery) -> Result<Date, CanonicalError> {
    const KEY: &str = "date";
    let Query(pairs) = query.map_err(|e| {
        OdataError::invalid_argument()
            .with_field_violation("query", e.body_text(), "INVALID_QUERY_PARAMS")
            .create()
    })?;
    let mut offenders: Vec<(&str, String, &'static str)> = pairs
        .iter()
        .filter(|(k, _)| k != KEY)
        .map(|(k, _)| {
            (
                k.as_str(),
                format!("`{k}` is not a parameter of this read; it takes `{KEY}`"),
                UNSUPPORTED,
            )
        })
        .collect();
    let dates: Vec<&str> = pairs
        .iter()
        .filter(|(k, _)| k == KEY)
        .map(|(_, v)| v.as_str())
        .collect();
    let date = match dates.as_slice() {
        [one] => {
            let format = time::format_description::parse_borrowed::<1>("[year]-[month]-[day]")
                .map_err(|e| CanonicalError::internal(e.to_string()).create())?;
            Date::parse(one, &format).ok()
        }
        _ => None,
    };
    if date.is_none() {
        offenders.push((
            KEY,
            match dates.as_slice() {
                [] => format!("`{KEY}` (YYYY-MM-DD) is required"),
                [one] => format!("`{KEY}` is a date YYYY-MM-DD, not `{one}`"),
                _ => format!("`{KEY}` is given more than once"),
            },
            "INVALID_QUERY_PARAMS",
        ));
    }
    refused(&offenders)?;
    date.ok_or_else(|| CanonicalError::internal("a checked date is present").create())
}
/// The version history takes no query key: `as_of` moved to its own path (P-D-214).
fn no_query(query: RawQuery) -> Result<(), CanonicalError> {
    let Query(pairs) = query.map_err(|e| {
        OdataError::invalid_argument()
            .with_field_violation("query", e.body_text(), "INVALID_QUERY_PARAMS")
            .create()
    })?;
    let offenders: Vec<(&str, String, &'static str)> = pairs
        .iter()
        .map(|(k, _)| {
            (
                k.as_str(),
                format!(
                    "`{k}` is not a parameter of the version history; the version in force on a \
                     date is GET /skus/{{id}}/versions/as-of?date=YYYY-MM-DD"
                ),
                UNSUPPORTED,
            )
        })
        .collect();
    refused(&offenders)
}
/// @cpt-cf-bss-products-fr-sku-versions
async fn sku_versions(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    query: RawQuery,
) -> Result<Json<Vec<SkuVersionDto>>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = scope(&enforcer, &ctx, actions::READ).await?;
    no_query(query)?;
    super::governance::touch(&state, &scope, ctx.subject_tenant_id(), id).await?;
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    find(&conn, &scope, ctx.subject_tenant_id(), id)
        .await
        .map_err(tx_to_canonical)?;
    let versions = repo::versions(&conn, &scope, ctx.subject_tenant_id(), id)
        .await
        .map_err(|e| repo_error_to_canonical(&e))?;
    Ok(Json(versions.into_iter().map(Into::into).collect()))
}
/// @cpt-cf-bss-products-fr-sku-versions
async fn sku_version_as_of(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    query: RawQuery,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = scope(&enforcer, &ctx, actions::READ).await?;
    let as_of = as_of_date(query)?;
    super::governance::touch(&state, &scope, ctx.subject_tenant_id(), id).await?;
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    find(&conn, &scope, ctx.subject_tenant_id(), id)
        .await
        .map_err(tx_to_canonical)?;
    let version = repo::version_as_of(&conn, &scope, ctx.subject_tenant_id(), id, as_of)
        .await
        .map_err(|e| repo_error_to_canonical(&e))?;
    let Some(version) = version else {
        // The toolkit's NotFound context is empty, so attach this door's specified
        // discriminator to its canonical Problem without changing the 404 family.
        let error = SkuResource::not_found(format!("no SKU version is in force on {as_of}"))
            .with_resource(id.to_string())
            .create();
        let mut problem = toolkit_canonical_errors::Problem::from_error(&error)
            .map_err(|e| CanonicalError::internal(e.to_string()).create())?;
        problem.context["reason"] = serde_json::json!("NO_VERSION_IN_FORCE");
        return Ok(problem.into_response());
    };
    Ok(Json(SkuVersionDto::from(version)).into_response())
}
/// @cpt-cf-bss-products-fr-reference-registry
async fn sku_references(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    q: Result<Query<ReferenceQuery>, QueryRejection>,
) -> Result<Json<ReferenceList>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = scope(&enforcer, &ctx, actions::READ).await?;
    let q = query(q)?;
    let tenant = ctx.subject_tenant_id();
    super::governance::touch(&state, &scope, ctx.subject_tenant_id(), id).await?;
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    find(&conn, &scope, tenant, id)
        .await
        .map_err(tx_to_canonical)?;
    let scope = AccessScope::for_tenant(tenant);
    let summary = repo::reference_summary(&conn, &scope, tenant, id)
        .await
        .map_err(|e| repo_error_to_canonical(&e))?;
    let items = repo::list_references(&conn, &scope, tenant, id, q.include_released)
        .await
        .map_err(|e| repo_error_to_canonical(&e))?;
    Ok(Json(ReferenceList {
        summary: summary.into(),
        items: items
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<_, RepoError>>()
            .map_err(|e| repo_error_to_canonical(&e))?,
    }))
}
/// Commit audit attribution atomically with the draft mutation, with the lifecycle move it made
/// (P-D-213).
#[expect(
    clippy::too_many_arguments,
    reason = "The audit row's actor, subject and move stay explicit at each draft door"
)]
async fn audit(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    actor_ref: Uuid,
    action: &str,
    s: &Sku,
    written_at: OffsetDateTime,
    lifecycle: repo::LifecycleMove,
) -> Result<(), TxError> {
    repo::write_eventless_act_audit(
        tx,
        scope,
        repo::AuditCommon {
            audit_id: Uuid::now_v7(),
            tenant_id,
            actor_ref,
            action: action.to_owned(),
            subject_kind: "sku".to_owned(),
            reason: None,
            correlation_id: None,
            written_at,
            lifecycle,
        },
        s.id,
        Some(s.revision),
    )
    .await
    .map_err(TxError::Repo)
}
#[cfg(test)]
#[path = "archive_tests.rs"]
mod archive_tests;
#[cfg(test)]
#[path = "skus_tests.rs"]
mod skus_tests;
