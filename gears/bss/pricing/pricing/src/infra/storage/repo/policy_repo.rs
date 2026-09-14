//! Repository over `pricing_policy_object` — the per-tenant policy store
//! (`design/01-foundation.md` §3.7).
//!
//! **One repository over the table, and one read.** The approval threshold is not
//! on this row at all — it lives in `pricing_approval_threshold`, per-currency and
//! versioned — while `tax_display_policy_mode` is read at [`crate::infra::publish`]
//! and by `GET /config/tax-display-policy`, and `enforced_migration_notice_days` at
//! [`crate::infra::migration`] when Slice 11 schedules. So [`AuthoringPolicy`]
//! carries **every** content column the row has and one read resolves them all,
//! which is what the two runs of one publish need in order not to disagree about
//! what the tenant configured.
//!
//! What this one reads is exactly what the **authoring** path resolves: the four
//! §14 caps and the descriptor required-set extension D-152 put in this table,
//! and — when the publish path that reads it landed — the
//! **default rounding policy**. That one joined this method rather than getting
//! one of its own because it is resolved at the same moment and by the same
//! caller: the §4.2 step-2 pre-check and the commit's re-validation each build
//! one rule set for one tenant, and two reads to build it would be two chances
//! for the two runs to disagree about what the tenant had configured.
//!
//! # Most content columns have no writer, here or anywhere
//!
//! [`set_tax_display_policy`] and [`set_default_rounding_policy`] are the
//! **only** writers of `pricing_policy_object` in this crate, and each sets
//! exactly one content column — `tax_display_policy_mode` and
//! `default_rounding_policy_ref` respectively — beside the `updated_at_utc` /
//! `updated_by` stamps. Not "there is no upsert here": the narrower statement is
//! the true one, and stating it precisely matters because the columns they leave
//! out are read on live paths:
//!
//! - `additional_required_descriptors` is permanently `[]`, so
//!   [`DescriptorSetComplete::extending_v1`] — D-152's mechanism — never extends,
//!   and `DESCRIPTOR_INCOMPLETE` can only ever check D-48 v1's pinned three.
//! - `enforced_migration_notice_days` is permanently the 60-day floor: a tenant
//!   can raise nothing, though D-49 says they may.
//! - the four §14 caps fall back to [`LimitsConfig`] for every tenant, which is
//!   benign in outcome and still means the per-tenant caps and their `CHECK`s
//!   guard a value nothing in this gear can set.
//!
//! **This is the state of the design set rather than an omission here.** The set
//! names the authoring surfaces over this table — `GET/PUT
//! /bss-pricing/v1/config/tax-display-policy` (S4 §5,
//! `design/04-currency-tax.md`; its authz row, `design/05-governance.md` §5) and
//! `GET/PUT /bss-pricing/v1/config/rounding-policy` (D-320) — and both are built
//! ([`crate::api::rest::tax_display_policy`],
//! [`crate::api::rest::rounding_policy`]). It names **none** for the columns
//! listed above: `/config/approval-threshold-policy` writes
//! `pricing_approval_threshold` and `/config/taxonomies` writes the
//! taxonomy tables, and those, with `/config/artifacts`, are every `/config`
//! path the set declares. D-152 is explicit that this carrier is *provisional*
//! for the four caps and the descriptor extension — the product owner confirmed
//! the columns here *"for now"* and expects them to move to a settings gear that
//! does not exist in this repository, so *"build against this carrier, and
//! expect the move"*. D-49 contemplates raising the notice period by *"an
//! explicit audited policy change"* (S11 `inst-mg-target`) and names no route
//! for one.
//!
//! So the missing writers are owed by a document nobody has written, and minting
//! them here would be the thing D-10/D-13 refuse: a policy change is not a row
//! write. One UPDATE on this table can make every plan of a tenant
//! unpublishable, which is exactly the materiality an approval unit exists to
//! hold — and the one write that does exist earns it by being the surface S4 §5
//! declares, authz-gated, `If-Match`ed and audited. Until the rest are declared
//! somewhere, a tenant's caps and descriptor extension are configurable only the
//! way this gear's own tests configure them: by a row written around it, with
//! the column `CHECK`s as the only guard.
//!
//! # The defect this closes, and why it is not a cache
//!
//! Four ratified numbers and the descriptor required-set extension were called
//! **tenant-configurable** by a ratified NFR and a pinned assumption while the
//! only carrier the code had was the gear's configuration section, which is per
//! **deployment**. Every tenant of a deployment shared one cap, and the
//! descriptor required-set had no declaration surface at all — so
//! `DESCRIPTOR_INCOMPLETE` could only ever check D-48's pinned three and "config
//! extensible without a schema change" described a capability with no
//! configuration to exercise it (D-152).
//!
//! **What D-152 closed was the carrier, not the surface.** The extension has a
//! declared home now and this repository reads it; nothing in the crate writes
//! it, per the section above, so the capability is still exercisable only by a
//! row written around the gear. The half that is paid is that a reader now knows
//! where the value would live and what absence means; the half that is owed is
//! the surface that puts one there.
//!
//! [`PolicyObjectRepo::authoring_policy`] is on the **authoring** path — the
//! §4.2 step-2 pre-check and the publish commit's re-validation — and never on a
//! resolution path. It is a point read on the primary key, once per validation
//! run, so nothing here is memoized: a cached cap is a cap that keeps rejecting
//! plans after an operator has raised it, and the two runs of one publish would
//! be free to disagree about the limit they enforced.
//!
//! # The carrier is provisional
//!
//! These settings read from a pricing table today because there is no settings
//! gear in this repository to read them from; D-152's confirmation records that
//! they are expected to move to one when it exists, and that
//! `gears/simple-user-settings` is not it (per **user**, so a tenant-wide cap
//! has no row to occupy). Build against this repository, and expect the move.

