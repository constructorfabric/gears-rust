//! `GET/PUT /bss-pricing/v1/config/taxonomies/{region|brand|partner|org_tier}` —
//! the tenant's four scope-value universes (`design/04-currency-tax.md` §5, §6,
//! `inst-tx-mutation`).
//!
//! # This is the surface the gear has been missing, not a convenience
//!
//! Slice 9's `inst-plv-scope` validates every non-`global` overlay scope value
//! against one of these four tables, and Slice 4's `inst-tx-region` validates
//! every price row's region against one of them. **Both rules shipped before any
//! of the four had a writer.** The consequence was not a cosmetic gap: an
//! operator could not author a brand-scoped `PriceOverlay` end to end, because
//! the value the overlay must name had nowhere to come from, and the only way to
//! declare one was direct SQL. That is the D-211 shape — a rule whose universe is
//! unreachable — arriving from the authoring side instead of the validating one.
//!
//! # One route pair for four classes, and why the class is a path segment
//!
//! §5 writes the row as a single cell, `{region|brand|partner|org_tier}`, and the
//! four are the same resource shape over four universes: same columns, same state
//! machine, same guard, same gate. A route each would be four copies of one
//! handler differing in a `match` arm, and the day a fifth class is declared
//! (`customerGroup` is already named in Slice 9 §3, waiting on the membership
//! half) it would be a fifth copy rather than a fifth enum member.
//!
//! The class is a **path segment** and not a query parameter because it selects
//! the resource rather than filtering it: `/config/taxonomies/brand` is a
//! different document from `/config/taxonomies/partner`, with its own `ETag` and
//! its own concurrent editors. A query parameter would make them one resource
//! with four representations, and one tag would then have to cover all four.
//!
//! **The segment is the token the overlay plane stores** — `TaxonomyClass::
//! path_segment` returns `ScopeClass::as_str`, so the two cannot diverge rather
//! than merely happening not to. §5 spelled the last class `orgTier` and this
//! route answered to that, on the argument that §5 is the normative statement of
//! the route and a path segment is not a JSON field. **D-241 closed it the other
//! way**: an operator meets both spellings in one sitting, and a client generated
//! from the `OpenAPI` document carries two names for one class. `orgTier` is
//! refused rather than aliased, because two spellings that both route is the
//! state in which neither is canonical.
//!
//! # The `PUT` carries the whole taxonomy
//!
//! `taxonomy_repo`'s module doc carries the argument in full. In short: the
//! resource is the tenant's value set, so the body is that set and not a patch,
//! and a value the body omits is **retired** — never deleted, because a value a
//! published row names has to keep existing. A retirement the guard refuses fails
//! the whole `PUT` with `TAXONOMY_VALUE_IN_USE` (409) and writes nothing.
//!
//! # `If-Match` is required, and the bootstrap is not an exception
//!
//! §5's Idempotency cell for this row is `ETag`, and D-171 makes the header
//! required wherever a table names a precondition. The reasoning
//! `api::rest::threshold_policy` records applies here unchanged, including the
//! premise it had to withdraw: a tenant with no taxonomy at all is answered
//! **200** with an empty list, so the resource always has a representation and
//! therefore always has a tag. A first `PUT` reads it off the `GET` like any
//! other.
//!
//! It matters more here than the row count suggests, because the `PUT` is a
//! whole-set replacement. Without a precondition, two admins who each read the
//! brand list and each add one value would produce a taxonomy holding **one** of
//! the two additions and silently retiring the other's — a lost update that looks
//! exactly like a value somebody meant to withdraw.
//!
//! # The gate is `config`, and it is one gate for both verbs' subjects
//!
//! `cf.bss.pricing.config.v1~` already exists and its own doc names this surface:
//! *"the tenant config plane (`write`, `read`): tax-display policy and the
//! taxonomies"*. **No authz vocabulary is minted here.** Deliberately not
//! `approval_policy`, which is segregated so a config admin cannot weaken the
//! thresholds governing their own changes — a taxonomy is the config plane's own
//! subject, and §10 assigns it to `CatalogAdmin`.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::HeaderMap;
use axum::http::header::ETAG;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, http::StatusCode};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::operation_builder::{ParamLocation, ParamSpec};
use toolkit::api::{OpenApiRegistry, operation_builder::OperationBuilder};
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;

