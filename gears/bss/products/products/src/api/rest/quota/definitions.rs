//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-reads:p1
//! The catalog reads (P-D-272): `GET /quota-definitions`, the definition versions applicable to
//! one SKU version, and `GET /quota-definitions/{id}/versions/{version}`, one exact version, both
//! under the whole `read` grant on `quota_definition` (its row constraints included, P-D-273) and
//! both answered from the stored content with the current
//! availability and the text in the asked locale or English. `ProductsQuotaCatalogClientV1` runs
//! the same code. No read asks types-registry or a plugin, or computes a digest again.
//!
//! The list's `$filter` takes only `eq` terms joined by `and` on `sku_id` and `sku_version` (both
//! required), `subject_type_ref`, `resource_type_ref` and `profile_id`; its cursor binds the
//! caller's tenant, those terms and `availability`, so a cursor reused by another tenant or under
//! other filters is refused.
use std::collections::BTreeMap;
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::{Extension, Json, Router};
use bss_products_sdk::quota::catalog::{
    QuotaAvailabilityFilter, QuotaDefinitionListRequest, QuotaDefinitionPage,
};
use gts::GtsId;
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::odata::OData;
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};
use toolkit_db::secure::AccessScope;
use toolkit_odata::ast::{CompareOperator, Expr, Value as Literal};
use toolkit_odata::filter::{FieldKind, FilterField, FilterOp};
use toolkit_odata::{CursorV1, Error as ODataError, ODataQuery, Page};
use toolkit_security::SecurityContext;

use super::dto::ProductsQuotaDefinition;
use super::targets::TAG;
use super::{On, QuotaState, definition_read_scope};
use crate::api::rest::require_authenticated;
use crate::api::rest::sku_list::{RawQuery, cursor_hash, params};
use crate::domain::error::DomainError;
use crate::domain::quota::DefinitionFilter;
use crate::domain::validation::ValidationReport;
use crate::infra::storage::RepoError;
use crate::infra::storage::repo::quota_catalog_repo as catalog;

use bss_products_sdk::quota::catalog::{
    QuotaAllowedUnit, QuotaAvailability, QuotaDefinition, QuotaOwnerRef, QuotaVersion,
};
use bss_products_sdk::quota::gts::{
    QuotaAccountingPolicyV1, QuotaChangeSupport, QuotaMetricV1, QuotaScopePolicyV1,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use crate::domain::quota::locale::resolve;
use crate::domain::quota::review::parse_basis;
use crate::domain::quota::{Availability, DefinitionVersion};

/// The definitions' path.
pub(crate) const DEFINITIONS: &str = "/bss-products/v1/quota-definitions";
/// A `$filter` outside the list's terms, or one without `sku_id` and `sku_version`.
pub const QUOTA_FILTER_INVALID: &str = "QUOTA_FILTER_INVALID";

/// The list's `$filter` terms (P-D-272), each served by `eq` only and named at most once. The
/// contract publishes them, and the list reads its terms by them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaDefinitionFilterField {
    SkuId,
    SkuVersion,
    SubjectTypeRef,
    ResourceTypeRef,
    ProfileId,
}
impl FilterField for QuotaDefinitionFilterField {
    const FIELDS: &'static [Self] = &[
        Self::SkuId,
        Self::SkuVersion,
        Self::SubjectTypeRef,
        Self::ResourceTypeRef,
        Self::ProfileId,
    ];
    fn name(&self) -> &'static str {
        match self {
            Self::SkuId => "sku_id",
            Self::SkuVersion => "sku_version",
            Self::SubjectTypeRef => "subject_type_ref",
            Self::ResourceTypeRef => "resource_type_ref",
            Self::ProfileId => "profile_id",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::SkuId | Self::ProfileId => FieldKind::Uuid,
            Self::SkuVersion | Self::SubjectTypeRef | Self::ResourceTypeRef => FieldKind::String,
        }
    }
    fn published_ops(&self) -> Option<&'static [FilterOp]> {
        Some(&[FilterOp::Eq])
    }
    /// The term's exact name: the list reads no other spelling.
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}