use std::collections::BTreeSet;

use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, JsonValue};
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::config::LimitsConfig;
use crate::domain::audit::AuditStamp;
use crate::domain::migration::{NOTICE_FLOOR_DAYS, NoticePeriod};
use crate::domain::plan_rules::{CustomIntervalBounds, DescriptorSetComplete};
use crate::domain::tax_display::TaxDisplayPolicy;
use crate::infra::storage::entity::policy_object;
use crate::infra::storage::{RepoError, contention_or_db};

/// One tenant's authoring-time configuration, already resolved against the
/// deployment defaults.
///
/// Resolved on the way out of storage rather than at the call sites, because a
/// caller holding "the tenant's value or else the deployment's" is a caller free
/// to forget the second half — and the half it would forget is the one that
/// keeps the ratified launch numbers from moving for every tenant that has
/// configured nothing.
///
/// Not a domain type: it is built from a row and from
/// [`LimitsConfig`](crate::config::LimitsConfig), both of which are
/// infrastructure. What it hands the domain is the two configured rules, already
/// constructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoringPolicy {
    max_tier_bands_per_row: u32,
    max_price_rows_per_plan: u32,
    max_custom_interval_days: u32,
    max_custom_interval_months: u32,
    additional_required_descriptors: Vec<String>,
    default_rounding_policy_ref: Option<String>,
    tax_display_policy_mode: String,
    enforced_migration_notice_days: i64,
}