use crate::api::rest::auth_context::{audit_stamp, require_authenticated};
use crate::api::rest::correlation::{CorrelationId, require_correlation};
use crate::api::rest::error::authz_error_to_canonical;
use crate::api::rest::preconditions;
use crate::api::rest::state::AuthoringState;
use crate::domain::error::DomainError;
use crate::domain::overlay::ScopeValue;
use crate::domain::taxonomy::{
    RegionTaxMarkers, TAXONOMY_VALUE_IN_USE, TaxonomyClass, TaxonomyEntry, TaxonomyState, tag_of,
};
use crate::infra::storage::repo_failure;

/// `OpenAPI` tag applied to both operations (DE0205).
const TAG: &str = "BSS Pricing Configuration";

/// The taxonomy resource.
///
/// The literal is repeated in both `OperationBuilder` calls below because DE0801
/// validates a **literal** argument and silently passes a `const` one; the two
/// spellings are pinned together by `tests/module_test.rs`'s route census.
pub const TAXONOMY: &str = "/bss-pricing/v1/config/taxonomies/{class}";

/// One declared value, as an operator reads and writes it.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct TaxonomyValueView {
    /// The declared code — the string a price row's `region` or an overlay's
    /// `scopeValue` must match. Never blank.
    pub value: String,
    /// The operator's label for it.
    pub display_name: String,
    /// `active` or `retired`. Optional on the way in and defaulting to `active`,
    /// because a body listing a value is a body declaring it; sending
    /// `"retired"` is the explicit spelling of the same act as leaving it out,
    /// and both are guarded identically.
    pub state: Option<String>,
    /// The region's default tax category (D-01). **Region taxonomy only** — the
    /// other three carry no such column, and a body setting it on them is
    /// refused rather than ignored.
    pub tax_category: Option<String>,
    /// Tenant-declared *"a tax rate is configured for this region"*, the MVP
    /// `RegionTaxReadiness` source (D-01). **Region taxonomy only.** Absent reads
    /// as `false`, which is C4's fail-closed default: a region nobody has
    /// declared a rate for is a region with no rate.
    pub tax_rate_present: Option<bool>,
}

/// One taxonomy, whole.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct TaxonomyView {
    /// Which universe this is — the path segment, echoed.
    pub class: String,
    /// Every declared value, `active` and `retired` alike, ordered by value.
    ///
    /// Retirements are **included**, which is what makes the round trip honest:
    /// an operator who reads, edits and writes back has to be able to see the
    /// value they are about to re-activate.
    pub values: Vec<TaxonomyValueView>,
}

/// The body of a `PUT`: the complete value set.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct PutTaxonomyRequest {
    /// The whole taxonomy, not a patch. A value declared today and absent here is
    /// **retired**.
    pub values: Vec<TaxonomyValueView>,
}

/// The `If-Match` header this `PUT` requires, declared so a generated client
/// sends it (D-171).
fn if_match_param() -> ParamSpec {
    ParamSpec {
        name: "If-Match".to_owned(),
        location: ParamLocation::Header,
        required: true,
        description: Some(
            "Mandatory precondition (RFC 9110), and section 5's `ETag` cell for this row. The \
             value is the **opaque** tag the `GET` returns in its `ETag` header - copy it back \
             verbatim. It is not a row version: a taxonomy is a value set with no version \
             column, so the tag is a digest over the representation the `GET` serves - the \
             class, and every value's code, state and label. It therefore moves when a value is \
             added, retired, re-activated or re-labelled. A tenant with no values at all is \
             answered `200` with an empty list and carries a tag, so a first `PUT` asserts it \
             like any other. It matters here more than on most rows because this `PUT` replaces \
             the **whole** set: without it, two admins who each add one value would leave a \
             taxonomy carrying one addition and silently retiring the other's. A tag that no \
             longer describes the taxonomy is `409` `STALE_VERSION`; an absent or malformed one \
             is `400`. Weak validators, the wildcard `*` and tag lists are all refused."
                .to_owned(),
        ),
        param_type: "string".to_owned(),
        // Scalar: every parameter this gear declares is single-valued.
        // `array` arrived upstream for `?tag=a&tag=b` repeats, which no route
        // here has.
        array: false,
    }
}

/// The `{class}` path parameter.
fn class_param() -> ParamSpec {
    ParamSpec {
        name: "class".to_owned(),
        location: ParamLocation::Path,
        required: true,
        description: Some(
            "Which value universe: `region`, `brand`, `partner` or `org_tier`. Each segment is \
             the same token the class carries in an overlay's `scopeClass` field, so a \
             generated client has one name per class (D-241); the camelCase `orgTier` section 5 \
             used to spell is refused rather than accepted as an alias. `global` and \
             `customerGroup` are deliberately not addressable: the first has no value universe, \
             and the second's table belongs to the customer-group membership half and does not \
             exist."
                .to_owned(),
        ),
        param_type: "string".to_owned(),
        // Scalar: every parameter this gear declares is single-valued.
        // `array` arrived upstream for `?tag=a&tag=b` repeats, which no route
        // here has.
        array: false,
    }
}

