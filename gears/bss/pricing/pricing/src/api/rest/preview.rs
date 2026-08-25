//! `GET /bss-pricing/v1/plans/{planId}/preview` — the base-price preview
//! (`design/04-currency-tax.md` §2's second flow, `inst-pv-api`,
//! `inst-pv-resolve`, `inst-pv-return`).
//!
//! # The published read model only, and that is the whole rule
//!
//! `inst-pv-resolve`: *"Resolve from the **published read model only** (no draft
//! read); base list price rows only, `PriceOverlay` adjustments disclaimed"*. So
//! this handler reaches the same way `api::rest::windows`' sellability read does
//! — the pin frontier, then the delta at that version — and never touches
//! `pricing_price`. A preview served off the truth side would show a partner a
//! draft nobody has approved.
//!
//! # Fail closed, and never FX
//!
//! `inst-mc-nofx` is the sharpest sentence in the slice: *"No FX derivation ever:
//! a missing `(currency, region)` row is simply absent — preview/publish paths
//! fail closed on it, no base-currency fallback"*. So an absent market is
//! [`PRICE_ROW_ABSENT`] (404) and there is deliberately **no** nearest-currency,
//! no base-currency and no region-fallback branch anywhere below. `Future
//! currencyFallbackPolicy` is named in §1.5 as exactly that — future.
//!
//! The 404 is the design set's own status for it, and it is right: the caller
//! asked for a price on a market this plan does not sell, so the resource they
//! named does not exist. It is **not** an empty 200 — a preview that answered
//! "no price" with a success would be indistinguishable, at a partner's end, from
//! a price of zero.
//!
//! # The disclaimer is part of the contract
//!
//! §2's success scenario requires *"an explicit disclaimer that
//! Contract/`PriceOverlays` apply at purchase (Tariffs evaluates)"*. It is a
//! field of the response rather than prose in the description, because a machine
//! consumer has to be able to carry it into whatever it renders: this is a
//! **list** price and the amount actually charged is Tariffs' to compute.
//!
//! Overlays are excluded from the preview for the same reason `inst-plv-disclosure`
//! gives one slice over — the base-price preview is the surface an overlay's
//! `restricted` disclosure keeps it out of.
//!
//! # The gate is `plan × preview`, which is not `plan × read`
//!
//! §10 and §2 both say so: the preview needs *"the explicit preview grant … an
//! **extra assignment** beyond the `FinanceManager` role: the default role matrix
//! does not carry `plan × preview`"*. Gating it on `read` would hand every holder
//! of the ordinary catalog read a surface the matrix deliberately withholds, and
//! no allow/deny fixture can see the difference — which is why `rest_authz`'s
//! census asserts the pair.

use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, http::StatusCode};
use serde::Deserialize;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::operation_builder::{ParamLocation, ParamSpec};
use toolkit::api::{OpenApiRegistry, operation_builder::OperationBuilder};
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::auth_context::require_authenticated;
use crate::api::rest::error::authz_error_to_canonical;
use crate::api::rest::state::GovernanceState;
use crate::domain::error::DomainError;
use crate::domain::lifecycle::LifecycleState;
use crate::domain::money::CurrencyCode;
use crate::domain::ports::metrics::PreviewFailClosed;
use crate::domain::read_model::SubjectRef;
use crate::domain::scope_key::{ChargeKind, PlanId, PriceEligibility, PriceOverlay, Region};
use crate::infra::storage::repo::{pin_frontier_repo, read_model_repo};
use crate::infra::storage::repo_failure;

/// `OpenAPI` tag (DE0205).
const TAG: &str = "BSS Pricing Catalog";

/// The preview resource.
///
/// The literal is repeated in the `OperationBuilder` call because DE0801
/// validates a **literal** argument and silently passes a `const` one; the two
/// spellings are pinned together by `tests/module_test.rs`'s route census.
pub const PLAN_PREVIEW: &str = "/bss-pricing/v1/plans/{planId}/preview";

/// A market with no published row (§5, 404 — fail closed, no FX).
pub const PRICE_ROW_ABSENT: &str = "PRICE_ROW_ABSENT";