impl AuthoringPolicy {
    /// The deployment's ratified launch values, which is what a tenant with no
    /// policy row is governed by.
    ///
    /// The descriptor extension is empty here and has no configuration field of
    /// its own: a per-**deployment** extension would let one deployment's
    /// tenants share a Billing contract they do not share, which is the half of
    /// D-152 that carries the sharpest cost. The absent extension is the pinned
    /// v1 three, and nothing else can produce them.
    #[must_use]
    pub fn from_deployment_defaults(limits: &LimitsConfig) -> Self {
        Self {
            max_tier_bands_per_row: limits.max_tier_bands_per_row,
            max_price_rows_per_plan: limits.max_price_rows_per_plan,
            max_custom_interval_days: limits.max_custom_interval_days,
            max_custom_interval_months: limits.max_custom_interval_months,
            additional_required_descriptors: Vec::new(),
            // Absent, and absent is the fail-closed reading: a deployment-wide
            // rounding default would decide the last minor unit of every charge
            // of every tenant that never asked for one, which is precisely the
            // implicit rounding PRD §17.4 refuses. A tenant without an entry
            // simply requires every published row to carry its own.
            default_rounding_policy_ref: None,
            // C4 is fail-closed "for **all** tenants", so a tenant with no
            // policy row is governed by it exactly as one with a row that says
            // so. There is no deployment knob here for the same reason there is
            // none for the rounding default.
            tax_display_policy_mode: TaxDisplayPolicy::FailClosed.as_str().to_owned(),
            // D-49's floor, which is what a tenant with no policy row is governed
            // by. Not a deployment knob: the floor is the ratified minimum notice
            // a subscriber is owed, and a deployment able to lower it would be
            // able to lower it for tenants who never asked.
            enforced_migration_notice_days: NOTICE_FLOOR_DAYS,
        }
    }

    /// The `customEveryN` rule bound to this tenant's caps.
    #[must_use]
    pub const fn interval_bounds(&self) -> CustomIntervalBounds {
        CustomIntervalBounds::new(
            self.max_custom_interval_days,
            self.max_custom_interval_months,
        )
    }

    /// The descriptor rule bound to this tenant's extension of D-48 v1.
    #[must_use]
    pub fn descriptor_rule(&self) -> DescriptorSetComplete {
        DescriptorSetComplete::extending_v1(self.additional_required_descriptors.clone())
    }

    /// The tier-band soft cap in force for this tenant.
    ///
    /// Read by
    /// [`PlanSizeWithinSoftCaps`](crate::domain::publish::rules::PlanSizeWithinSoftCaps)
    /// as of D-160, which named the advisory code `nfr-size-limits`' **SHOULD**
    /// needed. Until a document defined that code there was nothing to read the
    /// column *for*: minting a discriminator no document defines puts a code on the
    /// wire no consumer can act on.
    ///
    /// It stays **soft**: the finding rides `warnings[]` and never blocks.
    #[must_use]
    pub const fn max_tier_bands_per_row(&self) -> u32 {
        self.max_tier_bands_per_row
    }

    /// The price-row soft cap in force for this tenant; see
    /// [`AuthoringPolicy::max_tier_bands_per_row`] for what reads it and why it
    /// never blocks.
    #[must_use]
    pub const fn max_price_rows_per_plan(&self) -> u32 {
        self.max_price_rows_per_plan
    }

    /// The descriptor keys this tenant requires beyond D-48 v1's three, in the
    /// order the report will name them.
    #[must_use]
    pub fn additional_required_descriptors(&self) -> &[String] {
        &self.additional_required_descriptors
    }

    /// The tenant's default rounding policy, when they have configured one.
    ///
    /// `None` is not "no policy applies": it is the state in which **every**
    /// published row must carry its own `rounding_policy_ref` or the publish
    /// fails with `ROUNDING_POLICY_UNRESOLVED` (§3.3, PRD §17.4). There is
    /// deliberately no deployment fallback behind it, unlike the four caps
    /// above: a cap has a ratified launch number and rounding has no safe
    /// default at all, which is the whole of why the code exists.
    #[must_use]
    pub fn default_rounding_policy_ref(&self) -> Option<&str> {
        self.default_rounding_policy_ref.as_deref()
    }

    /// C4's tax-display enforcement mode.
    ///
    /// An unreadable token is **not** a fallback to the default: the column's
    /// `CHECK` admits exactly two values, so a third is an invariant breach and
    /// resolving it to `fail_closed` would hide a corrupt row behind the safe
    /// answer. It surfaces, which is `cap_or_default`'s discipline one field
    /// over — a stored value the schema forbids is not a tenant preference.
    ///
    /// # Errors
    /// [`RepoError::CorruptRow`] when the stored token is outside the `CHECK`.
    pub fn tax_display_policy(&self) -> Result<TaxDisplayPolicy, RepoError> {
        TaxDisplayPolicy::parse(&self.tax_display_policy_mode).ok_or_else(|| {
            RepoError::CorruptRow(format!(
                "pricing_policy_object.tax_display_policy_mode `{}`",
                self.tax_display_policy_mode
            ))
        })
    }