/// The terms' names, for a refusal.
fn term_names() -> String {
    QuotaDefinitionFilterField::FIELDS
        .iter()
        .map(FilterField::name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A pinned registry entity as a definition stores it.
#[derive(Deserialize)]
struct StoredEntity {
    #[serde(rename = "ref")]
    id: String,
    digest: String,
    content: Value,
}

/// A mapping or profile reference as a definition stores it.
#[derive(Deserialize)]
struct StoredOwner {
    id: Uuid,
    version: QuotaVersion,
    digest: String,
}

impl From<StoredOwner> for QuotaOwnerRef {
    fn from(owner: StoredOwner) -> Self {
        Self {
            id: owner.id,
            version: owner.version,
            digest: owner.digest,
        }
    }
}

/// The members of a definition's `content_json` a read answers.
#[derive(Deserialize)]
struct StoredContent {
    metric: StoredEntity,
    canonical_unit: StoredEntity,
    allowed_units: Vec<QuotaAllowedUnit>,
    subject_type_ref: String,
    resource_type_refs: Vec<String>,
    accounting_policy: StoredEntity,
    scope_policy: StoredEntity,
    change_support: QuotaChangeSupport,
    supported_bases: Vec<String>,
    profile_conditions: BTreeMap<String, String>,
    mapping_ref: StoredOwner,
    profile_ref: StoredOwner,
}

fn content_of<T: DeserializeOwned>(entity: &StoredEntity) -> Result<T, String> {
    serde_json::from_value(entity.content.clone())
        .map_err(|e| format!("content_json: {}: {e}", entity.id))
}

/// The SDK definition of a stored version and its current availability, text in `locale` or
/// English (P-D-272). The tag `locale` answers is the display name's.
///
/// # Errors
/// A message naming the stored member that does not decode.
pub(crate) fn project(
    row: &DefinitionVersion,
    availability: &Availability,
    locale: Option<&str>,
) -> Result<QuotaDefinition, String> {
    let content: StoredContent = serde_json::from_value(row.content_json.clone())
        .map_err(|e| format!("content_json: {e}"))?;
    let metric: QuotaMetricV1 = content_of(&content.metric)?;
    let accounting: QuotaAccountingPolicyV1 = content_of(&content.accounting_policy)?;
    let scope: QuotaScopePolicyV1 = content_of(&content.scope_policy)?;
    let (display_name, served) = resolve(&metric.display_name, locale);
    let (description, _) = resolve(&metric.description, locale);
    let (conditions, _) = resolve(&content.profile_conditions, locale);
    let supported_bases = content
        .supported_bases
        .iter()
        .map(|token| {
            parse_basis(token).ok_or_else(|| format!("content_json: supported_bases {token:?}"))
        })
        .collect::<Result<_, _>>()?;
    Ok(QuotaDefinition {
        definition_id: row.definition_id,
        version: row.version,
        definition_digest: row.digest.clone(),
        sku_id: row.sku_id,
        sku_version: row.sku_version,
        metric_ref: content.metric.id.clone(),
        metric_digest: content.metric.digest.clone(),
        quota_kind: metric.quota_kind,
        display_name: display_name.to_owned(),
        description: description.to_owned(),
        locale: served.to_owned(),
        canonical_unit_ref: content.canonical_unit.id,
        allowed_units: content.allowed_units,
        subject_type_ref: content.subject_type_ref,
        resource_type_refs: content.resource_type_refs,
        mapping_ref: content.mapping_ref.into(),
        profile_ref: content.profile_ref.into(),
        qualification_ref: availability.qualification.clone(),
        accounting_policy_ref: content.accounting_policy.id.clone(),
        accounting_policy_digest: content.accounting_policy.digest.clone(),
        accounting_policy: accounting,
        scope_policy_ref: content.scope_policy.id.clone(),
        scope_policy_digest: content.scope_policy.digest.clone(),
        scope_policy: scope,
        change_support: content.change_support,
        profile_conditions: conditions.to_owned(),
        supported_bases,
        availability: QuotaAvailability {
            state: availability.state,
            revision: availability.revision,
            observed_at: availability.observed_at,
            reason_code: availability.reason_code.clone(),
        },
    })
}

/// Register the read operations.
pub(super) fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::get(DEFINITIONS)
        .operation_id("bss_products.list_quota_definitions")
        .summary("List the quota definitions of a SKU version")
        .description(
            "One page of the caller's tenant's definition versions applicable to one SKU version: \
             those of the SKU approved at that SKU version or an earlier one (P-D-270, P-D-272), \
             by `definition_id`, then `version`. `$filter` is required: a conjunction (`and`) of \
             `eq` terms on `sku_id` and `sku_version` (both required), and optionally \
             `subject_type_ref`, `resource_type_ref` (GTS ids) and `profile_id`, each at most \
             once, as `sku_id eq <uuid> and sku_version eq '<n>'`; anything else, or a missing \
             required term, is 400 QUOTA_FILTER_INVALID naming the term. A `$filter` that does \
             not parse, or is over 8 KiB or 2000 nodes, is the shared OData refusal on \
             `$filter` (INVALID_FILTER, FILTER_TOO_LONG, FILTER_TOO_COMPLEX), answered before \
             any term is read. A grant on some \
             definitions lists only those. `availability` is `enabled` (the default) or \
             `all`, which adds the other versions with their reason; `locale` picks the text, \
             English when the locale is not held. `$top` (alias `limit`; default 50, at most \
             200) and `cursor` (alias `$skiptoken`) from `page_info`; the cursor binds the \
             caller's tenant, the filter and `availability`: reused by another tenant or under \
             others it is 400. Asks `read` on `quota_definition`; no read asks types-registry.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .with_odata_filter::<QuotaDefinitionFilterField>()
        .query_param_typed(
            "availability",
            false,
            "`enabled` (default) or `all`",
            "string",
        )
        .query_param_typed(
            "locale",
            false,
            "The locale tag to serve texts in; English when not held",
            "string",
        )
        .query_param_typed(
            "limit",
            false,
            "Page size, alias of $top (default 50, at most 200)",
            "integer",
        )
        .query_param_typed(
            "cursor",
            false,
            "Continuation from page_info (alias $skiptoken)",
            "string",
        )
        .handler(list_definitions)
        .json_response_with_schema::<Page<ProductsQuotaDefinition>>(
            openapi,
            StatusCode::OK,
            "One page of definition versions.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::get(format!("{DEFINITIONS}/{{id}}/versions/{{version}}"))
        .operation_id("bss_products.get_quota_definition")
        .summary("Read a quota definition version")
        .description(
            "One exact definition version of the caller's tenant with its current availability, \
             as stored (P-D-272); `locale` picks the text, English when the locale is not held. \
             Asks `read` on `quota_definition`. Refusals: 400 VALIDATION for a `version` that is \
             not a canonical decimal from 1; 404 for an unknown or foreign version, or one the \
             caller's grant does not name.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "The definition's id")
        .path_param("version", "The version, a canonical decimal from 1")
        .query_param_typed(
            "locale",
            false,
            "The locale tag to serve texts in; English when not held",
            "string",
        )
        .handler(get_definition)
        .json_response_with_schema::<ProductsQuotaDefinition>(
            openapi,
            StatusCode::OK,
            "The definition version.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// 400 `code` on `field`, on the quota definition.
fn refused(code: &'static str, field: &str, detail: impl Into<String>) -> CanonicalError {
    let mut report = ValidationReport::new();
    report.violate(code, field, detail);
    On::DEFINITION.refuse(DomainError::Validation(report))
}

fn filter_invalid(detail: impl Into<String>) -> CanonicalError {
    refused(QUOTA_FILTER_INVALID, "$filter", detail)
}

/// A term as the caller wrote it, for a refusal.
fn term_text(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(name) => name.clone(),
        Expr::Value(value) => format!("a {value}"),
        Expr::Compare(left, op, right) => {
            format!("{} {op:?} {}", term_text(left), term_text(right))
        }
        Expr::And(..) => "an `and`".to_owned(),
        Expr::Or(..) => "an `or`".to_owned(),
        Expr::Not(..) => "a `not`".to_owned(),
        Expr::In(..) => "an `in`".to_owned(),
        Expr::Function(name, _) => format!("the function {name}"),
    }
}

/// The `eq` terms of a conjunction, by field; anything else names the term it refuses.
fn terms_of(expr: &Expr, terms: &mut BTreeMap<String, Literal>) -> Result<(), CanonicalError> {
    match expr {
        Expr::And(left, right) => {
            terms_of(left, terms)?;
            terms_of(right, terms)
        }
        Expr::Compare(left, CompareOperator::Eq, right) => match (left.as_ref(), right.as_ref()) {
            (Expr::Identifier(field), Expr::Value(value))
                if QuotaDefinitionFilterField::from_name(field).is_some() =>
            {
                if terms.insert(field.clone(), value.clone()).is_some() {
                    return Err(filter_invalid(format!("`{field}` is named twice")));
                }
                Ok(())
            }
            _ => Err(filter_invalid(format!(
                "`{}` is not a term of this list: it takes `eq` on {}",
                term_text(expr),
                term_names()
            ))),
        },
        other => Err(filter_invalid(format!(
            "`{}` is not a term of this list: it takes `eq` terms joined by `and`",
            term_text(other)
        ))),
    }
}

/// A UUID term: a UUID literal or its text.
fn uuid_term(field: &str, value: &Literal) -> Result<uuid::Uuid, CanonicalError> {
    match value {
        Literal::Uuid(id) => Ok(*id),
        Literal::String(text) => {
            uuid::Uuid::parse_str(text).map_err(|_| filter_invalid(format!("`{field}` is a UUID")))
        }
        _ => Err(filter_invalid(format!("`{field}` is a UUID"))),
    }
}

/// A text term.
fn text_term(field: &str, value: &Literal) -> Result<String, CanonicalError> {
    match value {
        Literal::String(text) => Ok(text.clone()),
        _ => Err(filter_invalid(format!("`{field}` is a quoted string"))),
    }
}

/// The list's request from its `$filter`, as the `OData` extractor parsed it within its budget,
/// `availability` and `locale`.
fn request_of(
    filter: Option<&Expr>,
    availability: Option<&str>,
    locale: Option<String>,
) -> Result<QuotaDefinitionListRequest, CanonicalError> {
    let mut terms = BTreeMap::new();
    if let Some(expr) = filter {
        terms_of(expr, &mut terms)?;
    }
    let required = |field: &str| {
        terms
            .get(field)
            .ok_or_else(|| filter_invalid(format!("`{field}` is required")))
    };
    let sku_id = uuid_term("sku_id", required("sku_id")?)?;
    let version_text = text_term("sku_version", required("sku_version")?)?;
    let sku_version = version_text.parse().map_err(|_| {
        filter_invalid(format!(
            "`sku_version` is a canonical decimal from 1, not {version_text:?}"
        ))
    })?;
    let optional_text = |field: &str| {
        terms
            .get(field)
            .map(|value| text_term(field, value))
            .transpose()
    };
    let availability = match availability {
        None | Some("enabled") => QuotaAvailabilityFilter::Enabled,
        Some("all") => QuotaAvailabilityFilter::All,
        Some(other) => {
            return Err(refused(
                "VALIDATION",
                "availability",
                format!("`availability` is `enabled` or `all`, not {other:?}"),
            ));
        }
    };
    Ok(QuotaDefinitionListRequest {
        sku_id,
        sku_version,
        subject_type_ref: optional_text("subject_type_ref")?,
        resource_type_ref: optional_text("resource_type_ref")?,
        profile_id: terms
            .get("profile_id")
            .map(|value| uuid_term("profile_id", value))
            .transpose()?,
        locale,
        availability,
        limit: None,
        cursor: None,
    })
}

/// The deterministic UUID of a GTS type the filter names (package §3.3.1).
fn type_uuid(field: &str, id: Option<&str>) -> Result<Option<uuid::Uuid>, CanonicalError> {
    id.map(|id| {
        GtsId::try_new(id)
            .map(|g| g.to_uuid())
            .map_err(|e| filter_invalid(format!("`{field}` is a GTS id: {e}")))
    })
    .transpose()
}

/// A stored definition that does not project is a corrupt row (P-D-274).
fn corrupt(row: &DefinitionVersion, tenant: uuid::Uuid, why: &str) -> CanonicalError {
    On::DEFINITION.stored(&RepoError::CorruptRow(format!(
        "a stored quota row does not read: products_quota_definition_version, tenant {tenant}, \
         id {} version {}: {why}",
        row.definition_id, row.version
    )))
}

// @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-browse:p1:inst-quota-list
/// One page for a caller already authorized under `scope` (P-D-272): the cursor bound to the
/// tenant, the filter and the availability, the page and its availability in two statements,
/// each row projected from its stored content. `odata` carries the page size and the cursor.
async fn page_in(
    state: &QuotaState,
    scope: &AccessScope,
    tenant: uuid::Uuid,
    request: &QuotaDefinitionListRequest,
    mut odata: ODataQuery,
) -> Result<Page<QuotaDefinition>, CanonicalError> {
    let filter = DefinitionFilter {
        sku_id: request.sku_id,
        sku_version: request.sku_version,
        subject_type_uuid: type_uuid("subject_type_ref", request.subject_type_ref.as_deref())?,
        resource_type_uuid: type_uuid("resource_type_ref", request.resource_type_ref.as_deref())?,
        profile_id: request.profile_id,
        enabled_only: request.availability == QuotaAvailabilityFilter::Enabled,
    };
    // The cursor binds the caller's tenant, every term and the availability: reused by another
    // tenant or under another filter it is refused, not misread.
    let hash = cursor_hash(&serde_json::json!({
        "quota_definitions": {
            "tenant": tenant.to_string(),
            "sku_id": request.sku_id.to_string(),
            "sku_version": request.sku_version.to_string(),
            "subject_type_ref": request.subject_type_ref,
            "resource_type_ref": request.resource_type_ref,
            "profile_id": request.profile_id.map(|id| id.to_string()),
            "availability": match request.availability {
                QuotaAvailabilityFilter::Enabled => "enabled",
                QuotaAvailabilityFilter::All => "all",
            },
        }
    }));
    if let Some(cursor) = &odata.cursor
        && cursor.f.as_deref() != Some(hash.as_str())
    {
        return Err(ODataError::FilterMismatch.into());
    }
    // The list's terms are not the pager's fields: the repository applies them.
    odata.filter = None;
    odata.filter_hash = Some(hash);
    let conn = On::DEFINITION.conn(&state.api)?;
    let page = catalog::list_definitions(&conn, scope, tenant, &filter, &odata)
        .await
        .map_err(|e| On::DEFINITION.list(e))?;
    let locale = request.locale.as_deref();
    let items = page
        .items
        .iter()
        .map(|(row, availability)| {
            project(row, availability, locale).map_err(|why| corrupt(row, tenant, &why))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Page {
        items,
        page_info: page.page_info,
    })
}
// @cpt-end:cpt-cf-bss-products-flow-quota-catalog-browse:p1:inst-quota-list

/// One page for a caller (P-D-272): `read` on `quota_definition` first; REST and the SDK client
/// both call it.
///
/// # Errors
/// 400 `QUOTA_FILTER_INVALID` / `OData` cursor refusals, 403 / 503 from the PDP, 500 for a corrupt
/// row.
pub(crate) async fn list_page(
    state: &QuotaState,
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    request: &QuotaDefinitionListRequest,
) -> Result<QuotaDefinitionPage, CanonicalError> {
    let scope = definition_read_scope(enforcer, ctx).await?;
    let mut odata = ODataQuery::default();
    if let Some(limit) = request.limit {
        if limit == 0 {
            return Err(ODataError::InvalidLimit.into());
        }
        odata = odata.with_limit(u64::from(limit));
    }
    if let Some(cursor) = &request.cursor {
        odata = odata.with_cursor(CursorV1::decode(cursor).map_err(|_| ODataError::InvalidCursor)?);
    }
    let page = page_in(state, &scope, ctx.subject_tenant_id(), request, odata).await?;
    Ok(QuotaDefinitionPage {
        items: page.items,
        next_cursor: page.page_info.next_cursor,
    })
}

// @cpt-flow:cpt-cf-bss-products-flow-quota-catalog-browse:p1
async fn list_definitions(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    query: RawQuery,
    odata: Result<OData, CanonicalError>,
) -> Result<Json<Page<ProductsQuotaDefinition>>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = definition_read_scope(&enforcer, &ctx).await?;
    let pairs: Vec<(String, String)> = query
        .as_ref()
        .map(|axum::extract::Query(pairs)| pairs.clone())
        .unwrap_or_default();
    params(
        query,
        &["availability", "locale", "limit", "cursor"],
        Some(&["$filter", "$top", "$skiptoken"]),
    )?;
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    // The extractor answers first, `FILTER_TOO_LONG` and `FILTER_TOO_COMPLEX` included: its one
    // parse, within its length and node budget, is the one the terms are read from.
    let OData(odata) = odata?;
    let request = request_of(
        odata.filter.as_deref(),
        value("availability").as_deref(),
        value("locale"),
    )?;
    let page = page_in(&state, &scope, ctx.subject_tenant_id(), &request, odata).await?;
    Ok(Json(Page {
        items: page.items.into_iter().map(Into::into).collect(),
        page_info: page.page_info,
    }))
}

// @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-browse:p1:inst-quota-get
/// One exact version for a caller already authorized under `scope`, with its current
/// availability, text in `locale` or English (P-D-272).
async fn read_in(
    state: &QuotaState,
    scope: &AccessScope,
    tenant: uuid::Uuid,
    (definition_id, version): (uuid::Uuid, QuotaVersion),
    locale: Option<&str>,
) -> Result<QuotaDefinition, CanonicalError> {
    let conn = On::DEFINITION.conn(&state.api)?;
    let (row, availability) =
        catalog::find_definition(&conn, scope, tenant, definition_id, version)
            .await
            .map_err(|e| On::DEFINITION.stored(&e))?
            .ok_or_else(|| not_found(definition_id))?;
    project(&row, &availability, locale).map_err(|why| corrupt(&row, tenant, &why))
}
// @cpt-end:cpt-cf-bss-products-flow-quota-catalog-browse:p1:inst-quota-get

/// One exact version with its current availability, text in `locale` or English (P-D-272):
/// `read` on `quota_definition` first; REST and the SDK client both read through it.
///
/// # Errors
/// 404 for an unknown version or another tenant's; 403 / 503 from the PDP; 500 for a corrupt
/// row.
pub(crate) async fn read_one(
    state: &QuotaState,
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    definition_id: uuid::Uuid,
    version: QuotaVersion,
    locale: Option<&str>,
) -> Result<QuotaDefinition, CanonicalError> {
    let scope = definition_read_scope(enforcer, ctx).await?;
    read_in(
        state,
        &scope,
        ctx.subject_tenant_id(),
        (definition_id, version),
        locale,
    )
    .await
}

fn not_found(id: uuid::Uuid) -> CanonicalError {
    On::DEFINITION.refuse(DomainError::NotFound {
        what: "quota_definition",
        id,
    })
}

async fn get_definition(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path((id, version)): Path<(String, String)>,
    query: RawQuery,
) -> Result<Json<ProductsQuotaDefinition>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // The grant is asked before the path and the query are read.
    let scope = definition_read_scope(&enforcer, &ctx).await?;
    let pairs: Vec<(String, String)> = query
        .as_ref()
        .map(|axum::extract::Query(pairs)| pairs.clone())
        .unwrap_or_default();
    params(query, &["locale"], Some(&[]))?;
    let Ok(id) = uuid::Uuid::parse_str(&id) else {
        return Err(not_found(uuid::Uuid::nil()));
    };
    let version: QuotaVersion = version.parse().map_err(|_| {
        refused(
            "VALIDATION",
            "version",
            format!("a version is a canonical decimal from 1, not {version:?}"),
        )
    })?;
    let locale = pairs
        .iter()
        .find(|(k, _)| k == "locale")
        .map(|(_, v)| v.as_str());
    let tenant = ctx.subject_tenant_id();
    let definition = read_in(&state, &scope, tenant, (id, version), locale).await?;
    Ok(Json(definition.into()))
}