/// Build the Axum router for the two taxonomy operations and register them.
pub fn router(state: Arc<AuthoringState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::get("/bss-pricing/v1/config/taxonomies/{class}")
        .operation_id("bss_pricing.get_taxonomy")
        .summary("Read one of the tenant's four scope-value taxonomies")
        .description(
            "Every value the tenant has declared in this universe, `active` and `retired` \
             alike, ordered by value. Retirements are included deliberately: retirement is \
             guarded rather than cascading and `retired -> active` is a legal audited move, so \
             an operator editing this list has to be able to see the value they are about to \
             re-activate. A tenant that has declared nothing is answered `200` with an empty \
             list - that is a state, not an absent resource, and it is a fail-closed one: \
             `inst-tx-region` validates every price row's region against the **active** values \
             here, so a tenant with none publishes no rows at all. **The response carries the \
             `ETag` the `PUT` demands**, and this is the only place to obtain one. On the \
             `region` universe each value also carries D-01's two markers, `taxCategory` and \
             `taxRatePresent`, which are the MVP source for the tax-display readiness check; \
             the other three universes carry no such columns. Gates on `config` x `read`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .param(class_param())
        .param(crate::api::rest::plans::if_none_match_param())
        .handler(get_taxonomy)
        .json_response_with_schema::<TaxonomyView>(
            openapi,
            StatusCode::OK,
            "The declared values of this universe.",
        )
        // The conditional read's answer (RFC 9110 section 15.4.5). Declared
        // because it is reachable: this route emits an `ETag` and honours the
        // `If-None-Match` a caller sends it back in. A read that emits a validator
        // and ignores the conditional is the half-implementation to avoid.
        .no_content_response(
            StatusCode::NOT_MODIFIED,
            "The caller's `If-None-Match` matches the current representation, so the body is \
             not re-sent.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(Router::new(), openapi);

    let router = OperationBuilder::put("/bss-pricing/v1/config/taxonomies/{class}")
        .operation_id("bss_pricing.put_taxonomy")
        .summary("Replace one of the tenant's four scope-value taxonomies")
        .description(
            "The body is the **whole** taxonomy, not a patch. A value listed here is declared \
             (and re-activated if it was retired); a value the tenant holds today and this body \
             omits is **retired** - never deleted, because a value a published price row or a \
             published overlay scope still names has to keep existing. Sending a value with \
             `state: \"retired\"` is the explicit spelling of the same act, and the two are \
             guarded identically. **Retirement is guarded** (`inst-tx-mutation`): a value still \
             named by a published price row on its region axis, or by a published `PriceOverlay` \
             scoped to this class and value, cannot retire - the response is `409` \
             `TAXONOMY_VALUE_IN_USE` naming both counts, and **nothing at all is written**, so \
             the taxonomy an operator re-authors against is the one they had. One transaction, \
             one verdict: there is no partial application. Draft rows and draft overlays do not \
             block, and neither does history - a superseded row keeps naming its region and \
             counting it would refuse the retirement of every region a tenant ever repriced. \
             The whole call is one audited config mutation, however many values it moved. \
             `taxCategory` and `taxRatePresent` are accepted on the `region` universe only and \
             refused elsewhere. **`If-Match` is required**: send the opaque `ETag` the `GET` \
             returned. Gates on `config` x `write`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .param(class_param())
        .param(if_match_param())
        .json_request::<PutTaxonomyRequest>(openapi, "The complete value set for this universe.")
        .handler(put_taxonomy)
        .json_response_with_schema::<TaxonomyView>(
            openapi,
            StatusCode::OK,
            "The taxonomy as it now stands.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // D-178's edge, at this router's own tail for the reason every other mutating
    // router applies it at its own: a surface reachable without it cannot build
    // an `AuditStamp`.
    router
        .layer(Extension(state))
        .layer(axum::middleware::from_fn(
            crate::api::rest::correlation::establish,
        ))
}

/// `GET /config/taxonomies/{class}`.
///
/// Answers a [`Response`] rather than a [`Json`] because it carries the `ETag`
/// the `PUT` demands, and it is the **only** place a caller can obtain one: this
/// resource has no create whose response could seed a tag.
async fn get_taxonomy(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(class): Path<String>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = read_scope(&enforcer, &ctx).await?;
    // **After the gate, deliberately.** A caller who may not read this resource
    // is told that, rather than being told their path segment is unknown — the
    // ordering `rest_authz`'s `every_route_asks_the_catalogued_pair` depends on.
    let class = parse_class(&class)?;

    let held = state
        .taxonomies
        .list(&scope, ctx.subject_tenant_id(), class)
        .await
        .map_err(|e| CanonicalError::from(repo_failure(&e)))?;

    Ok(render(class, &held, Some(&headers)))
}

/// `PUT /config/taxonomies/{class}`.
async fn put_taxonomy(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    extension_correlation: Option<Extension<CorrelationId>>,
    Path(class): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let correlation = require_correlation(extension_correlation)?;
    let scope = write_scope(&enforcer, &ctx).await?;
    let tenant = ctx.subject_tenant_id();
    let now = chrono::Utc::now();

    let class = parse_class(&class)?;
    let asserted = preconditions::if_match_policy(&headers).map_err(CanonicalError::from)?;
    let request: PutTaxonomyRequest = preconditions::parse_body(&body)?;
    let entries = authored_entries(class, request.values)?;

    // **The premise is handed to the store, not tested here** (D-186, and
    // `infra::threshold::propose`'s arrangement): this module refuses a request
    // it cannot *understand* — an absent or malformed tag, which
    // `if_match_policy` already did above — and the store refuses one whose
    // premise has *moved*, inside the transaction that writes.
    //
    // It was compared here once, against a read on a plain connection, and that
    // was a real hole rather than a tidiness question: the `PUT` replaces the
    // whole set, so two callers whose reads both precede either commit each
    // passed, and the second one's write retired whatever the first had added.
    let result = state
        .taxonomies
        .replace(
            &scope,
            tenant,
            class,
            entries,
            &asserted,
            audit_stamp(&ctx, now, correlation),
        )
        .await
        .map_err(|e| CanonicalError::from(repo_failure(&e)))?;

    if result.stale {
        return Err(CanonicalError::from(DomainError::StaleVersion(format!(
            "the If-Match tag no longer describes the {class} taxonomy: it changed after you \
             read it. Re-read the GET and author against the tag it hands back — a PUT replaces \
             the whole value set, so applying yours over a moved one would retire whatever the \
             other author added"
        ))));
    }

    if let Some(violation) = result.report.violations.first() {
        // The **first** rather than all of them, for `overlay_rules::conflict_of`'s
        // reason: a conflict is a single fact about the world, and an operator told
        // that four values are pinned acts on one of them and re-reads.
        debug_assert_eq!(violation.code, TAXONOMY_VALUE_IN_USE);
        return Err(CanonicalError::from(DomainError::TaxonomyValueInUse(
            violation.detail.clone(),
        )));
    }

    Ok(render(class, &result.entries, None))
}

/// The representation, with the tag that covers it.
///
/// One function for both verbs, so the `GET`'s tag and the `PUT`'s response tag
/// cannot come from two renderings of one taxonomy — which is how a caller ends
/// up holding a tag their next request is refused for.
/// `conditional` is `Some` on the `GET` and `None` on the `PUT`: the tag has one
/// producer and a conditional read must compare against *that* value, so the
/// comparison lives beside the rendering rather than in a second reading of the
/// same resource. See [`preconditions::if_none_match`].
fn render(
    class: TaxonomyClass,
    entries: &[TaxonomyEntry],
    conditional: Option<&HeaderMap>,
) -> Response {
    let tag = preconditions::policy_etag(&tag_of(class, entries));
    if conditional.is_some_and(|headers| preconditions::if_none_match(headers, &tag)) {
        return preconditions::not_modified(&tag);
    }
    (
        [(ETAG, tag)],
        Json(TaxonomyView {
            class: class.path_segment().to_owned(),
            values: entries.iter().map(view_of).collect(),
        }),
    )
        .into_response()
}

fn view_of(entry: &TaxonomyEntry) -> TaxonomyValueView {
    TaxonomyValueView {
        value: entry.value.as_str().to_owned(),
        display_name: entry.display_name.clone(),
        state: Some(entry.state.as_str().to_owned()),
        tax_category: entry.tax.as_ref().and_then(|t| t.tax_category.clone()),
        tax_rate_present: entry.tax.as_ref().map(|t| t.tax_rate_present),
    }
}

/// Resolve the path segment, refusing the two classes that are not addressable.
///
/// The refusal names them rather than answering a bare 404, because `global` and
/// `customerGroup` are real scope classes an operator has met in the overlay
/// surface — being told the segment is unknown would send them looking for a typo
/// in a word they spelled correctly.
fn parse_class(segment: &str) -> Result<TaxonomyClass, CanonicalError> {
    TaxonomyClass::parse_segment(segment).ok_or_else(|| {
        CanonicalError::from(DomainError::InvalidRequest(format!(
            "unknown taxonomy `{segment}`: the addressable universes are region, brand, partner \
             and org_tier — each spelled as the class's own scope token (D-241; the camelCase \
             `orgTier` this route used to answer to is refused, not aliased). `global` has no \
             value universe — the classless scope carries no value — and `customerGroup`'s \
             taxonomy belongs to the customer-group membership plane and does not exist yet"
        )))
    })
}

/// Turn the authored body into domain entries, refusing what the class cannot
/// hold.
///
/// **The `tax_*` fields are refused on the three non-region classes rather than
/// ignored**, and that is the same argument `check_tax_basis_declared` makes one
/// slice over: silently dropping a field an operator set is indistinguishable, at
/// the operator's end, from having set it — they would read the `GET` back, see
/// no category, and conclude the value did not save.
fn authored_entries(
    class: TaxonomyClass,
    values: Vec<TaxonomyValueView>,
) -> Result<Vec<TaxonomyEntry>, CanonicalError> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut entries = Vec::with_capacity(values.len());
    for value in values {
        let declared = ScopeValue::new(&value.value).ok_or_else(|| {
            CanonicalError::from(DomainError::InvalidRequest(
                "a taxonomy value must not be blank or whitespace: the empty string is the \
                 store's sentinel for the classless overlay scope, so a blank value here would \
                 make that sentinel forgeable"
                    .to_owned(),
            ))
        })?;
        if !seen.insert(declared.as_str().to_owned()) {
            // Refused rather than de-duplicated: a body naming one value twice
            // has two states for it and no rule says which wins. The composite
            // key never sees the second one — `taxonomy_repo::apply_replace`
            // collects the submitted set into a `BTreeMap` keyed by value — so
            // the alternative is not a driver error but a silent 200 for a set
            // the author did not send, carrying whichever line came last.
            return Err(CanonicalError::from(DomainError::InvalidRequest(format!(
                "value `{declared}` appears twice in this body; a taxonomy declares each value \
                 once, and a repeated one leaves its state undecided"
            ))));
        }
        let state = match value.state.as_deref() {
            None => TaxonomyState::Active,
            Some(token) => TaxonomyState::parse(token).ok_or_else(|| {
                CanonicalError::from(DomainError::InvalidRequest(format!(
                    "value `{declared}` carries state `{token}`; a taxonomy value is `active` or \
                     `retired`, and nothing else — retirement is guarded and re-activation is a \
                     legal audited move, so a third state would be one no rule describes"
                )))
            })?,
        };
        if !class.carries_tax_markers()
            && (value.tax_category.is_some() || value.tax_rate_present.is_some())
        {
            return Err(CanonicalError::from(DomainError::InvalidRequest(format!(
                "value `{declared}`: taxCategory and taxRatePresent are declared on the region \
                 taxonomy alone (D-01) and the {class} taxonomy has no column for either. They \
                 are refused rather than dropped, because a field that vanishes silently reads \
                 to the operator exactly like one that failed to save"
            ))));
        }
        entries.push(TaxonomyEntry {
            value: declared,
            display_name: value.display_name,
            state,
            tax: class.carries_tax_markers().then(|| RegionTaxMarkers {
                tax_category: value.tax_category,
                tax_rate_present: value.tax_rate_present.unwrap_or(false),
            }),
        });
    }
    Ok(entries)
}

/// The `config × read` gate.
async fn read_scope(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<AccessScope, CanonicalError> {
    crate::authz::access_scope(
        enforcer,
        ctx,
        &crate::authz::resource_types::CONFIG,
        crate::authz::actions::READ,
        /* owner_tenant_id */ None,
        /* resource_id */ None,
    )
    .await
    .map_err(authz_error_to_canonical)
}

/// The `config × write` gate.
///
/// `owner_tenant_id = Some(caller's tenant)` because this is a write, for
/// `threshold_policy::write_scope`'s reason: the membership assertion is what
/// refuses a target outside the compiled scope, the degraded flat-`In` decision
/// not re-checking the property.
async fn write_scope(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<AccessScope, CanonicalError> {
    crate::authz::access_scope(
        enforcer,
        ctx,
        &crate::authz::resource_types::CONFIG,
        crate::authz::actions::WRITE,
        /* owner_tenant_id */ Some(crate::authz::OwnerTenant(ctx.subject_tenant_id())),
        /* resource_id */ None,
    )
    .await
    .map_err(authz_error_to_canonical)
}