    /// The tenant's enforced-migration notice period (D-49, M5, `inst-mg-target`).
    ///
    /// Resolved through [`NoticePeriod::resolved`], so a stored value below the
    /// 60-day floor is **raised** rather than honoured. That is not defensive
    /// duplication of `chk_pricing_policy_object_notice_floor`: the `CHECK`
    /// governs what may be *written*, and a row predating it — or one restored
    /// around it — would otherwise hand a shorter notice to the one caller whose
    /// whole job is to enforce the longer one. Unlike
    /// [`AuthoringPolicy::tax_display_policy`] this does not surface a
    /// `CorruptRow`, because unlike a token outside a `CHECK`'s enumeration an
    /// out-of-range integer has a safe reading, and it is this one.
    #[must_use]
    pub const fn migration_notice_period(&self) -> NoticePeriod {
        NoticePeriod::resolved(self.enforced_migration_notice_days)
    }
}

/// `SeaORM`-backed reader of the per-tenant policy object.
#[derive(Clone)]
pub struct PolicyObjectRepo {
    db: DBProvider<DbError>,
    /// What a tenant with no policy row is governed by. Held here rather than
    /// taken per call so that no caller can read a cap without the default
    /// behind it.
    defaults: AuthoringPolicy,
}

impl PolicyObjectRepo {
    /// Build over one database provider and the deployment's ratified defaults.
    #[must_use]
    pub fn new(db: DBProvider<DbError>, limits: &LimitsConfig) -> Self {
        Self {
            db,
            defaults: AuthoringPolicy::from_deployment_defaults(limits),
        }
    }

    /// Resolve `tenant_id`'s authoring-time caps and descriptor required-set.
    ///
    /// **A tenant with no policy row gets the ratified launch values**, and so
    /// does a tenant whose row leaves a cap null: the resolution is per column,
    /// not per row, because a tenant that configured one cap did not thereby ask
    /// for the other three to change. This is the clause that keeps D-152 from
    /// moving any of the ratified numbers.
    ///
    /// SQL-level BOLA: a foreign tenant's policy is invisible, which resolves to
    /// the deployment defaults rather than to someone else's caps — the same
    /// fail-to-absent reading every read in this module takes, and here it is
    /// the safe direction, since the alternative is enforcing one tenant's
    /// limits on another's catalog.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure;
    /// [`RepoError::CorruptRow`] when a stored cap is not a positive count the
    /// domain can count in, or `additional_required_descriptors` is not a JSON
    /// array of strings. Both are invariant breaches: the caps carry positivity
    /// `CHECK`s and the column is `NOT NULL DEFAULT '[]'`, so a row that reads
    /// otherwise was written around this gear.
    pub async fn authoring_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<AuthoringPolicy, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        self.authoring_policy_on(&conn, scope, tenant_id).await
    }

    /// The same resolution, through whichever runner the caller holds.
    ///
    /// The publish commit re-resolves the policy **inside its transaction**,
    /// where `Db::conn()` is refused by the toolkit's transaction-bypass guard —
    /// and it re-resolves it rather than carrying the pre-check's copy because
    /// the caps are part of the state §4.2's second run re-validates. An
    /// operator lowering a cap between submit and commit is exactly the moved
    /// world that clause exists to catch.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure;
    /// [`RepoError::CorruptRow`] when a stored cap is not a positive count the
    /// domain can count in, or `additional_required_descriptors` is not a JSON
    /// array of strings.
    pub async fn authoring_policy_on(
        &self,
        runner: &impl DBRunner,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<AuthoringPolicy, RepoError> {
        let row = policy_object::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(policy_object::Column::TenantId.eq(tenant_id)))
            .one(runner)
            .await
            .map_err(|e| RepoError::Db(format!("read policy object: {e}")))?;
        let Some(row) = row else {
            return Ok(self.defaults.clone());
        };
        Ok(AuthoringPolicy {
            max_tier_bands_per_row: cap(
                "pricing_policy_object.max_tier_bands_per_row",
                row.max_tier_bands_per_row,
                self.defaults.max_tier_bands_per_row,
            )?,
            max_price_rows_per_plan: cap(
                "pricing_policy_object.max_price_rows_per_plan",
                row.max_price_rows_per_plan,
                self.defaults.max_price_rows_per_plan,
            )?,
            max_custom_interval_days: cap(
                "pricing_policy_object.max_custom_interval_days",
                row.max_custom_interval_days,
                self.defaults.max_custom_interval_days,
            )?,
            max_custom_interval_months: cap(
                "pricing_policy_object.max_custom_interval_months",
                row.max_custom_interval_months,
                self.defaults.max_custom_interval_months,
            )?,
            additional_required_descriptors: read_required_keys(
                &row.additional_required_descriptors,
            )?,
            // Taken as stored, with no deployment fallback: see
            // `AuthoringPolicy::default_rounding_policy_ref`.
            default_rounding_policy_ref: row.default_rounding_policy_ref,
            tax_display_policy_mode: row.tax_display_policy_mode,
            enforced_migration_notice_days: i64::from(row.enforced_migration_notice_days),
        })
    }
}