/// The disclaimer §2 requires on every preview.
///
/// A constant, so the sentence a partner is shown cannot drift between two
/// responses of one deployment.
const OVERLAY_DISCLAIMER: &str = "This is the catalog base list price for the requested market. Contract terms and \
     PriceOverlays may apply at purchase and are evaluated by Tariffs; the amount actually \
     charged may differ.";

/// `?currency=&region=` — both required.
#[derive(Debug, Deserialize)]
struct PreviewQuery {
    currency: Option<String>,
    region: Option<String>,
}

/// One market's base list price.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PreviewView {
    /// The plan previewed.
    pub plan_id: Uuid,
    /// The catalog version the answer was resolved at, so a caller can pin what
    /// they were shown.
    pub catalog_version: u64,
    /// The requested currency, echoed.
    pub currency: String,
    /// The requested region, echoed.
    pub region: String,
    /// The base list amount in minor units.
    ///
    /// NULL on a row whose money is a **rate** — see [`Self::unit_rate_nano_minor`].
    pub amount_minor: Option<i64>,
    /// The base list **rate**, in nano-minor units, for the rows that price from
    /// one.
    ///
    /// D-311 gave `per_unit` the pair `(wants_amount = false, wants_rate = true)`:
    /// a per-seat row's money lives here and its `amount_minor` is NULL *by rule*,
    /// so a view carrying only the amount quotes such a plan no price at all. That
    /// is the modal subscription shape — recurring, `per_unit`, seat-counted.
    ///
    /// A member of its own rather than folded into `amount_minor`, because folding
    /// them is exactly the conflation D-311 removed, and because a consumer reading
    /// a nano-minor rate as a minor amount is wrong by a factor of a billion.
    pub unit_rate_nano_minor: Option<i64>,
    /// Whether that amount is tax-inclusive (§2's success scenario).
    pub tax_inclusive: bool,
    /// D-154's resolved effective tax category, as the version froze it.
    pub resolved_tax_category: Option<String>,
    /// C3's gate: authorable and **previewable**, but not sellable on this
    /// market until Tax Engine GA. Carried because §2 explicitly permits the
    /// preview of a gated row — a caller has to be able to tell the two apart.
    pub not_sellable_ga: bool,
    /// §2's **tier summary**: how many tier bands this market's rows carry, or
    /// `None` when none do.
    ///
    /// A count rather than the bands themselves — a full band table is what
    /// `GET …/prices` is for. It exists because a tiered or usage-priced market
    /// has **no** single `amountMinor`, so without it §2's success scenario is
    /// unanswerable for exactly the plans whose pricing is most worth previewing.
    pub tier_band_count: Option<u32>,
    /// The trial days a consumer surface shows, when the plan declares any.
    pub display_trial_days: Option<i64>,
    /// §2's required disclaimer.
    pub disclaimer: String,
}

fn currency_param() -> ParamSpec {
    ParamSpec {
        name: "currency".to_owned(),
        location: ParamLocation::Query,
        required: true,
        description: Some(
            "The ISO 4217 code of the market to preview. Required: the catalog performs no FX \
             and has no base currency, so there is no answer to `what does this plan cost` \
             without one."
                .to_owned(),
        ),
        param_type: "string".to_owned(),
        // Scalar: every parameter this gear declares is single-valued.
        // `array` arrived upstream for `?tag=a&tag=b` repeats, which no route
        // here has.
        array: false,
    }
}

fn region_param() -> ParamSpec {
    ParamSpec {
        name: "region".to_owned(),
        location: ParamLocation::Query,
        required: true,
        description: Some(
            "The commercial region of the market to preview. Required for `currency`'s reason: a \
             price row is keyed on the pair, and a region-less query names no row. This is the \
             pricing region, not the IdP authorization-region claim."
                .to_owned(),
        ),
        param_type: "string".to_owned(),
        // Scalar: every parameter this gear declares is single-valued.
        // `array` arrived upstream for `?tag=a&tag=b` repeats, which no route
        // here has.
        array: false,
    }
}

