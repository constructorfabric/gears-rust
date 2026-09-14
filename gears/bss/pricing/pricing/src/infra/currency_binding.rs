//! Resolving `inst-cb-addon`'s inputs from storage — the add-on coverage
//! `domain::currency_binding` judges.
//!
//! `infra::bundle::referencing_markets`' shape one plane over, and for its
//! reason: the rule is a property of **other plans'** published rows, so a pure
//! walk over one plan shape cannot have it, and the rule set runs twice on one
//! publish — a rule reaching for storage itself could answer differently in the
//! two runs for no authored reason. So the read happens once, in
//! `infra::publish::rule_params`, through this function.
//!
//! # An add-on is named by its SKU, and resolved through the plan that sells it
//!
//! `AddonRule::addon_sku_id` is a **registry** SKU id, not a plan id — S2 keeps
//! it that way deliberately, so a composition survives the add-on being
//! re-planned. What has coverage, though, is a *plan*: rows hang off `plan_id`.
//! So the resolution is `sku_id -> current plan revision -> its published rows'
//! markets`, and an add-on SKU no plan in this tenant sells resolves to **no**
//! coverage rather than to an error.
//!
//! That last part is the fail-closed reading and it is deliberate: an add-on the
//! catalog cannot resolve is one a subscription could not resolve either, so a
//! required add-on naming an unsellable SKU should block the publish — which is
//! what an empty coverage set makes it do.
//!
//! # `published` only, and the eligibility narrowing is the domain's
//!
//! Only `published` rows are coverage, because a draft is not something an order
//! can resolve against. The `existing_grandfathered` narrowing is applied by
//! `domain::currency_binding::sold_markets` on the selling side and mirrored here
//! on the covering side, so the two sides of the comparison mean the same thing —
//! a grandfathered row of an add-on is no more a coverage candidate than a
//! grandfathered row of the plan.

use std::collections::{BTreeMap, BTreeSet};

use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt};
use uuid::Uuid;

use crate::domain::currency_binding::{AddonCoverage, Market};
use crate::domain::lifecycle::LifecycleState;
use crate::domain::money::CurrencyCode;
use crate::domain::plan_shape::PlanShape;
use crate::domain::scope_key::{PriceEligibility, Region};
use crate::infra::storage::RepoError;
use crate::infra::storage::entity::{plan, price};
use crate::infra::storage::repo::plan_repo;

/// What each add-on SKU covers, as this module assembles it.
type CoverageBySku = BTreeMap<Uuid, BTreeSet<Market>>;

/// Resolve the markets each of `shape`'s add-on SKUs covers.
///
/// Only the add-ons the shape actually composes are resolved — the rule's domain
/// is that set, and reading every plan of the tenant to answer a question about
/// three add-ons is a scan where an index probe will do.
///
/// # Errors
/// [`RepoError::Db`] on a scope or storage failure; [`RepoError::CorruptRow`]
/// when a stored currency or region cannot be read as its domain type.
pub async fn addon_coverage(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    shape: &PlanShape,
) -> Result<AddonCoverage, RepoError> {
    let skus: BTreeSet<Uuid> = shape
        .addon_rules
        .iter()
        .map(|rule| rule.addon_sku_id)
        .collect();
    if skus.is_empty() {
        return Ok(AddonCoverage::default());
    }

    // SKU -> the plans currently selling it. A `Vec` per SKU rather than one
    // plan, because nothing in the schema makes a SKU's plan unique and a silent
    // `first()` would pick one of two arbitrarily. How the set is folded into an
    // answer is stated where the fold happens, not here.
    let plans = plan::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(plan::Column::TenantId.eq(tenant_id))
                .add(plan::Column::SkuId.is_in(skus.iter().copied()))
                // `published` **and** `retired`, because both are current
                // (D-31): a retired plan's rows keep resolving for in-flight
                // subscribers, so an add-on on a retired plan still covers what
                // it covered, and reading `retired` as "not published" would
                // block its base plan's remediation publish. Derived from
                // `LifecycleState::is_current_revision` rather than written out,
                // so a widening of that predicate moves this query with it.
                .add(plan::Column::LifecycleState.is_in(plan_repo::current_tokens())),
        )
        .all(runner)
        .await
        .map_err(|e| RepoError::Db(format!("read add-on plans: {e}")))?;

    let mut plan_to_sku: BTreeMap<Uuid, Uuid> = BTreeMap::new();
    for row in &plans {
        if let Some(sku) = row.sku_id {
            plan_to_sku.insert(row.plan_id, sku);
        }
    }
    if plan_to_sku.is_empty() {
        // Every add-on resolves to nothing, which blocks any that are required.
        return Ok(AddonCoverage::default());
    }

    let rows = price::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(price::Column::TenantId.eq(tenant_id))
                .add(price::Column::PlanId.is_in(plan_to_sku.keys().copied()))
                .add(price::Column::LifecycleState.eq(LifecycleState::Published.as_str()))
                // **The one `.ne()` over this vocabulary in the crate**, which is
                // what made a hand-spelled token here fail *open*: a spelling
                // that stopped matching the stored one would match every row and
                // count grandfathered rows as coverage, letting an
                // `inst-cb-addon` publish through that D-95 requires refused.
                // Every `.eq()` sibling drifts to zero rows and is loud. So this
                // predicate renders through the enum that owns the token.
                .add(
                    price::Column::PriceEligibility
                        .ne(PriceEligibility::ExistingGrandfathered.as_str()),
                ),
        )
        .all(runner)
        .await
        .map_err(|e| RepoError::Db(format!("read add-on coverage rows: {e}")))?;

    // Per **plan**, then intersected per SKU — see below.
    let mut by_plan: BTreeMap<Uuid, BTreeSet<Market>> = BTreeMap::new();
    for row in rows {
        if !plan_to_sku.contains_key(&row.plan_id) {
            continue;
        }
        let currency = CurrencyCode::new(&row.currency).map_err(|e| {
            RepoError::CorruptRow(format!("pricing_price.currency `{}`: {e}", row.currency))
        })?;
        let region = Region::new(&row.region).map_err(|e| {
            RepoError::CorruptRow(format!("pricing_price.region `{}`: {e}", row.region))
        })?;
        by_plan
            .entry(row.plan_id)
            .or_default()
            .insert((currency, region));
    }

    // **The intersection across plans sharing a SKU, not the union.**
    //
    // Nothing in the schema makes a SKU's plan unique, and an order resolves the
    // add-on to **one** plan revision — not to the set. Under a union, SKU `S`
    // sold by plan A (EUR/eu only) and plan B (USD/us only) covers both, a base
    // plan selling both publishes clean, and a subscription bound to EUR/eu that
    // resolves `S` through plan B dies at order assembly with an unresolvable
    // line. That is the D-95 failure `inst-cb-addon` exists to prevent, reached
    // through the SKU-resolution door. Found by review; register `T-15`.
    //
    // A plan carrying **no** covering row contributes an empty set and therefore
    // empties the intersection, which is the fail-closed reading: an add-on one
    // of whose plans sells nowhere is one an order may resolve to nowhere.
    let mut by_addon: CoverageBySku = BTreeMap::new();
    for (plan_id, sku) in &plan_to_sku {
        let markets = by_plan.get(plan_id).cloned().unwrap_or_default();
        by_addon
            .entry(*sku)
            .and_modify(|held| *held = held.intersection(&markets).cloned().collect())
            .or_insert(markets);
    }
    Ok(AddonCoverage::new(by_addon))
}