/// Set the tenant's tax-display enforcement mode (§5's `PUT`, C4).
///
/// **Upsert, because a tenant with no policy row is the ordinary state.** C4
/// governs every tenant whether or not one has ever written a policy object, so
/// the first `PUT` has to be able to create the row — and every other column
/// then takes the schema default, which is the ratified launch value each of
/// them documents.
///
/// Answers **`false`** when the stored mode is not `expected` — a stale
/// precondition, which the surface renders `STALE_VERSION` (409) — and `true`
/// when the write landed.
///
/// # Errors
/// [`RepoError::Db`] on a scope or storage failure;
/// [`RepoError::ConcurrentMutation`] when two first writes race the same insert.
pub async fn set_tax_display_policy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    mode: TaxDisplayPolicy,
    expected: TaxDisplayPolicy,
    stamp: &AuditStamp,
) -> Result<bool, RepoError> {
    let updated = policy_object::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            policy_object::Column::TaxDisplayPolicyMode,
            Expr::value(mode.as_str()),
        )
        .col_expr(
            policy_object::Column::UpdatedAtUtc,
            Expr::value(stamp.recorded_at),
        )
        .col_expr(
            policy_object::Column::UpdatedBy,
            Expr::value(stamp.actor_principal_id),
        )
        // **The premise is in the `WHERE`, so the check and the write are one
        // statement.** The caller's `If-Match` resolved to `expected`; matching
        // on it makes this a compare-and-swap, and a concurrent writer who moved
        // the mode between the caller's read and this statement affects zero rows
        // rather than being silently overwritten. Comparing in the handler and
        // updating here unconditionally is the T-7 defect, and it was rebuilt on
        // this surface one commit after being removed from the taxonomy one.
        .filter(
            Condition::all()
                .add(policy_object::Column::TenantId.eq(tenant_id))
                .add(policy_object::Column::TaxDisplayPolicyMode.eq(expected.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| RepoError::Db(format!("update tax-display policy: {e}")))?;
    if updated.rows_affected > 0 {
        return Ok(true);
    }

    // No row matched. Either the tenant has no policy object at all — the
    // ordinary bootstrap — or one exists and its mode has moved. Only the first
    // may insert, and the difference is what separates a first write from a lost
    // update, so it is read rather than assumed.
    let exists = policy_object::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(policy_object::Column::TenantId.eq(tenant_id)))
        .one(runner)
        .await
        .map_err(|e| RepoError::Db(format!("read policy object: {e}")))?
        .is_some();
    if exists {
        return Ok(false);
    }
    // A tenant with no row is governed by `fail_closed`, so that is the only
    // premise a first write may assert.
    if expected != TaxDisplayPolicy::FailClosed {
        return Ok(false);
    }

    let row = policy_object::ActiveModel {
        tenant_id: Set(tenant_id),
        tax_display_policy_mode: Set(mode.as_str().to_owned()),
        updated_at_utc: Set(stamp.recorded_at),
        updated_by: Set(stamp.actor_principal_id),
        ..Default::default()
    };
    policy_object::Entity::insert(row.clone())
        .secure()
        .scope_with_model(scope, &row)
        .map_err(|e| RepoError::Db(format!("scope pricing_policy_object: {e}")))?
        .exec(runner)
        .await
        .map(|_| true)
        .map_err(|e| contention_or_db(&e, "pricing_policy_object", "insert policy object"))
}

