//! Date policy channel and admission-time date preparation (S2-10; 02 §3.2, §4.2, §5.3;
//! DESIGN 02 §3.7 `orders_date_policy`, §3.8 policy channel; D-60, D-121).
//!
//! - **Promotion** applies the deployment's validated policy rows at startup, in one transaction,
//!   through the secure repositories and the database's revision guards. It runs only when the
//!   stored rows differ, so an unchanged promotion needs no write privilege. No endpoint writes
//!   the table.
//! - **Startup** refuses a missing or invalid platform default.
//! - **Preparation** reads the effective row once, with the database clock in the same
//!   statement, and freezes the [`DateBasis`] that dependent calls, the engine's
//!   `capture.date-basis` guard and admission all use.
//! - **Admission** materialises the resolved triple and the identical snapshot on the admitted
//!   line, copying the authored term verbatim. Nothing re-derives it later.
use std::collections::{BTreeMap, BTreeSet};

use time::{Date, OffsetDateTime};
use toolkit_db::secure::ScopeError;
use toolkit_db::{Db, DbError};
use toolkit_security::access_scope::{AccessScope, ScopeConstraint, ScopeFilter};
use uuid::Uuid;

use crate::config::{DatePolicyConfig, DatePolicySwitches};
use crate::domain::dates::{
    DateBasis, DatePolicySnapshot, DateSource, PLATFORM_DEFAULT_POLICY_ID, PolicyFault, PolicyRow,
    ResolvedDate, SuppliedDates, utc_date,
};
use crate::infra::storage::entity::{date_policy, draft_content, order_line};
use crate::infra::storage::repo::{TransactionRunner, mutable, private};

/// The largest number of override and retirement rows one promotion may carry.
pub const MAX_PROMOTED_ROWS: usize = 10_000;

/// A validated promotion: the default's switches, overrides by resource tenant, retirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatePolicyPlan {
    platform_default: DatePolicySwitches,
    overrides: BTreeMap<Uuid, DatePolicySwitches>,
    retired: BTreeSet<Uuid>,
}
impl DatePolicyPlan {
    /// Validate the configured promotion.
    ///
    /// # Errors
    /// A nil tenant, a duplicated tenant, a tenant both overridden and retired, or more than
    /// [`MAX_PROMOTED_ROWS`] rows.
    pub fn from_config(config: &DatePolicyConfig) -> anyhow::Result<Self> {
        anyhow::ensure!(
            config.overrides.len() + config.retired_overrides.len() <= MAX_PROMOTED_ROWS,
            "bss-orders-lifecycle: date_policy carries more than {MAX_PROMOTED_ROWS} rows"
        );
        let mut overrides = BTreeMap::new();
        for o in &config.overrides {
            anyhow::ensure!(
                !o.resource_tenant_id.is_nil(),
                "bss-orders-lifecycle: date_policy override needs a resource tenant"
            );
            let switches = DatePolicySwitches {
                service_activation_required: o.service_activation_required,
                acceptance_due_required: o.acceptance_due_required,
            };
            anyhow::ensure!(
                overrides.insert(o.resource_tenant_id, switches).is_none(),
                "bss-orders-lifecycle: date_policy overrides tenant {} twice",
                o.resource_tenant_id
            );
        }
        let mut retired = BTreeSet::new();
        for t in &config.retired_overrides {
            anyhow::ensure!(
                !t.is_nil() && retired.insert(*t),
                "bss-orders-lifecycle: date_policy retired_overrides must be distinct resource tenants"
            );
            anyhow::ensure!(
                !overrides.contains_key(t),
                "bss-orders-lifecycle: date_policy tenant {t} is both overridden and retired"
            );
        }
        Ok(Self {
            platform_default: config.platform_default,
            overrides,
            retired,
        })
    }

    /// The scope of every row this promotion reads or writes: the permanent default by its
    /// identity, and each named tenant's override by its resource tenant.
    fn scope(&self) -> AccessScope {
        policy_scope(self.overrides.keys().chain(&self.retired).copied())
    }
}

/// The platform default by identity, plus the named tenants' overrides.
fn policy_scope(tenants: impl IntoIterator<Item = Uuid>) -> AccessScope {
    let tenants: Vec<Uuid> = tenants.into_iter().collect();
    let mut constraints = vec![ScopeConstraint::new(vec![ScopeFilter::in_uuids(
        "id",
        vec![PLATFORM_DEFAULT_POLICY_ID],
    )])];
    if !tenants.is_empty() {
        constraints.push(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
            "resource_tenant_id",
            tenants,
        )]));
    }
    AccessScope::from_constraints(constraints)
}