/// Build the Axum router for the preview and register it.
pub fn router(state: Arc<GovernanceState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::get("/bss-pricing/v1/plans/{planId}/preview")
        .operation_id("bss_pricing.preview_plan_price")
        .summary("Preview a plan's base list price on one market")
        .description(
            "The catalog **base list price** for one `(currency, region)` market, resolved from \
             the **published read model only** - never from a draft, so a preview cannot show a \
             price nobody has approved. The response carries the amount, its `taxInclusive` \
             display basis, the resolved tax category the catalog version froze, the trial days \
             a consumer surface shows, and an explicit **disclaimer**: Contract terms and \
             PriceOverlays may apply at purchase and are evaluated by Tariffs, so the amount \
             actually charged may differ. Overlay adjustments are deliberately not applied here. \
             **Fails closed on an absent market**: if the plan publishes no row for the \
             requested pair the answer is `404` `PRICE_ROW_ABSENT`, never a converted price and \
             never a base-currency fallback - the catalog performs no FX under any circumstance, \
             and a currency fallback policy is a named Future item rather than an omission. A \
             row flagged `notSellableGa` is previewable and is returned with the flag set, which \
             is what lets a caller distinguish `not sold here` from `not sellable yet`. Gates on \
             `plan` x `preview`, which is deliberately **not** `plan` x `read`: the preview \
             grant is an extra assignment the default role matrix does not carry.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("planId", "The plan to preview.")
        .param(currency_param())
        .param(region_param())
        .handler(preview_plan_price)
        .json_response_with_schema::<PreviewView>(
            openapi,
            StatusCode::OK,
            "The base list price for the requested market.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(Router::new(), openapi);

    router.layer(Extension(state))
}

async fn preview_plan_price(
    Extension(state): Extension<Arc<GovernanceState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(plan_id): Path<Uuid>,
    Query(query): Query<PreviewQuery>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let plan_id = PlanId::new(plan_id);
    let scope = preview_scope(&enforcer, &ctx, plan_id.get()).await?;

    // **After the gate.** A caller without the preview grant is told that, rather
    // than that their query is malformed — the ordering `schedule_window` argues
    // for its own parameters and `rest_authz`'s catalogued-pair property depends
    // on.
    let (currency, region) = market_of(&query).inspect_err(|_| {
        // The caller named no market. Counted because it is a distinct
        // remediation from "nobody authored that row" — this one is a client
        // fault and needs no catalog change.
        state
            .metrics
            .preview_failclosed(PreviewFailClosed::MarketNotNamed);
    })?;
    let tenant = ctx.subject_tenant_id();

    let conn = state
        .db
        .conn()
        .map_err(|e| CanonicalError::from(DomainError::Internal(format!("preview conn: {e}"))))?;

    // **Two constructors, one wire answer.** Both are `404 PRICE_ROW_ABSENT` —
    // §5 declares one code — but they are different facts to an operator: a
    // market nobody authored, versus a tenant that has published nothing at all.
    // The counter is where that distinction lives, so the constructors are
    // separate purely to keep each `return` counting the reason it means.
    let unpublished = || {
        CanonicalError::from(DomainError::PriceRowAbsent(format!(
            "plan {plan_id} has no published catalog version, so there is no price to preview \
             on {}/{region} or on any other market",
            currency.as_str()
        )))
    };
    let absent = || {
        state
            .metrics
            .preview_failclosed(PreviewFailClosed::MarketAbsent);
        CanonicalError::from(DomainError::PriceRowAbsent(format!(
            "plan {plan_id} publishes no price row on {}/{region}. The catalog performs no FX \
             and has no base-currency fallback, so an absent market is an absent price rather \
             than a converted one",
            currency.as_str()
        )))
    };

    // The frontier first, then the delta at it — `windows::sellability_facts`'
    // arrangement, and for its reason: a read taken at "the latest row" rather
    // than at the pinned version could serve two callers two different answers
    // while the projector is mid-sweep.
    let Some(frontier) = pin_frontier_repo::read_at(&conn, &scope, tenant)
        .await
        .map_err(|e| CanonicalError::from(repo_failure(&e)))?
    else {
        // **A different reason from an absent market**, though the caller sees
        // the same 404: no version of this tenant's catalog is readable, so no
        // row of any market exists to quote. An operator seeing this series climb
        // has a different job from one seeing `market_absent` climb.
        //
        // "Not readable" rather than "never published", which is what the reason
        // is named for and is the wider of the two: `read_at` answers `None` both
        // for a tenant that has never published and for one whose only
        // projections are **later than the pin**, i.e. published and not yet
        // warmed. The remediations differ — publish, versus wait for the
        // projector — and this series does not separate them. Recorded rather
        // than split, because splitting it needs a fact the frontier read does
        // not return.
        state
            .metrics
            .preview_failclosed(PreviewFailClosed::NoPublishedVersion);
        return Err(unpublished());
    };
    let Some(delta) = read_model_repo::delta_at(
        &conn,
        &scope,
        tenant,
        &SubjectRef::Plan(plan_id.get()),
        frontier.catalog_version,
    )
    .await
    .map_err(|e| CanonicalError::from(repo_failure(&e)))?
    else {
        state
            .metrics
            .preview_failclosed(PreviewFailClosed::NoPublishedVersion);
        return Err(unpublished());
    };

    let rows = market_rows(&delta.payload, currency.as_str(), region.as_str());
    let row = base_amount_row(&rows, terminal_phase_id(&delta.payload)).ok_or_else(absent)?;

    Ok(Json(PreviewView {
        plan_id: plan_id.get(),
        catalog_version: delta.catalog_version.get(),
        currency: currency.as_str().to_owned(),
        region: region.as_str().to_owned(),
        amount_minor: row["amountMinor"].as_i64(),
        unit_rate_nano_minor: row["unitRateNanoMinor"].as_i64(),
        tax_inclusive: row["taxInclusive"].as_bool().unwrap_or(false),
        resolved_tax_category: row["resolvedTaxCategory"].as_str().map(ToOwned::to_owned),
        // **Absent reads as gated, and over the whole market.** Two fail-closed
        // readings, both deliberate. A delta frozen before `notSellableGa`
        // existed carries no such key, and defaulting that to `false` would tell
        // a partner a C3-gated market is sellable — in a handler whose own header
        // is titled "fail closed" — with no way to heal, since a frozen version
        // is never re-projected. And the gate is per **market**: if any row of it
        // is gated the market is not sellable, so reporting only the amount row's
        // flag would understate it.
        not_sellable_ga: rows
            .iter()
            .any(|r| r["notSellableGa"].as_bool().unwrap_or(true)),
        tier_band_count: u32::try_from(
            rows.iter()
                .filter_map(|r| r["bands"].as_array().map(Vec::len))
                .sum::<usize>(),
        )
        .ok()
        .filter(|count| *count > 0),
        display_trial_days: delta.payload["phases"]
            .as_array()
            .and_then(|phases| phases.iter().find_map(|p| p["displayTrialDays"].as_i64())),
        disclaimer: OVERLAY_DISCLAIMER.to_owned(),
    })
    .into_response())
}

/// Every row of one market this preview may speak for.
///
/// **Four filters, and each closes a way to quote a price nobody sells.**
///
/// * `priceOverlay = base` — `inst-pv-resolve`'s "base list price rows only".
/// * not `existing_grandfathered` — a frozen generation is not what a *new*
///   purchaser is quoted, and this surface's whole audience is people without a
///   subscription.
/// * **`lifecycleState = published`.** `PROJECTED_ROW_STATES` includes
///   `superseded`, and a supersession stages the successor on the **same**
///   `ScopeKey` while flipping its predecessor — so a market that has ever been
///   repriced carries two byte-identical keys in the frozen delta. Without this
///   filter a plan raised from 12.00 to 15.00 quotes 12.00 for the life of that
///   version, decided only by which `priceId` sorted first, and a frozen version
///   is never re-projected. Found by review.
/// * a **non-usage** charge kind, in [`base_amount_row`].
fn market_rows<'a>(
    payload: &'a serde_json::Value,
    currency: &str,
    region: &str,
) -> Vec<&'a serde_json::Value> {
    payload["prices"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| {
                    // Every token below is read back through the enum that
                    // rendered it into the delta, not respelled here: the payload
                    // is written by `PriceOverlay`/`PriceEligibility`/
                    // `LifecycleState`'s own `as_str`, and a second spelling on
                    // the reading side is a filter free to stop matching what the
                    // projector writes.
                    let key = &row["scopeKey"];
                    key["currency"] == currency
                        && key["region"] == region
                        && key["priceOverlay"] == PriceOverlay::Base.as_str()
                        && key["priceEligibility"]
                            != PriceEligibility::ExistingGrandfathered.as_str()
                        && row["lifecycleState"] == LifecycleState::Published.as_str()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The row whose amount **is** the base list price.
///
/// One market legitimately holds many rows — `phase`, `chargeKind`, `meter` and
/// `dimensionKey` are all scope-key axes — and they are not interchangeable. A
/// row whose money lives in its tier bands has a NULL `amountMinor`, so a preview
/// taking whichever row sorted first answers `null` for a hybrid plan that plainly
/// has a monthly price.
///
/// **Banded money is a property of the model, not of the charge kind**, and this
/// doc conflated them: [`PriceRow::is_usage`](crate::domain::price_row::PriceRow::is_usage)
/// reads `charge_kind` while
/// [`PriceRow::is_tiered`](crate::domain::price_row::PriceRow::is_tiered) reads
/// `model_kind`. It is **not** the case that *"a usage row's money lives in its
/// tier bands and its `amountMinor` is NULL by rule"*: `per_unit` is legal on a
/// usage row and keeps its money in `amount_minor`, so a usage row's amount is not
/// NULL by any rule.
///
/// **The mirror claim — that a terminal `recurring` row may price in bands — was
/// asserted here and is false**, which is why the naming predicate no longer asks
/// about the money without that being a bug fix. `inst-mk-chargekind` (D-18) is
/// registered in `price_row_rules()` and refuses `graduated`/`volume`/`package` on
/// a `recurring` row (`MODEL_KIND_CHARGEKIND_MISMATCH`; §3 puts tiered per-seat
/// pricing in Future scope in the same sentence), and `AMOUNT_PLACEMENT_INVALID`
/// refuses `flat`/`per_unit` — the only kinds a non-usage row may carry — with an
/// absent price. **Every publishable terminal recurring row therefore has a
/// price** — but not necessarily an *amount*: since D-311 the placement matrix
/// sends `flat` to `amount_minor` and `per_unit` to `unit_rate`, and refuses the
/// other column on each. "Therefore has an amount" was true only before that
/// split, and a view built on it carries one money member and quotes a per-seat
/// plan nothing at all. Dropping the conjunct
/// is still a simplification: the name is about the scope key, and the money is a
/// different question — the view has to read **both** columns.
///
/// **D-244 names the row**: the *terminal phase's* `all_subscriptions`
/// **recurring** row. "The catalog base list price" reads as though a market had
/// one: picking the first non-usage row carrying an amount, ties broken on
/// `priceId`, is deterministic and arbitrary — a plan with a trial-phase row
/// beside a full-price row has two candidates and is quoted whichever `priceId`
/// sorts first.
///
/// The owner's ruling is that a prospective purchaser should see **the row they
/// would actually be charged first**, which is the steady state a trial converts
/// into rather than the trial. Terminality is structural — a phase whose
/// `convertsToPhaseId` is null — and never `kind`, because C-4 exists precisely
/// because those two were once conflated.
///
/// **A preference, not a filter**, and the fallbacks below are the reason. A
/// market may have no phase chain in its payload, no recurring row, or no
/// `amountMinor` at all — a usage-priced market has a price, it just lives in the
/// tier bands. Filtering instead of preferring made such a market answer 404, as
/// though the plan did not sell there.
fn base_amount_row<'a>(
    rows: &[&'a serde_json::Value],
    terminal_phase: Option<&str>,
) -> Option<&'a serde_json::Value> {
    let mut ordered: Vec<&'a serde_json::Value> = rows.to_vec();
    ordered.sort_by_key(|row| row["priceId"].as_str().unwrap_or_default().to_owned());

    // The naming is the scope key and **nothing about the money**. Whether the row
    // carries a single `amountMinor` or prices in bands is a different question,
    // and asking it here is what let a *trial* row be quoted: a tiered terminal
    // row failed the name and the fallback below then crossed the phase boundary.
    let named = terminal_phase.and_then(|phase| {
        ordered.iter().copied().find(|row| {
            row["scopeKey"]["phase"] == phase
                && row["scopeKey"]["priceEligibility"]
                    == PriceEligibility::AllSubscriptions.as_str()
                && row["scopeKey"]["chargeKind"] == ChargeKind::Recurring.as_str()
        })
    });
    if named.is_some() {
        return named;
    }

    ordered
        .iter()
        .copied()
        .find(|row| {
            row["scopeKey"]["chargeKind"] != ChargeKind::Usage.as_str()
                && !row["amountMinor"].is_null()
        })
        // **A preference, not a filter**, and the difference is a market priced
        // solely by usage. Excluding metered rows outright made such a market
        // answer 404 — as though the plan did not sell there — which contradicts
        // §2 carrying a "tier summary" at all: a metered market has a price, it
        // just has no `amountMinor`. Found because a probe removing the exclusion
        // reddened nothing, and reasoning about *why* showed the exclusion was
        // doing something it was not meant to.
        .or_else(|| ordered.first().copied())
}

/// The phase a plan converts *into* and never out of — D-244's referent.
///
/// **Structural, not a `kind`.** `PlanPhase::converts_to_phase_id == None` *is*
/// terminality, and C-4 exists because that was once conflated with
/// `kind = evergreen`, which let a `trial`-terminal chain through. Reading the
/// projected `convertsToPhaseId` keeps this on the same definition the domain
/// enforces rather than on a second one.
///
/// Answers `None` for a payload with no phase array — a delta frozen before
/// phases were projected — and the selection then falls back, which is the
/// honest direction: a market with no readable phase chain still has a price.
fn terminal_phase_id(payload: &serde_json::Value) -> Option<&str> {
    payload["phases"]
        .as_array()?
        .iter()
        .find(|phase| phase["convertsToPhaseId"].is_null())
        .and_then(|phase| phase["phaseId"].as_str())
}

/// Both query parameters, parsed and non-blank.
fn market_of(query: &PreviewQuery) -> Result<(CurrencyCode, Region), CanonicalError> {
    let currency = query.currency.as_deref().unwrap_or_default();
    let region = query.region.as_deref().unwrap_or_default();
    let currency = CurrencyCode::new(currency).map_err(CanonicalError::from)?;
    let region = Region::new(region).map_err(CanonicalError::from)?;
    Ok((currency, region))
}

/// The `plan × preview` gate.
///
/// **Not `plan × read`.** §2 and §10 both make the preview grant an extra
/// assignment the default role matrix does not carry, so gating on `read` would
/// hand it to every holder of the ordinary catalog read. No allow/deny fixture
/// can see that difference, which is why `rest_authz`'s census asserts the pair —
/// the same argument `plan × publish` carries.
async fn preview_scope(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &SecurityContext,
    plan_id: Uuid,
) -> Result<AccessScope, CanonicalError> {
    crate::authz::access_scope(
        enforcer,
        ctx,
        &crate::authz::resource_types::PLAN,
        crate::authz::actions::PREVIEW,
        // **`None`, because this is a read** -- `authz::access_scope`'s stated
        // two-way split: reads let the PDP derive the scope from the subject and
        // its role, never from a caller-supplied tenant, and only a write passes
        // `Some(target_tenant)` so the membership assertion has a target to test.
        // `PREVIEW` is a read by `authz`'s own definition, and the last gate that
        // would still pass `Some(tenant)`. Nothing escalates -- the value is
        // `ctx.subject_tenant_id()`
        // and never caller-supplied -- but the assertion it ran is the one that
        // fires when a grant is compiled to a tenant the caller does not
        // authenticate in, which is precisely the partner model this grant exists
        // for: it would answer 403 to the caller it was built to serve.
        /* owner_tenant_id */
        None,
        /* resource_id */ Some(crate::authz::ResourceRef(plan_id)),
    )
    .await
    .map_err(authz_error_to_canonical)
}