/// The tenant's `default_rounding_policy_ref`, read in the caller's transaction.
///
/// A free function rather than [`PolicyObjectRepo::authoring_policy_on`] for the
/// reason `taxonomy_repo::region_readiness_map` is one: the three commit paths
/// that freeze this value hold a `txn` and no repository, and D-154's rule —
/// resolve in **this** transaction so the row and its resolution cannot disagree —
/// applies to it exactly as it does to the tax readiness beside it.
///
/// `None` covers both "no policy object" and "no default set"; the caller reads
/// them the same way, because a row with no row-level policy and a tenant with no
/// default is what `RoundingPolicyResolved` refuses at publish.
///
/// # Errors
/// [`RepoError::Db`] on a storage failure.
pub async fn default_rounding_policy_on(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
) -> Result<Option<String>, RepoError> {
    let row = policy_object::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(policy_object::Column::TenantId.eq(tenant_id)))
        .one(runner)
        .await
        .map_err(|e| RepoError::Db(format!("read policy object: {e}")))?;
    Ok(row.and_then(|row| row.default_rounding_policy_ref))
}

/// Set the tenant's **default rounding policy**, gated on the premise the caller
/// asserted (D-320).
///
/// `pricing_policy_object`'s second content writer, and the reason there is one
/// at all: with `default_rounding_policy_ref` declared and unreachable, the
/// fail-closed arm of `foundation.rounding_policy_resolved` is the only reachable
/// path, and every row of every plan has to carry its own ref or the plan does
/// not publish. This is the writer that gives PRD §17.4's per-tenant default
/// somewhere to come from.
///
/// # The premise is a value, not an enumeration
///
/// [`set_tax_display_policy`] resolves its caller's `If-Match` by **enumerating**
/// the two modes and finding the one whose tag matches. A rounding ref is free
/// text, so nothing can be enumerated; the caller's asserted tag is resolved
/// against the value the tenant currently **holds**, and that value is what goes
/// in the `WHERE`. The compare-and-swap is unchanged in strength: a concurrent
/// writer who moved the ref between the caller's read and this statement affects
/// zero rows rather than being overwritten. Comparing in the handler and
/// updating unconditionally is the T-7 defect, and it has been rebuilt on this
/// table once already.
///
/// # `None` is a value on both sides
///
/// Clearing the default back to unset is legitimate — it is the state every
/// tenant is in today — so `ref_value` may be `None`, and so may `expected`. SQL
/// equality is not null-safe, so the premise is matched with `IS NULL` on that
/// arm rather than `= NULL`, which matches nothing and would turn every clear
/// into a spurious `409`.
///
/// # Errors
/// [`RepoError::Db`] on a scope or storage failure. Answers `Ok(false)` — not
/// an error — when the premise no longer holds, which is the caller's `409`.
pub async fn set_default_rounding_policy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    ref_value: Option<&str>,
    expected: Option<&str>,
    stamp: &AuditStamp,
) -> Result<bool, RepoError> {
    let premise = match expected {
        Some(value) => policy_object::Column::DefaultRoundingPolicyRef.eq(value),
        None => policy_object::Column::DefaultRoundingPolicyRef.is_null(),
    };
    let updated = policy_object::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            policy_object::Column::DefaultRoundingPolicyRef,
            Expr::value(ref_value),
        )
        .col_expr(
            policy_object::Column::UpdatedAtUtc,
            Expr::value(stamp.recorded_at),
        )
        .col_expr(
            policy_object::Column::UpdatedBy,
            Expr::value(stamp.actor_principal_id),
        )
        .filter(
            Condition::all()
                .add(policy_object::Column::TenantId.eq(tenant_id))
                .add(premise),
        )
        .exec(runner)
        .await
        .map_err(|e| RepoError::Db(format!("update default rounding policy: {e}")))?;
    if updated.rows_affected > 0 {
        return Ok(true);
    }

    // No row matched: either the tenant has no policy object at all — the
    // ordinary bootstrap — or one exists and its ref has moved. Read rather than
    // assume, exactly as the tax-display writer does: the difference between a
    // first write and a lost update is the whole point of the precondition.
    let exists = policy_object::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(policy_object::Column::TenantId.eq(tenant_id)))
        .one(runner)
        .await
        .map_err(|e| RepoError::Db(format!("read policy object: {e}")))?
        .is_some();
    if exists {
        return Ok(false);
    }
    // A tenant with no row holds no default, so that is the only premise a first
    // write may assert.
    if expected.is_some() {
        return Ok(false);
    }

    let row = policy_object::ActiveModel {
        tenant_id: Set(tenant_id),
        default_rounding_policy_ref: Set(ref_value.map(ToOwned::to_owned)),
        updated_at_utc: Set(stamp.recorded_at),
        updated_by: Set(stamp.actor_principal_id),
        ..Default::default()
    };
    policy_object::Entity::insert(row.clone())
        .secure()
        .scope_with_model(scope, &row)
        .map_err(|e| RepoError::Db(format!("scope pricing_policy_object: {e}")))?
        .exec(runner)
        .await
        .map(|_| true)
        .map_err(|e| contention_or_db(&e, "pricing_policy_object", "insert policy object"))
}