/// What one promotion changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromotionReport {
    pub platform_default_changed: bool,
    pub overrides_inserted: usize,
    pub overrides_updated: usize,
    pub overrides_retired: usize,
}
impl PromotionReport {
    #[must_use]
    pub fn changed(&self) -> bool {
        self.platform_default_changed
            || self.overrides_inserted + self.overrides_updated + self.overrides_retired > 0
    }
}

/// Policy store failure.
#[derive(Debug, thiserror::Error)]
pub enum PolicyStoreError {
    #[error(transparent)]
    Fault(#[from] PolicyFault),
    #[error("date policy store: {0}")]
    Scope(#[from] ScopeError),
    #[error("date policy store: {0}")]
    Db(#[from] DbError),
}

fn switches(row: &date_policy::Model) -> DatePolicySwitches {
    DatePolicySwitches {
        service_activation_required: row.service_activation_required,
        acceptance_due_required: row.acceptance_due_required,
    }
}

fn platform(rows: &[date_policy::Model]) -> Result<&date_policy::Model, PolicyFault> {
    rows.iter()
        .find(|r| r.policy_id == PLATFORM_DEFAULT_POLICY_ID)
        .ok_or(PolicyFault::Missing)
        .and_then(|r| {
            (r.resource_tenant_id.is_none() && r.revision > 0)
                .then_some(r)
                .ok_or(PolicyFault::Invalid)
        })
}

fn tenant_row(rows: &[date_policy::Model], tenant: Uuid) -> Option<&date_policy::Model> {
    rows.iter().find(|r| r.resource_tenant_id == Some(tenant))
}

/// Compare the plan with stored rows: `true` when some row must be written.
fn differs(plan: &DatePolicyPlan, rows: &[date_policy::Model]) -> Result<bool, PolicyFault> {
    if switches(platform(rows)?) != plan.platform_default {
        return Ok(true);
    }
    let override_differs = plan
        .overrides
        .iter()
        .any(|(t, s)| tenant_row(rows, *t).is_none_or(|r| switches(r) != *s));
    let retired_present = plan.retired.iter().any(|t| tenant_row(rows, *t).is_some());
    Ok(override_differs || retired_present)
}

/// Apply `plan` through the policy channel (D-121). An unchanged plan reads only. Otherwise one
/// transaction locks the platform default (the namespace serializer) and the named overrides,
/// re-compares, and writes each change with a revision above the namespace high-water mark:
/// the default advances by one; an inserted or changed override takes the default's current
/// revision plus one (the database then advances the default past it); a retired override is
/// deleted, which also advances the default. A failure writes nothing.
///
/// # Errors
/// A missing or invalid stored default, a store failure or a refused write (for example a
/// connection without the policy role's privileges).
pub async fn promote(db: &Db, plan: &DatePolicyPlan) -> Result<PromotionReport, PolicyStoreError> {
    let scope = plan.scope();
    let rows = private::date_policy_rows(&db.conn()?, &scope, false).await?;
    if !differs(plan, &rows)? {
        return Ok(PromotionReport::default());
    }
    let plan = plan.clone();
    db.transaction_ref_mapped(move |tx| Box::pin(async move { apply(tx, &scope, &plan).await }))
        .await
}

async fn apply(
    tx: &impl TransactionRunner,
    scope: &AccessScope,
    plan: &DatePolicyPlan,
) -> Result<PromotionReport, PolicyStoreError> {
    let landed = OffsetDateTime::now_utc();
    let mut report = PromotionReport::default();
    // Take the namespace lock (the permanent default, which every override write locks too)
    // in its own statement first. Under READ COMMITTED a waiting `FOR UPDATE` never returns
    // rows inserted after its statement began, so the rows must be read after the wait, or a
    // replica promoting the same plan concurrently would insert a duplicate override.
    private::date_policy_rows(tx, &policy_scope([]), true).await?;
    let rows = private::date_policy_rows(tx, scope, true).await?;
    let stored = platform(&rows)?.clone();
    if switches(&stored) != plan.platform_default {
        let proposed = date_policy::Model {
            service_activation_required: plan.platform_default.service_activation_required,
            acceptance_due_required: plan.platform_default.acceptance_due_required,
            revision: stored.revision + 1,
            updated_at: landed,
            ..stored.clone()
        };
        mutable::replace_date_policy(tx, scope, &stored, proposed).await?;
        report.platform_default_changed = true;
    }
    for (tenant, wanted) in &plan.overrides {
        let current = tenant_row(&rows, *tenant);
        if current.is_some_and(|r| switches(r) == *wanted) {
            continue;
        }
        // The namespace high-water mark is the default's revision as of now in this transaction.
        let high_water = private::find_date_policy(tx, scope, PLATFORM_DEFAULT_POLICY_ID)
            .await?
            .ok_or(PolicyFault::Missing)?
            .revision;
        let proposed = date_policy::Model {
            policy_id: current.map_or_else(Uuid::new_v4, |r| r.policy_id),
            resource_tenant_id: Some(*tenant),
            service_activation_required: wanted.service_activation_required,
            acceptance_due_required: wanted.acceptance_due_required,
            revision: high_water + 1,
            updated_at: landed,
        };
        if let Some(current) = current {
            mutable::replace_date_policy(tx, scope, current, proposed).await?;
            report.overrides_updated += 1;
        } else {
            private::insert_date_policy(tx, scope, proposed).await?;
            report.overrides_inserted += 1;
        }
    }
    for tenant in &plan.retired {
        if let Some(current) = tenant_row(&rows, *tenant) {
            mutable::delete_date_policy_override(tx, scope, current).await?;
            report.overrides_retired += 1;
        }
    }
    Ok(report)
}

/// Startup check (DESIGN 02 §3.7): the platform default exists and is valid. A missing default
/// is a deployment failure, never a permissive policy.
///
/// # Errors
/// A missing or invalid default, or a store failure.
pub async fn verify_platform_default(db: &Db) -> Result<DatePolicySnapshot, PolicyStoreError> {
    let rows = private::date_policy_rows(&db.conn()?, &policy_scope([]), false).await?;
    let default = platform(&rows)?;
    Ok(DatePolicySnapshot::effective(
        Uuid::nil(),
        &[policy_row(default)],
    )?)
}

fn policy_row(row: &date_policy::Model) -> PolicyRow {
    PolicyRow {
        policy_id: row.policy_id,
        resource_tenant_id: row.resource_tenant_id,
        service_activation_required: row.service_activation_required,
        acceptance_due_required: row.acceptance_due_required,
        revision: row.revision,
    }
}

/// Install the configured promotion, then verify the default (02 §5.3: startup fails without
/// the required default).
///
/// # Errors
/// Any promotion or verification failure: the gear refuses to start.
pub async fn install(db: &Db, plan: Option<&DatePolicyPlan>) -> anyhow::Result<()> {
    if let Some(plan) = plan {
        let report = promote(db, plan)
            .await
            .map_err(|e| anyhow::anyhow!("bss-orders-lifecycle: date policy promotion: {e}"))?;
        if report.changed() {
            tracing::info!(
                platform_default_changed = report.platform_default_changed,
                overrides_inserted = report.overrides_inserted,
                overrides_updated = report.overrides_updated,
                overrides_retired = report.overrides_retired,
                "bss-orders-lifecycle: date policy promoted"
            );
        }
    }
    verify_platform_default(db)
        .await
        .map_err(|e| anyhow::anyhow!("bss-orders-lifecycle: date policy: {e}"))?;
    Ok(())
}

/// Where the proposed UTC date comes from. Production uses the database clock read with the
/// policy snapshot, the same source as the engine's transition timestamp.
#[derive(Debug, Clone, Copy)]
pub enum PreparationClock {
    Database,
    /// Injected instant (tests: UTC-midnight rollover).
    #[cfg(test)]
    Fixed(OffsetDateTime),
}

/// A preparation that could not produce a basis.
#[derive(Debug, thiserror::Error)]
pub enum PrepareDatesError {
    /// Missing or invalid effective policy: the date guard fails (`date-cascade-invalid`).
    #[error(transparent)]
    Policy(#[from] PolicyFault),
    /// Store outage: no basis, so no date-dependent call may proceed.
    #[error("date policy store: {0}")]
    Store(String),
}

/// Prepares the frozen date basis for Preview, submit and amendment (02 §3.2 steps 1-3).
#[derive(Clone)]
pub struct DatePreparer {
    db: Db,
    clock: PreparationClock,
}
impl DatePreparer {
    #[must_use]
    pub fn new(db: Db, clock: PreparationClock) -> Self {
        Self { db, clock }
    }

    /// Snapshot the resource tenant's effective policy once, choose the proposed UTC date and
    /// resolve every line, before any date-dependent external call.
    ///
    /// # Errors
    /// [`PrepareDatesError::Policy`] for a missing/invalid effective policy;
    /// [`PrepareDatesError::Store`] for a store failure.
    pub async fn prepare(
        &self,
        resource_tenant_id: Uuid,
        lines: impl IntoIterator<Item = (Uuid, SuppliedDates)>,
    ) -> Result<DateBasis, PrepareDatesError> {
        // Every order has a resource tenant; a nil one has no effective row to select.
        if resource_tenant_id.is_nil() {
            return Err(PolicyFault::Invalid.into());
        }
        let store = |e: &dyn std::fmt::Display| PrepareDatesError::Store(e.to_string());
        let conn = self.db.conn().map_err(|e| store(&e))?;
        let observed = private::date_policy_rows_at(&conn, &policy_scope([resource_tenant_id]))
            .await
            .map_err(|e| store(&e))?;
        let (rows, observed_at) = observed.ok_or(PolicyFault::Missing)?;
        let rows: Vec<PolicyRow> = rows.iter().map(policy_row).collect();
        let snapshot = DatePolicySnapshot::effective(resource_tenant_id, &rows)?;
        let now = match self.clock {
            PreparationClock::Database => observed_at,
            #[cfg(test)]
            PreparationClock::Fixed(instant) => instant,
        };
        Ok(DateBasis::resolve(
            resource_tenant_id,
            snapshot,
            utc_date(now),
            lines,
        ))
    }
}

/// A draft line's authored values (submit; Preview).
#[must_use]
pub fn supplied_from_draft(line: &draft_content::Model) -> SuppliedDates {
    SuppliedDates {
        contract_effective: line.contract_effective_date,
        service_activation: line.service_activation_date,
        acceptance_due: line.acceptance_due_date,
    }
}

/// An admitted line's resolved values, carried forward as values by an amendment unless its
/// delta changes them (02 §4.2).
#[must_use]
pub fn supplied_from_admitted(line: &order_line::Model) -> SuppliedDates {
    SuppliedDates {
        contract_effective: Some(line.contract_effective_date),
        service_activation: line.service_activation_date,
        acceptance_due: line.acceptance_due_date,
    }
}

/// Why an admitted line row could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdmittedLineError {
    #[error("the date basis was prepared for another resource tenant")]
    ForeignBasis,
    #[error("the line has no resolved dates in the basis")]
    Unresolved,
    #[error("the basis does not retain the line's authored dates")]
    AuthoredMismatch,
    #[error("an admitted line needs an authored billing cycle")]
    BillingCycle,
}

/// The dated part of an admitted `orders_order_line` row from its draft line (02 §4.2, 01 §3.7):
/// the three resolved dates and the identical policy snapshot. Authored content, including the
/// D-193 term intent, duration and kind, is copied verbatim. Pricing, pin and overlap columns
/// are left empty for the submit/amendment contribution that owns them (S3-12, S4).
///
/// The basis must have been prepared for the order's `resource_tenant_id`, and its supplied
/// values must be exactly the draft line's authored dates: an authored value is retained, an
/// unauthored one is defaulted (02 §4.2). A basis prepared before a later draft edit therefore
/// cannot admit dates the line no longer carries.
///
/// # Errors
/// A basis for another resource tenant, a line without a resolved triple (invalid or absent
/// from the basis), a resolution that does not retain the authored dates, or no cycle.
pub fn admitted_line(
    draft: &draft_content::Model,
    resource_tenant_id: Uuid,
    version: i32,
    basis: &DateBasis,
) -> Result<order_line::Model, AdmittedLineError> {
    if basis.resource_tenant_id != resource_tenant_id {
        return Err(AdmittedLineError::ForeignBasis);
    }
    let dates = basis
        .line(draft.line_id)
        .ok_or(AdmittedLineError::Unresolved)?;
    let retained = |authored: Option<Date>, resolved: &ResolvedDate| match authored {
        Some(value) => resolved.source == DateSource::Supplied && resolved.date() == value,
        None => resolved.source != DateSource::Supplied,
    };
    let authored = supplied_from_draft(draft);
    if !(retained(authored.contract_effective, &dates.contract_effective)
        && retained(authored.service_activation, &dates.service_activation)
        && retained(authored.acceptance_due, &dates.acceptance_due))
    {
        return Err(AdmittedLineError::AuthoredMismatch);
    }
    Ok(order_line::Model {
        order_id: draft.order_id,
        version,
        line_id: draft.line_id,
        commercial_attempt_id: None,
        pricing_acceptance_id: None,
        pricing_request_digest: None,
        pricing_terms_digest: None,
        plan_id: draft.plan_id,
        plan_revision_id: draft.plan_revision_id,
        selected_items: draft.selected_items.clone(),
        currency: draft.currency.clone(),
        contract_effective_date: dates.contract_effective.date(),
        service_activation_date: Some(dates.service_activation.date()),
        acceptance_due_date: Some(dates.acceptance_due.date()),
        term_duration: draft.term_duration.clone(),
        term_kind: draft.term_kind.clone(),
        authored_term: draft.authored_term.clone(),
        billing_cycle: draft
            .billing_cycle
            .clone()
            .ok_or(AdmittedLineError::BillingCycle)?,
        order_pin: None,
        overlap_scope_key: None,
        date_policy_switch_state: basis.policy.switch_state(),
    })
}

/// The proposed UTC date a basis was prepared for.
#[must_use]
pub fn proposed_date(basis: &DateBasis) -> Date {
    basis.proposed_utc_date.0
}