/// Resolve one cap column against the deployment default.
///
/// A null column is the ratified value and never an error. A stored zero or a
/// negative is an **invariant breach**, not a caller mistake — the column's
/// `CHECK` refuses both — and it surfaces rather than silently falling back,
/// because falling back would make a cap nobody can satisfy indistinguishable
/// from a cap nobody configured.
fn cap(column: &str, stored: Option<i32>, default: u32) -> Result<u32, RepoError> {
    let Some(value) = stored else {
        return Ok(default);
    };
    u32::try_from(value)
        .ok()
        .filter(|count| *count > 0)
        .ok_or_else(|| RepoError::CorruptRow(format!("{column} holds {value}, not a positive cap")))
}

/// Read the descriptor required-set extension out of its JSON column.
///
/// De-duplicated and first-seen order kept, for
/// [`DescriptorSetComplete::extending_v1`]'s reason: a key named twice is a typo
/// and reporting one absence twice reads as two faults. A column that is not an
/// array of strings is an invariant breach — it is `NOT NULL DEFAULT '[]'` and
/// no CHECK looks inside a JSON document, the same weaker ground the add-on edge
/// sets rest on.
fn read_required_keys(stored: &JsonValue) -> Result<Vec<String>, RepoError> {
    let malformed = || {
        RepoError::CorruptRow(format!(
            "pricing_policy_object.additional_required_descriptors is not a JSON array of \
             strings: {stored}"
        ))
    };
    let names = stored
        .as_array()
        .ok_or_else(malformed)?
        .iter()
        .map(|value| value.as_str().map(ToOwned::to_owned).ok_or_else(malformed))
        .collect::<Result<Vec<String>, RepoError>>()?;
    // De-duplicated after the whole array has been read, not during: a filter
    // over `Result`s drops the failures as well as the duplicates, and a
    // malformed entry would then read as a tenant that configured nothing.
    let mut seen = BTreeSet::new();
    Ok(names
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .collect())
}

#[cfg(test)]
#[path = "policy_repo_tests.rs"]
mod policy_repo_tests;
