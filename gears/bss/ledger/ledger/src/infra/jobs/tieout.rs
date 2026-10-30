//! `TieOutJob` — daily self-reconciliation of the ledger.
//!
//! Recomputes `account_balance` (all-grain) from the posted journal lines and
//! compares against the cache, runs an entry-balance backstop independent of
//! the commit trigger, re-checks the no-negative guarded set, and counts
//! PENDING-mapped lines — raising `billing.ledger.invariant.alarm` on any
//! defect. System-context / cross-tenant.
//!
//! **Architecture:** the gear has no raw-SQL / unscoped / DB-side-aggregate
//! access (`DbConn` wraps a `pub(crate)` connection, `SecureSelect` exposes no
//! `GROUP BY`/`SUM`). Every aggregation here is therefore done IN MEMORY. To
//! keep peak memory bounded regardless of tenant history, the high-cardinality
//! `journal_line` / `journal_entry` sets are read in keyset pages (ordered by
//! the row's unique id, see `TIE_OUT_PAGE_SIZE`) and folded incrementally into
//! per-grain accumulators rather than materialized whole; the working set is
//! one page plus the grain-cardinality-bounded maps. The all-tenants
//! enumeration uses [`AccessScope::allow_all`] (the sanctioned all-tenants
//! system scope, same as AM's reaper/lease paths); per-tenant reads use
//! [`AccessScope::for_tenant`].
//!
//! **Scope:** tie-out is **per-tenant, all-time** — the running
//! `account_balance` cache is not period-scoped, so the recompute sums *all* of
//! a tenant's lines (paginated). Period-pruning (only re-summing
//! recently-touched periods) is a further optimization, deferred.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bss_ledger_sdk::{AccountClass, PostedMoney};
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::reconciliation::GrainAmount;
use crate::domain::status::{AR_STATUS_DISPUTED, PERIOD_STATUS_CLOSED, PERIOD_STATUS_OPEN};
use crate::infra::events::payloads::{
    AffectedItem, AlarmCategory, AlarmSeverity, LedgerInvariantAlarm,
};
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::storage::entity::verified_balance::{
    GRAIN_ACCOUNT, GRAIN_AR_INVOICE, GRAIN_AR_INVOICE_DISPUTED, GRAIN_AR_PAYER,
    GRAIN_REUSABLE_CREDIT, GRAIN_TAX, GRAIN_UNALLOCATED,
};
use crate::infra::storage::entity::{
    account_balance, ar_invoice_balance, ar_payer_balance, fiscal_period, journal_entry,
    journal_line, payment_allocation, payment_settlement, reusable_credit_subbalance,
    tax_subbalance, tenant_account, unallocated_balance,
};
use crate::infra::storage::money_text::decode_money;
use crate::infra::storage::repo::verified_balance_repo::{
    key_account, key_payer_account_ccy, key_payer_account_invoice, key_reusable, key_tax,
};
use crate::infra::storage::repo::{BaselineRow, VerifiedBalanceRepo};

/// Debit side code (matches `bss_ledger_sdk::Side::Debit`).
const SIDE_DEBIT: &str = "DR";
/// Credit side code (matches `bss_ledger_sdk::Side::Credit`).
const SIDE_CREDIT: &str = "CR";
/// PENDING mapping-status code (matches `bss_ledger_sdk::MappingStatus::Pending`).
const MAPPING_PENDING: &str = "PENDING";
/// AR account-class code (matches `bss_ledger_sdk::AccountClass::Ar`).
const CLASS_AR: &str = "AR";
/// Tax-payable account-class code (matches `bss_ledger_sdk::AccountClass::TaxPayable`).
const CLASS_TAX_PAYABLE: &str = "TAX_PAYABLE";
/// Unallocated-pool account-class code (matches `bss_ledger_sdk::AccountClass::Unallocated`).
const CLASS_UNALLOCATED: &str = "UNALLOCATED";
/// Reusable-credit account-class code (matches `bss_ledger_sdk::AccountClass::ReusableCredit`).
const CLASS_REUSABLE_CREDIT: &str = "REUSABLE_CREDIT";
/// PSP-fee-expense account-class code (matches `bss_ledger_sdk::AccountClass::PspFeeExpense`).
const CLASS_PSP_FEE_EXPENSE: &str = "PSP_FEE_EXPENSE";
/// `source_doc_type` of the settlement entry (matches `bss_ledger_sdk::SourceDocType::PaymentSettle`).
const DOC_PAYMENT_SETTLE: &str = "PAYMENT_SETTLE";
/// `source_doc_type` of a settlement-return entry (matches
/// `bss_ledger_sdk::SourceDocType::SettlementReturn`). Its presence for a tenant
/// makes the journal-recomputed `settled_minor` AND `fee_minor` unsafe (a return
/// decrements both cached counters via a `psp_return_id`-keyed entry that carries
/// no `payment_id`, so neither decrement is journal→payment recoverable) — both
/// are then skipped for that tenant (see
/// [`recompute_payment_counter_variances`]).
const DOC_SETTLEMENT_RETURN: &str = "SETTLEMENT_RETURN";

/// Cap on the per-alarm `affected` list — bounds the event size on a wide
/// defect while still naming enough grains for an operator to act.
const MAX_AFFECTED: usize = 50;

/// Page size for the full-fold `journal_line` / `journal_entry` scans. The
/// tie-out recompute is all-time per-tenant, so these tables are read in keyset
/// pages (ordered by the row's unique id) and folded incrementally rather than
/// materialized whole — peak memory is one page plus the grain-bounded
/// accumulators. Large enough to amortize round-trips, small enough to bound the
/// working set on a long-lived tenant.
const TIE_OUT_PAGE_SIZE: u64 = 5_000;

/// The `v1` alarm payload field `field` of the alarm item `item`: the grain
/// total's integer minor units under the saturating policy; `None` for an
/// untrusted or out-of-contract total.
fn grain_minor_units(
    grain: &GrainAmount,
    field: &'static str,
    item: &dyn std::fmt::Display,
) -> Option<i64> {
    grain
        .to_posted()
        .ok()
        .map(|m| crate::infra::v1_payload::v1_minor_units_for(&m, field, item))
}

/// Names one tie-out alarm item in a `v1` saturation warning; formatted only
/// when an amount saturates.
struct AlarmItem<'a> {
    id: &'a str,
    currency: &'a str,
}

impl std::fmt::Display for AlarmItem<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tie-out alarm item {} currency {}",
            self.id, self.currency
        )
    }
}

/// A cache row whose stored money the tie-out decodes: enough identity to name
/// the row when its text is corrupt.
trait CacheRow {
    /// The cache table.
    const TABLE: &'static str;
    /// The owning tenant.
    fn tenant(&self) -> Uuid;
    /// The row's stored currency code and scale.
    fn currency_meta(&self) -> (&str, i16);
    /// The row's grain key (ids only, no PII).
    fn grain(&self) -> String;
}

impl<T: CacheRow> CacheRow for &T {
    const TABLE: &'static str = T::TABLE;
    fn tenant(&self) -> Uuid {
        (**self).tenant()
    }
    fn currency_meta(&self) -> (&str, i16) {
        (**self).currency_meta()
    }
    fn grain(&self) -> String {
        (**self).grain()
    }
}

/// Implement [`CacheRow`] for a cache entity from its grain-key rendering.
macro_rules! cache_row {
    ($model:path, $table:literal, |$r:ident| $grain:expr) => {
        impl CacheRow for $model {
            const TABLE: &'static str = $table;
            fn tenant(&self) -> Uuid {
                self.tenant_id
            }
            fn currency_meta(&self) -> (&str, i16) {
                (&self.currency, self.currency_scale)
            }
            fn grain(&self) -> String {
                let $r = self;
                $grain
            }
        }
    };
}

cache_row!(account_balance::Model, "account_balance", |r| format!(
    "account={}/currency={}",
    r.account_id, r.currency
));
cache_row!(ar_payer_balance::Model, "ar_payer_balance", |r| format!(
    "payer={}/account={}/currency={}",
    r.payer_tenant_id, r.account_id, r.currency
));
cache_row!(
    ar_invoice_balance::Model,
    "ar_invoice_balance",
    |r| format!(
        "payer={}/account={}/invoice={}",
        r.payer_tenant_id, r.account_id, r.invoice_id
    )
);
cache_row!(tax_subbalance::Model, "tax_subbalance", |r| format!(
    "account={}/jurisdiction={}/filing={}",
    r.account_id, r.tax_jurisdiction, r.tax_filing_period
));
cache_row!(
    unallocated_balance::Model,
    "unallocated_balance",
    |r| format!(
        "payer={}/account={}/currency={}",
        r.payer_tenant_id, r.account_id, r.currency
    )
);
cache_row!(
    reusable_credit_subbalance::Model,
    "reusable_credit_subbalance",
    |r| format!(
        "payer={}/account={}/currency={}/event_type={}",
        r.payer_tenant_id, r.account_id, r.currency, r.credit_grant_event_type
    )
);
cache_row!(
    payment_settlement::Model,
    "payment_settlement",
    |r| format!("payment={}", r.payment_id)
);

/// Decode one stored cache amount into an exact grain total; a corrupt stored
/// value becomes an untrusted zero that always surfaces as a variance, and the
/// log names the tenant, table, column and grain so the row can be found.
fn cached_amount<R: CacheRow>(row: &R, column: &'static str, text: &str) -> GrainAmount {
    let (currency, currency_scale) = row.currency_meta();
    match decode_money(text, currency, currency_scale) {
        Ok(money) => GrainAmount::from_posted(&money),
        Err(e) => {
            tracing::error!(
                target: "bss-ledger",
                tenant_id = %row.tenant(),
                table = R::TABLE,
                column,
                grain = %row.grain(),
                currency,
                error = %e,
                "bss-ledger: tie-out: corrupt stored balance text"
            );
            GrainAmount::untrusted(currency, u8::try_from(currency_scale).unwrap_or(0))
        }
    }
}

/// A line's validated posting, or `None` for corrupt stored text (the caller
/// force-flags the grain so the corruption surfaces).
fn line_money(line: &journal_line::Model) -> Option<PostedMoney> {
    decode_money(&line.amount, &line.currency, line.currency_scale).ok()
}

/// One page of journal lines with every stored amount decoded once and shared
/// by all the accumulators (`None` marks corrupt stored text), instead of each
/// fold re-parsing the same text.
struct DecodedPage<'a> {
    lines: &'a [journal_line::Model],
    money: Vec<Option<PostedMoney>>,
}

impl<'a> DecodedPage<'a> {
    fn new(lines: &'a [journal_line::Model]) -> Self {
        Self {
            lines,
            money: lines.iter().map(line_money).collect(),
        }
    }

    /// Each line with its decoded posting.
    fn iter(&self) -> impl Iterator<Item = (&'a journal_line::Model, Option<&PostedMoney>)> {
        self.lines.iter().zip(self.money.iter().map(Option::as_ref))
    }
}

/// The additive inverse of a validated posting (always representable).
fn negated(money: &PostedMoney) -> Option<PostedMoney> {
    PostedMoney::try_new(-money.amount(), money.currency().clone()).ok()
}

/// Build one `v1` alarm item from exact totals. `None`, logged with its cause,
/// when either side is untrusted (corrupt stored text, a missing normal side, a
/// mixed scale: it has no amount to report, never a zero) or when a trusted
/// total exceeds the bounded money contract and so has no v1 representation.
fn affected_item(
    id: String,
    currency: String,
    expected: &GrainAmount,
    actual: &GrainAmount,
) -> Option<AffectedItem> {
    if expected.is_untrusted() || actual.is_untrusted() {
        tracing::error!(
            target: "bss-ledger",
            id,
            currency,
            expected_untrusted = expected.is_untrusted(),
            actual_untrusted = actual.is_untrusted(),
            "bss-ledger: tie-out: untrusted grain total carries no v1 alarm amount"
        );
        return None;
    }
    let item = AlarmItem {
        id: &id,
        currency: &currency,
    };
    if let (Some(expected_minor), Some(actual_minor)) = (
        grain_minor_units(expected, "expected_minor", &item),
        grain_minor_units(actual, "actual_minor", &item),
    ) {
        Some(AffectedItem {
            id,
            currency,
            expected_minor,
            actual_minor,
        })
    } else {
        tracing::error!(
            target: "bss-ledger",
            id,
            currency,
            expected = %expected.text(),
            actual = %actual.text(),
            "bss-ledger: tie-out: grain total exceeds the bounded money contract; \
             no v1 alarm amount"
        );
        None
    }
}

/// A recomputed `account_balance` grain that disagrees with the cache (or has
/// no cache counterpart / a stray cache row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountBalanceVariance {
    /// Account whose balance grain diverged.
    pub account_id: Uuid,
    /// Currency of the grain.
    pub currency: String,
    /// Balance recomputed from the journal lines (zero if the cache row has no
    /// computed counterpart).
    pub computed: GrainAmount,
    /// Cached `account_balance.balance` (zero if no cache row exists).
    pub cached: GrainAmount,
}

/// A recomputed sub-grain (`ar_payer_balance` / `ar_invoice_balance` /
/// `tax_subbalance`) that disagrees with its cache (or has no cache counterpart
/// / a stray cache row). The same signed-fold rule as `account_balance` drives
/// `computed`; the recompute mirrors `BalanceProjector::derive_grains`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubGrainVariance {
    /// Which sub-grain cache diverged
    /// (`"ar_payer_balance"`/`"ar_invoice_balance"`/`"tax_subbalance"`).
    pub grain: &'static str,
    /// Human-readable grain key (ids only — no PII), for the alarm diagnostic.
    pub key: String,
    /// Balance recomputed from the journal lines (zero if no computed
    /// counterpart for a stray cache row).
    pub computed: GrainAmount,
    /// Cached balance (zero if no cache row exists for a computed grain).
    pub cached: GrainAmount,
}

/// A posted entry that fails the entry-balance backstop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImbalancedEntry {
    /// The entry id.
    pub entry_id: Uuid,
    /// Currency of the imbalanced group.
    pub currency: String,
    /// Exact net (`sum(DR) - sum(CR)`); non-zero is a defect.
    pub net: GrainAmount,
    /// Number of lines in the group.
    pub line_count: u64,
    /// Distinct `payer_tenant_id` count (`> 1` is a defect).
    pub payer_count: u64,
}

/// A guarded `account_balance` grain that has gone negative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegativeGrain {
    /// Account whose balance is negative.
    pub account_id: Uuid,
    /// Currency of the grain.
    pub currency: String,
    /// The offending (negative) balance.
    pub balance: PostedMoney,
}

/// A `payment_settlement` counter that disagrees with the value recomputed from
/// the truth (`payment_allocation` rows for `allocated`; the
/// `PAYMENT_SETTLE` journal entry for `settled` / `fee`). Shares the
/// [`AlarmCategory::TieOutVariance`] alarm class with the balance variances (a
/// cache disagreeing with truth). `counter` names which counter diverged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentCounterVariance {
    /// The payment whose counter diverged (`payment_settlement.payment_id`).
    pub payment_id: String,
    /// Which counter (`"allocated"` / `"settled"` / `"fee"`).
    pub counter: &'static str,
    /// Value recomputed from the truth (allocation rows / settle journal).
    pub computed: GrainAmount,
    /// Cached `payment_settlement` counter value.
    pub cached: GrainAmount,
}

/// Per-tenant tie-out result. Clean iff every defect vec is empty AND there are
/// no PENDING-mapped lines.
pub struct TieOutReport {
    /// Tenant this report covers.
    pub tenant_id: Uuid,
    /// Count of posted `journal_line` rows the report tied out — the denominator
    /// for the Slice 7 AR↔derived rounding tolerance (X4: ≤ N minor units per
    /// 1,000 posted lines).
    pub posted_line_count: u64,
    /// `account_balance` cache divergences.
    pub account_balance_variances: Vec<AccountBalanceVariance>,
    /// Sub-grain cache divergences (`ar_payer_balance` / `ar_invoice_balance` /
    /// `tax_subbalance`).
    pub sub_grain_variances: Vec<SubGrainVariance>,
    /// Entries failing the entry-balance backstop.
    pub imbalanced_entries: Vec<ImbalancedEntry>,
    /// Guarded grains that went negative.
    pub negative_grains: Vec<NegativeGrain>,
    /// `payment_settlement` counter divergences (`allocated` / `settled` / `fee`).
    pub payment_counter_variances: Vec<PaymentCounterVariance>,
    /// Count of PENDING-mapped lines (a soft defect — blocks a clean report).
    pub pending_lines: u64,
}

impl TieOutReport {
    /// `true` when no defect of any class was found.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.account_balance_variances.is_empty()
            && self.sub_grain_variances.is_empty()
            && self.imbalanced_entries.is_empty()
            && self.negative_grains.is_empty()
            && self.payment_counter_variances.is_empty()
            && self.pending_lines == 0
    }

    /// One-line count summary (no PII — counts and the tenant id only).
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "tenant={} variances={} sub_grain_variances={} imbalanced_entries={} \
             negative_grains={} payment_counter_variances={} pending_lines={}",
            self.tenant_id,
            self.account_balance_variances.len(),
            self.sub_grain_variances.len(),
            self.imbalanced_entries.len(),
            self.negative_grains.len(),
            self.payment_counter_variances.len(),
            self.pending_lines,
        )
    }
}

/// Daily tie-out job over every tenant with posted rows.
pub struct TieOutJob {
    db: DBProvider<DbError>,
    publisher: Arc<LedgerEventPublisher>,
}

impl TieOutJob {
    /// Build the job over one database provider and the event publisher
    /// (used out-of-band to emit invariant alarms on a separate connection).
    #[must_use]
    pub fn new(db: DBProvider<DbError>, publisher: Arc<LedgerEventPublisher>) -> Self {
        Self { db, publisher }
    }

    /// Tie out a single tenant: recompute `account_balance` from the journal
    /// lines and compare to the cache, run the entry-balance backstop, re-check
    /// the no-negative guarded set, and count PENDING-mapped lines. All
    /// aggregation is in memory (see the module docs). All-time, per-tenant.
    ///
    /// # Errors
    /// Returns `Err` only on an infrastructure failure (DB unreachable / read
    /// failure); tie-out *defects* are reported in the [`TieOutReport`], not as
    /// `Err`.
    pub async fn tie_out_tenant(&self, tenant_id: Uuid) -> anyhow::Result<TieOutReport> {
        let conn = self.db.conn()?;
        self.tie_out_on(&conn, tenant_id).await
    }

    /// Tie out one tenant for a daily tick: the incremental path (baseline + open
    /// fold) when `full` is false AND the tenant has a baseline, else
    /// the full all-time fold. Both yield a [`TieOutReport`] so the alarm path is
    /// shared. The incremental report omits the full-only defect classes
    /// (imbalanced entries / negative grains / PENDING lines) — those ride the
    /// periodic full backstop.
    ///
    /// # Errors
    /// Infrastructure / read failure.
    async fn tie_out_for(&self, tenant_id: Uuid, full: bool) -> anyhow::Result<TieOutReport> {
        if !full {
            let conn = self.db.conn()?;
            if let Some(inc) = self.tie_out_incremental(&conn, tenant_id).await? {
                return Ok(inc.into_tie_out_report(tenant_id));
            }
        }
        self.tie_out_tenant(tenant_id).await
    }

    /// Tie out a single tenant against the supplied executor. The daily job
    /// passes a plain connection; period-close passes its `SERIALIZABLE`
    /// transaction so these reads join close's snapshot — a concurrent post
    /// then conflicts (SSI), forcing close to retry and re-tie-out instead of
    /// certifying a period an in-flight entry is landing in.
    ///
    /// # Errors
    /// Returns an error if any scoped ledger table cannot be read from the
    /// supplied runner.
    pub async fn tie_out_on<R: DBRunner>(
        &self,
        runner: &R,
        tenant_id: Uuid,
    ) -> anyhow::Result<TieOutReport> {
        let scope = AccessScope::for_tenant(tenant_id);

        // Per-tenant secure scoped reads of the (bounded) cache / reference tables
        // → `Vec<Model>`. The high-cardinality `journal_line` / `journal_entry`
        // sets are NOT materialized here — they are folded page-by-page below (see
        // `TIE_OUT_PAGE_SIZE`) so peak memory stays bounded regardless of tenant
        // history. Aggregation is still in memory (SecureORM exposes no DB-side
        // SUM/GROUP BY); pagination only bounds the working set.
        let accounts = tenant_account::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(tenant_account::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("tie-out: read tenant_account: {e}"))?;
        let balances = account_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(account_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("tie-out: read account_balance: {e}"))?;
        let ar_payer_cache = ar_payer_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(ar_payer_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("tie-out: read ar_payer_balance: {e}"))?;
        let ar_invoice_cache = ar_invoice_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(ar_invoice_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("tie-out: read ar_invoice_balance: {e}"))?;
        let tax_cache = tax_subbalance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(tax_subbalance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("tie-out: read tax_subbalance: {e}"))?;
        let unallocated_cache = unallocated_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(unallocated_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("tie-out: read unallocated_balance: {e}"))?;
        let reusable_credit_cache = reusable_credit_subbalance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all().add(reusable_credit_subbalance::Column::TenantId.eq(tenant_id)),
            )
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("tie-out: read reusable_credit_subbalance: {e}"))?;
        // Payment-counter cache inputs: the cached counters + the allocation rows
        // (truth for `allocated_minor`). The PAYMENT_SETTLE headers that carry
        // `source_business_id = payment_id` (and the SETTLEMENT_RETURN gate) are
        // folded from the paginated `journal_entry` scan below.
        let settlement_cache = payment_settlement::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(payment_settlement::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("tie-out: read payment_settlement: {e}"))?;
        let allocations = payment_allocation::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(payment_allocation::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("tie-out: read payment_allocation: {e}"))?;

        // account_id -> normal_side ("DR"/"CR").
        let normal_side_map: HashMap<Uuid, String> = accounts
            .iter()
            .map(|a| (a.account_id, a.normal_side.clone()))
            .collect();
        // (account_id, currency) -> cached balance row.
        let cache_map: HashMap<(Uuid, String), &account_balance::Model> = balances
            .iter()
            .map(|b| ((b.account_id, b.currency.clone()), b))
            .collect();

        // Fold `journal_entry` page-by-page into the PAYMENT_SETTLE index
        // (entry_id → payment_id) + the tenant-wide SETTLEMENT_RETURN flag. Only
        // the (settle-entry-bounded) index is retained; keyset by the unique
        // `entry_id`.
        let mut settle_payment_by_entry: HashMap<Uuid, String> = HashMap::new();
        let mut has_settlement_return = false;
        let mut entry_cursor: Option<Uuid> = None;
        loop {
            let mut cond = Condition::all().add(journal_entry::Column::TenantId.eq(tenant_id));
            if let Some(after) = entry_cursor {
                cond = cond.add(journal_entry::Column::EntryId.gt(after));
            }
            let page = journal_entry::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(cond)
                .order_by(journal_entry::Column::EntryId, sea_orm::Order::Asc)
                .limit(TIE_OUT_PAGE_SIZE)
                .all(runner)
                .await
                .map_err(|e| anyhow::anyhow!("tie-out: read journal_entry: {e}"))?;
            let Some(last) = page.last() else { break };
            entry_cursor = Some(last.entry_id);
            let fetched = u64::try_from(page.len()).unwrap_or(u64::MAX);
            let (page_index, page_has_return) = settle_index(&page);
            settle_payment_by_entry.extend(page_index);
            has_settlement_return = has_settlement_return || page_has_return;
            if fetched < TIE_OUT_PAGE_SIZE {
                break;
            }
        }

        // Fold `journal_line` page-by-page into every accumulator in a single
        // pass, keyset by the unique `line_id`. Peak memory is one page of lines
        // plus the grain-cardinality-bounded accumulator maps — never the whole
        // all-time line set.
        let mut account_balance = AccountBalanceAcc::default();
        let mut sub_grain = SubGrainAcc::default();
        let mut entry_balance = EntryBackstopAcc::default();
        let mut payment_counter = PaymentCounterAcc::default();
        let mut pending_lines: u64 = 0;
        let mut posted_line_count: u64 = 0;
        let mut line_cursor: Option<Uuid> = None;
        loop {
            let mut cond = Condition::all().add(journal_line::Column::TenantId.eq(tenant_id));
            if let Some(after) = line_cursor {
                cond = cond.add(journal_line::Column::LineId.gt(after));
            }
            let page = journal_line::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(cond)
                .order_by(journal_line::Column::LineId, sea_orm::Order::Asc)
                .limit(TIE_OUT_PAGE_SIZE)
                .all(runner)
                .await
                .map_err(|e| anyhow::anyhow!("tie-out: read journal_line: {e}"))?;
            let Some(last) = page.last() else { break };
            line_cursor = Some(last.line_id);
            let fetched = u64::try_from(page.len()).unwrap_or(u64::MAX);
            // Decode each stored amount once; all four accumulators share it.
            let decoded = DecodedPage::new(&page);
            account_balance.fold_page(&decoded, &normal_side_map);
            sub_grain.fold_page(&decoded, &normal_side_map);
            entry_balance.fold_page(&decoded);
            payment_counter.fold_page(&decoded, &settle_payment_by_entry);
            let page_pending = u64::try_from(
                page.iter()
                    .filter(|l| l.mapping_status == MAPPING_PENDING)
                    .count(),
            )
            .unwrap_or(u64::MAX);
            pending_lines = pending_lines.saturating_add(page_pending);
            posted_line_count = posted_line_count.saturating_add(fetched);
            if fetched < TIE_OUT_PAGE_SIZE {
                break;
            }
        }

        let account_balance_variances = account_balance.finalize(&cache_map, &balances);
        let sub_grain_variances = sub_grain.finalize(
            &ar_payer_cache,
            &ar_invoice_cache,
            &tax_cache,
            &unallocated_cache,
            &reusable_credit_cache,
        );
        let payment_counter_variances =
            payment_counter.finalize(&allocations, &settlement_cache, has_settlement_return);
        let imbalanced_entries = entry_balance.finalize();
        let negative_grains = negative_grains(&balances);

        Ok(TieOutReport {
            tenant_id,
            posted_line_count,
            account_balance_variances,
            sub_grain_variances,
            imbalanced_entries,
            negative_grains,
            payment_counter_variances,
            pending_lines,
        })
    }

    /// Run a tie-out pass over every tenant that has ever posted, raising one
    /// invariant alarm per defect class found.
    ///
    /// Tenants are enumerated from `journal_entry` — the source of truth — so a
    /// tenant whose balance-cache rows are missing (the exact projector failure
    /// tie-out exists to catch) is still reconciled. The set is unioned with
    /// `account_balance` to also surface an orphan cache row with no journal.
    /// Enumeration runs under the all-tenants [`AccessScope::allow_all`] system
    /// scope; per-tenant reads use [`AccessScope::for_tenant`].
    ///
    /// # Errors
    /// Returns `Err` only if the up-front tenant *enumeration* fails (DB
    /// unreachable). A per-tenant tie-out failure is logged and skipped — one
    /// flaky tenant must not starve the rest — and tie-out *defects* are
    /// reported via alarms, not as `Err`.
    pub async fn run(&self) -> anyhow::Result<()> {
        self.run_tick(true).await
    }

    /// One tie-out tick over every tenant. `full` forces the all-time fold for
    /// every tenant (the periodic drift backstop); otherwise each tenant takes the
    /// incremental path (baseline + open fold) when it has a baseline, falling
    /// back to the full fold when it does not.
    ///
    /// # Errors
    /// Returns `Err` only if the up-front tenant *enumeration* fails (DB
    /// unreachable). A per-tenant tie-out failure is logged and skipped.
    pub async fn run_tick(&self, full: bool) -> anyhow::Result<()> {
        // Cross-tenant enumeration under the all-tenants system scope. Scoped to
        // a block so the connection is released before the per-tenant loop
        // (each `tie_out_tenant` opens its own connection).
        let tenant_ids: HashSet<Uuid> = {
            let conn = self.db.conn()?;
            let entries = journal_entry::Entity::find()
                .secure()
                .scope_with(&AccessScope::allow_all())
                .all(&conn)
                .await
                .map_err(|e| anyhow::anyhow!("tie-out: enumerate tenants (journal): {e}"))?;
            let balances = account_balance::Entity::find()
                .secure()
                .scope_with(&AccessScope::allow_all())
                .all(&conn)
                .await
                .map_err(|e| anyhow::anyhow!("tie-out: enumerate tenants (cache): {e}"))?;
            entries
                .iter()
                .map(|e| e.tenant_id)
                .chain(balances.iter().map(|b| b.tenant_id))
                .collect()
        };

        let mut failed = 0_usize;
        for tenant_id in tenant_ids {
            let report = match self.tie_out_for(tenant_id, full).await {
                Ok(report) => report,
                Err(e) => {
                    // Isolate per-tenant infra failures: log and continue so a
                    // single flaky tenant doesn't abort the whole tick.
                    failed += 1;
                    tracing::error!(
                        tenant_id = %tenant_id,
                        error = %e,
                        "bss-ledger: tie-out failed for tenant; continuing"
                    );
                    continue;
                }
            };
            if report.is_clean() {
                continue;
            }

            let summary = report.summary();
            tracing::warn!(
                tenant_id = %tenant_id,
                variances = report.account_balance_variances.len(),
                sub_grain_variances = report.sub_grain_variances.len(),
                imbalanced_entries = report.imbalanced_entries.len(),
                negative_grains = report.negative_grains.len(),
                payment_counter_variances = report.payment_counter_variances.len(),
                pending_lines = report.pending_lines,
                "bss-ledger: tie-out defects detected"
            );

            // One Critical alarm per non-empty defect class, each carrying the
            // specific affected grains/entries (capped) so an operator sees what
            // diverged and by how much. PENDING lines alone get no dedicated
            // category (logged above + folded into every alarm's `detail`).
            // Sub-grain + payment-counter variances share the `TieOutVariance`
            // category with account-balance variances (each is a cache
            // disagreeing with truth).
            if !report.account_balance_variances.is_empty()
                || !report.sub_grain_variances.is_empty()
                || !report.payment_counter_variances.is_empty()
            {
                let affected = report
                    .account_balance_variances
                    .iter()
                    .filter_map(|v| {
                        affected_item(
                            v.account_id.to_string(),
                            v.currency.clone(),
                            &v.computed,
                            &v.cached,
                        )
                    })
                    .chain(report.sub_grain_variances.iter().filter_map(|v| {
                        affected_item(
                            v.key.clone(),
                            v.cached.currency().to_owned(),
                            &v.computed,
                            &v.cached,
                        )
                    }))
                    .chain(report.payment_counter_variances.iter().filter_map(|v| {
                        affected_item(
                            format!("payment={}/{}", v.payment_id, v.counter),
                            v.cached.currency().to_owned(),
                            &v.computed,
                            &v.cached,
                        )
                    }))
                    .take(MAX_AFFECTED)
                    .collect();
                self.emit(tenant_id, AlarmCategory::TieOutVariance, &summary, affected)
                    .await;
            }
            if !report.imbalanced_entries.is_empty() {
                let affected = report
                    .imbalanced_entries
                    .iter()
                    .filter_map(|ie| {
                        affected_item(
                            ie.entry_id.to_string(),
                            ie.currency.clone(),
                            &GrainAmount::zero(ie.net.currency(), ie.net.currency_scale()),
                            &ie.net,
                        )
                    })
                    .take(MAX_AFFECTED)
                    .collect();
                self.emit(tenant_id, AlarmCategory::EntryImbalance, &summary, affected)
                    .await;
            }
            if !report.negative_grains.is_empty() {
                let affected = report
                    .negative_grains
                    .iter()
                    .filter_map(|g| {
                        let balance = GrainAmount::from_posted(&g.balance);
                        affected_item(
                            g.account_id.to_string(),
                            g.currency.clone(),
                            &GrainAmount::zero(balance.currency(), balance.currency_scale()),
                            &balance,
                        )
                    })
                    .take(MAX_AFFECTED)
                    .collect();
                self.emit(
                    tenant_id,
                    AlarmCategory::NegativeBalanceViolation,
                    &summary,
                    affected,
                )
                .await;
            }
        }
        if failed > 0 {
            tracing::warn!(
                failed,
                "bss-ledger: tie-out tick completed with per-tenant failures"
            );
        }
        Ok(())
    }

    /// Emit one fire-and-forget invariant alarm for `category` against `tenant`,
    /// carrying the specific `affected` grains/entries that diverged.
    async fn emit(
        &self,
        tenant_id: Uuid,
        category: AlarmCategory,
        detail: &str,
        affected: Vec<AffectedItem>,
    ) {
        let code = category.as_str().to_owned();
        let alarm = LedgerInvariantAlarm {
            category,
            severity: AlarmSeverity::Critical,
            tenant_id,
            scope: format!("tenant:{tenant_id}"),
            code,
            detail: detail.to_owned(),
            affected,
        };
        self.publisher
            .emit_invariant_alarm(&SecurityContext::anonymous(), alarm)
            .await;
    }
}

/// A line's signed delta: `+amount` when its side equals the account's
/// `normal_side`, else `-amount` — the SAME rule the `BalanceProjector` uses for
/// every grain. `None` when the account has no `normal_side` in the map, or
/// when the stored amount text is corrupt (both defects handled by the
/// `account_balance` pass, which force-flags the grain).
fn signed(
    line: &journal_line::Model,
    money: Option<&PostedMoney>,
    normal_side_map: &HashMap<Uuid, String>,
) -> Option<PostedMoney> {
    signed_owned(line, money?.clone(), normal_side_map)
}

/// [`signed`] for a posting the caller owns: the normal-side delta is the
/// posting itself, with no clone.
fn signed_owned(
    line: &journal_line::Model,
    money: PostedMoney,
    normal_side_map: &HashMap<Uuid, String>,
) -> Option<PostedMoney> {
    let normal_side = normal_side_map.get(&line.account_id)?;
    if &line.side == normal_side {
        Some(money)
    } else {
        negated(&money)
    }
}

/// The grain total a line's currency metadata starts from.
fn line_zero(line: &journal_line::Model) -> GrainAmount {
    GrainAmount::zero(
        &line.currency,
        u8::try_from(line.currency_scale).unwrap_or(0),
    )
}

/// Fold the lines into `(account_id, currency) -> signed sum` and diff against
/// the cache. A line whose account has no `normal_side` (itself a defect — a
/// posting against an unknown chart-of-accounts row) is excluded from the sum
/// and its grain is force-flagged as a variance so it always surfaces. Also
/// flags a cache grain with no computed counterpart and vice-versa.
/// Streaming accumulator for the `account_balance` recompute. Folds journal
/// lines into `(account_id, currency) -> signed sum` page-by-page so the full
/// line set is never materialized; the retained state is bounded by the number
/// of distinct grains, not the number of lines.
#[derive(Default)]
struct AccountBalanceAcc {
    computed: HashMap<(Uuid, String), GrainAmount>,
    /// Grains whose recompute is untrustworthy (missing normal_side) — always a
    /// variance regardless of whether the cache happens to match.
    forced: HashSet<(Uuid, String)>,
}

impl AccountBalanceAcc {
    /// Fold raw lines (decoding them first); the page loop shares one decode.
    #[cfg(test)]
    fn fold(&mut self, lines: &[journal_line::Model], normal_side_map: &HashMap<Uuid, String>) {
        self.fold_page(&DecodedPage::new(lines), normal_side_map);
    }

    /// Fold one decoded page of lines into the running sums.
    fn fold_page(&mut self, page: &DecodedPage<'_>, normal_side_map: &HashMap<Uuid, String>) {
        for (line, money) in page.iter() {
            let key = (line.account_id, line.currency.clone());
            let Some(delta) = signed(line, money, normal_side_map) else {
                self.forced.insert(key);
                continue;
            };
            self.computed
                .entry(key)
                .or_insert_with(|| line_zero(line))
                .add_posted(&delta);
        }
    }

    /// Diff the folded sums against the cache. A line whose account has no
    /// `normal_side` (itself a defect — a posting against an unknown
    /// chart-of-accounts row) is excluded from the sum and its grain is
    /// force-flagged as a variance so it always surfaces. Also flags a cache
    /// grain with no computed counterpart and vice-versa.
    fn finalize(
        self,
        cache_map: &HashMap<(Uuid, String), &account_balance::Model>,
        balances: &[account_balance::Model],
    ) -> Vec<AccountBalanceVariance> {
        let Self { computed, forced } = self;
        let mut variances = Vec::new();
        let cached_of = |key: &(Uuid, String), like: &GrainAmount| {
            cache_map.get(key).map_or_else(
                || GrainAmount::zero(like.currency(), like.currency_scale()),
                |m| cached_amount(m, "balance", &m.balance),
            )
        };

        // Computed grains: compare to the cache (missing cache → zero).
        for (key, computed_sum) in &computed {
            let cached = cached_of(key, computed_sum);
            if computed_sum.differs_from(&cached) || forced.contains(key) {
                variances.push(AccountBalanceVariance {
                    account_id: key.0,
                    currency: key.1.clone(),
                    computed: computed_sum.clone(),
                    cached,
                });
            }
        }

        // Force-flagged grains that had NO surviving computed entry (every line
        // for the grain was dropped for a missing normal_side / corrupt text).
        for key in &forced {
            if !computed.contains_key(key) {
                let cached = cache_map.get(key).map_or_else(
                    || GrainAmount::untrusted(&key.1, 0),
                    |m| cached_amount(m, "balance", &m.balance),
                );
                variances.push(AccountBalanceVariance {
                    account_id: key.0,
                    currency: key.1.clone(),
                    computed: GrainAmount::untrusted(cached.currency(), cached.currency_scale()),
                    cached,
                });
            }
        }

        // Stray cache grains with no computed counterpart (computed treated as
        // zero); a corrupt stored balance always surfaces.
        for b in balances {
            let key = (b.account_id, b.currency.clone());
            if computed.contains_key(&key) || forced.contains(&key) {
                continue;
            }
            let cached = cached_amount(b, "balance", &b.balance);
            if !cached.is_zero() {
                variances.push(AccountBalanceVariance {
                    account_id: b.account_id,
                    currency: b.currency.clone(),
                    computed: GrainAmount::zero(cached.currency(), cached.currency_scale()),
                    cached,
                });
            }
        }

        variances
    }
}

/// Diff a computed `GrainKey -> signed sum` map against a cache keyed the same
/// way, emitting a [`SubGrainVariance`] for every key where the sums disagree
/// (a computed grain with no cache row → `cached = 0`; a stray non-zero cache
/// row with no computed counterpart → `computed = 0`). `grain` labels the
/// cache; `label` renders a key into the human (ids-only) diagnostic string.
fn diff_grain<K, F>(
    grain: &'static str,
    computed: &HashMap<K, GrainAmount>,
    cache: &HashMap<K, GrainAmount>,
    label: F,
) -> Vec<SubGrainVariance>
where
    K: std::hash::Hash + Eq,
    F: Fn(&K) -> String,
{
    let mut variances = Vec::new();
    // Computed grains: compare to the cache (missing cache → zero).
    for (key, computed_sum) in computed {
        let cached = cache.get(key).cloned().unwrap_or_else(|| {
            GrainAmount::zero(computed_sum.currency(), computed_sum.currency_scale())
        });
        if computed_sum.differs_from(&cached) {
            variances.push(SubGrainVariance {
                grain,
                key: label(key),
                computed: computed_sum.clone(),
                cached,
            });
        }
    }
    // Stray non-zero (or corrupt) cache grains with no computed counterpart
    // (computed = zero).
    for (key, cached) in cache {
        if !cached.is_zero() && !computed.contains_key(key) {
            variances.push(SubGrainVariance {
                grain,
                key: label(key),
                computed: GrainAmount::zero(cached.currency(), cached.currency_scale()),
                cached: cached.clone(),
            });
        }
    }
    variances
}

/// Recompute the sub-grain caches in memory from the journal lines and diff each
/// against its cache, mirroring `BalanceProjector::derive_grains`:
/// - `ar_payer_balance` `(payer, account, currency)` from `AR` lines;
/// - `ar_invoice_balance.balance` `(payer, account, invoice)` from `AR`
///   lines carrying an `invoice_id`;
/// - `ar_invoice_balance.disputed` — a SECOND `(payer, account, invoice)`
///   map summing the signed delta of ONLY the `ar_status == "DISPUTED"` AR lines
///   (mirrors `projector.rs:788`: a DISPUTED leg routes its signed amount onto
///   `disputed`; `DR +`, `CR −`);
/// - `tax_subbalance` `(account, jurisdiction, filing)` from `TAX_PAYABLE`
///   lines carrying BOTH tax dims;
/// - `unallocated_balance` `(payer, account, currency)` from `UNALLOCATED` lines;
/// - `reusable_credit_subbalance` `(payer, account, currency,
///   credit_grant_event_type)` from `REUSABLE_CREDIT` lines (a `None`
///   `credit_grant_event_type` keys as `""`, mirroring the projector's
///   `unwrap_or_default()`).
///
/// All use the same signed-delta rule as `account_balance` (see [`signed`]). A
/// line whose account has no `normal_side` is skipped here — it is force-flagged
/// by the `account_balance` pass, so double-counting it would surface a spurious
/// sub-grain variance (the projector would also have aborted the post outright,
/// so clean books never carry such a line).
/// Streaming accumulator for the projector sub-grain caches. Folds journal
/// lines into per-grain signed sums page-by-page (retained state bounded by the
/// grain cardinality, not the line count), mirroring
/// `BalanceProjector::derive_grains` — see [`SubGrainAcc::finalize`] for the
/// grain→cache mapping.
#[derive(Default)]
struct SubGrainAcc {
    // (payer_tenant_id, account_id, currency) -> signed sum.
    ar_payer: HashMap<(Uuid, Uuid, String), GrainAmount>,
    // (payer_tenant_id, account_id, invoice_id) -> signed sum (balance).
    ar_invoice: HashMap<(Uuid, Uuid, String), GrainAmount>,
    // (payer_tenant_id, account_id, invoice_id) -> signed sum of DISPUTED legs only.
    ar_invoice_disputed: HashMap<(Uuid, Uuid, String), GrainAmount>,
    // (account_id, tax_jurisdiction, tax_filing_period) -> signed sum.
    tax: HashMap<(Uuid, String, String), GrainAmount>,
    // (payer_tenant_id, account_id, currency) -> signed sum.
    unallocated: HashMap<(Uuid, Uuid, String), GrainAmount>,
    // (payer_tenant_id, account_id, currency, credit_grant_event_type) -> signed sum.
    reusable_credit: HashMap<(Uuid, Uuid, String, String), GrainAmount>,
}

impl SubGrainAcc {
    /// Fold raw lines (decoding them first); the page loop shares one decode.
    #[cfg(test)]
    fn fold(&mut self, lines: &[journal_line::Model], normal_side_map: &HashMap<Uuid, String>) {
        self.fold_page(&DecodedPage::new(lines), normal_side_map);
    }

    /// Fold one decoded page of lines into the per-grain sums.
    fn fold_page(&mut self, page: &DecodedPage<'_>, normal_side_map: &HashMap<Uuid, String>) {
        let Self {
            ar_payer,
            ar_invoice,
            ar_invoice_disputed,
            tax,
            unallocated,
            reusable_credit,
        } = self;
        for (line, money) in page.iter() {
            // Skip lines whose account lacks a normal_side (already force-flagged
            // by the account_balance pass) — never contributes to any projector
            // cache.
            let Some(delta) = signed(line, money, normal_side_map) else {
                continue;
            };

            if line.account_class == CLASS_AR {
                ar_payer
                    .entry((line.payer_tenant_id, line.account_id, line.currency.clone()))
                    .or_insert_with(|| line_zero(line))
                    .add_posted(&delta);
                if let Some(invoice_id) = &line.invoice_id {
                    let key = (line.payer_tenant_id, line.account_id, invoice_id.clone());
                    ar_invoice
                        .entry(key.clone())
                        .or_insert_with(|| line_zero(line))
                        .add_posted(&delta);
                    // DISPUTED-tagged AR lines additionally route their signed delta
                    // onto `disputed` (projector.rs:788). Untagged / ACTIVE AR
                    // lines leave it untouched.
                    if line.ar_status.as_deref() == Some(AR_STATUS_DISPUTED) {
                        ar_invoice_disputed
                            .entry(key)
                            .or_insert_with(|| line_zero(line))
                            .add_posted(&delta);
                    }
                }
            }

            if line.account_class == CLASS_TAX_PAYABLE
                && let (Some(juris), Some(filing)) =
                    (&line.tax_jurisdiction, &line.tax_filing_period)
            {
                tax.entry((line.account_id, juris.clone(), filing.clone()))
                    .or_insert_with(|| line_zero(line))
                    .add_posted(&delta);
            }

            if line.account_class == CLASS_UNALLOCATED {
                unallocated
                    .entry((line.payer_tenant_id, line.account_id, line.currency.clone()))
                    .or_insert_with(|| line_zero(line))
                    .add_posted(&delta);
            }

            if line.account_class == CLASS_REUSABLE_CREDIT {
                // The credit-grant event type sub-divides the wallet (a PK dim); a
                // missing one keys as `""` (the projector's `unwrap_or_default()`).
                let event_type = line.credit_grant_event_type.clone().unwrap_or_default();
                reusable_credit
                    .entry((
                        line.payer_tenant_id,
                        line.account_id,
                        line.currency.clone(),
                        event_type,
                    ))
                    .or_insert_with(|| line_zero(line))
                    .add_posted(&delta);
            }
        }
    }

    /// Diff the folded per-grain sums against each cache, emitting a
    /// [`SubGrainVariance`] per disagreement (see [`diff_grain`]).
    fn finalize(
        self,
        ar_payer_cache: &[ar_payer_balance::Model],
        ar_invoice_cache: &[ar_invoice_balance::Model],
        tax_cache: &[tax_subbalance::Model],
        unallocated_cache: &[unallocated_balance::Model],
        reusable_credit_cache: &[reusable_credit_subbalance::Model],
    ) -> Vec<SubGrainVariance> {
        let Self {
            ar_payer,
            ar_invoice,
            ar_invoice_disputed,
            tax,
            unallocated,
            reusable_credit,
        } = self;

        // Cache rows keyed the same way as the computed maps (decoded exactly;
        // a corrupt stored amount is an untrusted zero that always surfaces).
        let ar_payer_cache_map: HashMap<(Uuid, Uuid, String), GrainAmount> = ar_payer_cache
            .iter()
            .map(|r| {
                (
                    (r.payer_tenant_id, r.account_id, r.currency.clone()),
                    cached_amount(r, "balance", &r.balance),
                )
            })
            .collect();
        let ar_invoice_cache_map: HashMap<(Uuid, Uuid, String), GrainAmount> = ar_invoice_cache
            .iter()
            .map(|r| {
                (
                    (r.payer_tenant_id, r.account_id, r.invoice_id.clone()),
                    cached_amount(r, "balance", &r.balance),
                )
            })
            .collect();
        let ar_invoice_disputed_cache_map: HashMap<(Uuid, Uuid, String), GrainAmount> =
            ar_invoice_cache
                .iter()
                .map(|r| {
                    (
                        (r.payer_tenant_id, r.account_id, r.invoice_id.clone()),
                        cached_amount(r, "disputed", &r.disputed),
                    )
                })
                .collect();
        let tax_cache_map: HashMap<(Uuid, String, String), GrainAmount> = tax_cache
            .iter()
            .map(|r| {
                (
                    (
                        r.account_id,
                        r.tax_jurisdiction.clone(),
                        r.tax_filing_period.clone(),
                    ),
                    cached_amount(r, "balance", &r.balance),
                )
            })
            .collect();
        let unallocated_cache_map: HashMap<(Uuid, Uuid, String), GrainAmount> = unallocated_cache
            .iter()
            .map(|r| {
                (
                    (r.payer_tenant_id, r.account_id, r.currency.clone()),
                    cached_amount(r, "balance", &r.balance),
                )
            })
            .collect();
        let reusable_credit_cache_map: HashMap<(Uuid, Uuid, String, String), GrainAmount> =
            reusable_credit_cache
                .iter()
                .map(|r| {
                    (
                        (
                            r.payer_tenant_id,
                            r.account_id,
                            r.currency.clone(),
                            r.credit_grant_event_type.clone(),
                        ),
                        cached_amount(r, "balance", &r.balance),
                    )
                })
                .collect();

        let mut variances = diff_grain("ar_payer_balance", &ar_payer, &ar_payer_cache_map, |k| {
            format!("payer={}/account={}/currency={}", k.0, k.1, k.2)
        });
        variances.extend(diff_grain(
            "ar_invoice_balance",
            &ar_invoice,
            &ar_invoice_cache_map,
            |k| format!("payer={}/account={}/invoice={}", k.0, k.1, k.2),
        ));
        variances.extend(diff_grain(
            "ar_invoice_disputed",
            &ar_invoice_disputed,
            &ar_invoice_disputed_cache_map,
            |k| format!("payer={}/account={}/invoice={}", k.0, k.1, k.2),
        ));
        variances.extend(diff_grain("tax_subbalance", &tax, &tax_cache_map, |k| {
            format!("account={}/jurisdiction={}/filing={}", k.0, k.1, k.2)
        }));
        variances.extend(diff_grain(
            "unallocated_balance",
            &unallocated,
            &unallocated_cache_map,
            |k| format!("payer={}/account={}/currency={}", k.0, k.1, k.2),
        ));
        variances.extend(diff_grain(
            "reusable_credit_subbalance",
            &reusable_credit,
            &reusable_credit_cache_map,
            |k| {
                format!(
                    "payer={}/account={}/currency={}/event_type={}",
                    k.0, k.1, k.2, k.3
                )
            },
        ));
        variances
    }
}

/// Build the `entry_id -> payment_id` index for PAYMENT_SETTLE entries plus the
/// tenant-wide SETTLEMENT_RETURN flag (its presence makes journal-recomputed
/// `settled_minor` AND `fee_minor` un-reconcilable — see
/// [`DOC_SETTLEMENT_RETURN`]). Retained state is bounded by the settle-entry
/// count, so it can be folded from a paginated `journal_entry` scan.
fn settle_index(entries: &[journal_entry::Model]) -> (HashMap<Uuid, String>, bool) {
    let mut settle_payment_by_entry: HashMap<Uuid, String> = HashMap::new();
    let mut has_settlement_return = false;
    for entry in entries {
        if entry.source_doc_type == DOC_SETTLEMENT_RETURN {
            has_settlement_return = true;
        }
        if entry.source_doc_type == DOC_PAYMENT_SETTLE {
            settle_payment_by_entry.insert(entry.entry_id, entry.source_business_id.clone());
        }
    }
    (settle_payment_by_entry, has_settlement_return)
}

/// Streaming accumulator for the journal-recomputed payment counters. Folds the
/// PAYMENT_SETTLE legs page-by-page into per-payment `settled_minor` (Σ CR
/// UNALLOCATED) and `fee_minor` (Σ DR PSP_FEE_EXPENSE) sums — retained state
/// bounded by the settled-payment count, not the line count.
///
/// Reconciles the `payment_settlement` counters against the truth, per
/// `payment_id`, emitting a [`PaymentCounterVariance`] for each disagreement:
///
/// - **`allocated`** ← Σ `payment_allocation.amount` for the
///   `payment_id` (the allocation rows ARE the truth — a direct table sum, not a
///   journal recompute). Always reconciled.
/// - **`settled`** ← Σ of the `CR UNALLOCATED` line amounts on the
///   payment's `PAYMENT_SETTLE` journal entry (`= gross`, the settlement seed).
///   Reconciled ONLY when the tenant has NO `SETTLEMENT_RETURN` entry: a return
///   decrements the cached `settled_minor` via `add_settled(-amount)`, but its
///   entry is keyed by `psp_return_id` (no `payment_id` on the header or lines),
///   so the decrement is not journal→payment recoverable in memory. Skipping it
///   tenant-wide (returns are rare) avoids a guaranteed false positive on every
///   returned payment; the skip is logged once.
/// - **`fee_minor`** ← Σ of the `DR PSP_FEE_EXPENSE` line amounts on the
///   `PAYMENT_SETTLE` entry. Reconciled under the SAME `SETTLEMENT_RETURN` gate
///   as `settled_minor`: a return now reverses a proportional fee slice
///   (`add_fee(-fee_share)`, Model N D1) on the SAME `psp_return_id`-keyed entry,
///   so the decrement is equally un-mappable journal→payment in memory. With NO
///   return the fee is finalised at settle and never adjusted, so it reconciles
///   cleanly; with a return present it is skipped tenant-wide alongside
///   `settled_minor`.
/// - **`clawed_back_minor` / `refunded_minor`** — DEFERRED (see [`Self::finalize`]):
///   no clean journal→payment map exists in memory (`clawed_back_minor` comes off
///   a `CHARGEBACK` entry keyed `dispute_id:cycle:phase`, recoverable only via a
///   `ledger_dispute` join + composite-id parse + phase/variant re-derivation;
///   `refunded_minor` has no posting source in this slice — it is seeded `0` and
///   never written). Each is logged once and skipped.
///
/// The `PAYMENT_SETTLE` header carries `source_business_id = payment_id`; its
/// lines join via `entry_id`. A payment whose settle entry is missing (a counter
/// row with no journal) surfaces as `computed = 0 != cached`; a settle entry with
/// no counter row surfaces as `computed != 0, cached = 0`.
#[derive(Default)]
struct PaymentCounterAcc {
    settled_from_journal: HashMap<String, GrainAmount>,
    fee_from_journal: HashMap<String, GrainAmount>,
}

impl PaymentCounterAcc {
    /// Fold one page of lines: attribute each `CR UNALLOCATED` leg to
    /// `settled_minor` and each `DR PSP_FEE_EXPENSE` leg to `fee_minor` of the
    /// line's PAYMENT_SETTLE entry payment (via `settle_payment_by_entry`). A line
    /// on a non-settle entry is ignored — matching the by-entry grouping the
    /// former one-shot pass used.
    #[cfg(test)]
    fn fold(
        &mut self,
        lines: &[journal_line::Model],
        settle_payment_by_entry: &HashMap<Uuid, String>,
    ) {
        self.fold_page(&DecodedPage::new(lines), settle_payment_by_entry);
    }

    /// [`Self::fold`] over a page whose amounts are already decoded.
    fn fold_page(
        &mut self,
        page: &DecodedPage<'_>,
        settle_payment_by_entry: &HashMap<Uuid, String>,
    ) {
        for (line, money) in page.iter() {
            let Some(payment_id) = settle_payment_by_entry.get(&line.entry_id) else {
                continue;
            };
            let Some(money) = money else {
                // Corrupt stored text: the counter cannot be trusted; mark it so
                // the payment surfaces as a variance rather than vanish.
                self.settled_from_journal
                    .entry(payment_id.clone())
                    .or_insert_with(|| line_zero(line))
                    .mark_untrusted();
                continue;
            };
            if line.side == SIDE_CREDIT && line.account_class == CLASS_UNALLOCATED {
                self.settled_from_journal
                    .entry(payment_id.clone())
                    .or_insert_with(|| line_zero(line))
                    .add_posted(money);
            }
            if line.side == SIDE_DEBIT && line.account_class == CLASS_PSP_FEE_EXPENSE {
                self.fee_from_journal
                    .entry(payment_id.clone())
                    .or_insert_with(|| line_zero(line))
                    .add_posted(money);
            }
        }
    }

    /// Diff the folded counters against the settlement cache, per payment. See
    /// the [`PaymentCounterAcc`] docs for the per-counter rules and the
    /// SETTLEMENT_RETURN gate.
    fn finalize(
        self,
        allocations: &[payment_allocation::Model],
        settlement_cache: &[payment_settlement::Model],
        has_settlement_return: bool,
    ) -> Vec<PaymentCounterVariance> {
        let Self {
            settled_from_journal,
            fee_from_journal,
        } = self;

        // `clawed_back_minor` / `refunded_minor` reconciles are deferred — no clean
        // in-memory journal→payment map. Logged once per pass (not per payment) so
        // the tracked gap is visible without flooding the log.
        tracing::debug!(
            "bss-ledger: tie-out counter clawed_back_minor reconcile deferred \
             (no clean journal→payment map: CHARGEBACK keyed dispute_id:cycle:phase)"
        );
        tracing::debug!(
            "bss-ledger: tie-out counter refunded_minor reconcile deferred \
             (no posting source in this slice — counter seeded 0, never written)"
        );
        // A tenant with ANY SETTLEMENT_RETURN entry has un-mappable `settled_minor`
        // AND `fee_minor` decrements (Model N D1: a return reverses both on its
        // `psp_return_id`-keyed entry), so BOTH counters are skipped tenant-wide.
        if has_settlement_return {
            tracing::debug!(
                "bss-ledger: tie-out counter settled_minor + fee_minor reconcile deferred for tenant \
                 (a SETTLEMENT_RETURN decrement is not journal→payment recoverable in memory)"
            );
        }

        // `allocated` from the allocation rows (truth), per payment.
        let mut allocated_from_rows: HashMap<String, GrainAmount> = HashMap::new();
        for alloc in allocations {
            let scale = u8::try_from(alloc.currency_scale).unwrap_or(0);
            let total = allocated_from_rows
                .entry(alloc.payment_id.clone())
                .or_insert_with(|| GrainAmount::zero(&alloc.currency, scale));
            match decode_money(&alloc.amount, &alloc.currency, alloc.currency_scale) {
                Ok(money) => total.add_posted(&money),
                Err(_) => total.mark_untrusted(),
            }
        }

        // Diff each reconciled counter against the cache. The settlement cache row is
        // the per-payment anchor; allocation rows / settle-journal sums with NO cache
        // row are flagged with `cached = 0`, and a cache counter with no computed
        // counterpart is flagged with `computed = 0`.
        let mut variances = Vec::new();
        let mut seen_payments: HashSet<&str> = HashSet::new();
        for row in settlement_cache {
            seen_payments.insert(row.payment_id.as_str());
            let scale = u8::try_from(row.currency_scale).unwrap_or(0);
            let zero = || GrainAmount::zero(&row.currency, scale);

            let allocated = allocated_from_rows
                .get(&row.payment_id)
                .cloned()
                .unwrap_or_else(zero);
            let cached_allocated = cached_amount(row, "allocated", &row.allocated);
            if allocated.differs_from(&cached_allocated) {
                variances.push(PaymentCounterVariance {
                    payment_id: row.payment_id.clone(),
                    counter: "allocated",
                    computed: allocated,
                    cached: cached_allocated,
                });
            }

            // `settled` + `fee` share the SETTLEMENT_RETURN gate: a return
            // decrements both on its un-mappable `psp_return_id`-keyed entry (Model N
            // D1), so neither is journal→payment recoverable in memory once any return
            // exists for the tenant.
            if !has_settlement_return {
                let fee = fee_from_journal
                    .get(&row.payment_id)
                    .cloned()
                    .unwrap_or_else(zero);
                let cached_fee = cached_amount(row, "fee", &row.fee);
                if fee.differs_from(&cached_fee) {
                    variances.push(PaymentCounterVariance {
                        payment_id: row.payment_id.clone(),
                        counter: "fee",
                        computed: fee,
                        cached: cached_fee,
                    });
                }

                let settled = settled_from_journal
                    .get(&row.payment_id)
                    .cloned()
                    .unwrap_or_else(zero);
                let cached_settled = cached_amount(row, "settled", &row.settled);
                if settled.differs_from(&cached_settled) {
                    variances.push(PaymentCounterVariance {
                        payment_id: row.payment_id.clone(),
                        counter: "settled",
                        computed: settled,
                        cached: cached_settled,
                    });
                }
            }
        }

        // Computed-but-uncached payments (a settle journal / allocation rows with NO
        // `payment_settlement` counter row — the seed that should anchor them is
        // missing). Mirror `diff_grain`'s stray-computed rule: flag each non-zero
        // computed counter with `cached = 0`. `settled` stays gated on the
        // no-return guard.
        let mut orphans: HashSet<&str> = HashSet::new();
        orphans.extend(allocated_from_rows.keys().map(String::as_str));
        orphans.extend(settled_from_journal.keys().map(String::as_str));
        orphans.extend(fee_from_journal.keys().map(String::as_str));
        for payment_id in orphans {
            if seen_payments.contains(payment_id) {
                continue;
            }
            if let Some(allocated) = allocated_from_rows.get(payment_id)
                && !allocated.is_zero()
            {
                variances.push(PaymentCounterVariance {
                    payment_id: payment_id.to_owned(),
                    counter: "allocated",
                    computed: allocated.clone(),
                    cached: GrainAmount::zero(allocated.currency(), allocated.currency_scale()),
                });
            }
            // `fee` + `settled` share the SETTLEMENT_RETURN gate (see the
            // main loop): both carry un-mappable return decrements (Model N D1).
            if !has_settlement_return {
                if let Some(fee) = fee_from_journal.get(payment_id)
                    && !fee.is_zero()
                {
                    variances.push(PaymentCounterVariance {
                        payment_id: payment_id.to_owned(),
                        counter: "fee",
                        computed: fee.clone(),
                        cached: GrainAmount::zero(fee.currency(), fee.currency_scale()),
                    });
                }
                if let Some(settled) = settled_from_journal.get(payment_id)
                    && !settled.is_zero()
                {
                    variances.push(PaymentCounterVariance {
                        payment_id: payment_id.to_owned(),
                        counter: "settled",
                        computed: settled.clone(),
                        cached: GrainAmount::zero(settled.currency(), settled.currency_scale()),
                    });
                }
            }
        }

        variances
    }
}

/// Per-entry running tally for the entry-balance backstop.
#[derive(Default)]
struct EntryAgg {
    net: Option<GrainAmount>,
    line_count: u64,
    payers: HashSet<Uuid>,
}

/// Group lines by `(entry_id, currency, currency_scale)` and flag any group
/// whose net (`sum(DR) - sum(CR)`) is non-zero, that has no lines, or that
/// spans more than one payer. Independent of the commit trigger — catches a
/// malformed entry committed with a missing/buggy trigger.
/// Streaming accumulator for the entry-balance backstop. Folds lines into
/// per-`(entry_id, currency, currency_scale)` tallies page-by-page (retained
/// state bounded by the entry-grain count, not the line count).
#[derive(Default)]
struct EntryBackstopAcc {
    groups: HashMap<(Uuid, String, i16), EntryAgg>,
}

impl EntryBackstopAcc {
    /// Fold raw lines (decoding them first); the page loop shares one decode.
    #[cfg(test)]
    fn fold(&mut self, lines: &[journal_line::Model]) {
        self.fold_page(&DecodedPage::new(lines));
    }

    /// Fold one decoded page of lines into the per-entry tallies.
    fn fold_page(&mut self, page: &DecodedPage<'_>) {
        for (line, money) in page.iter() {
            let key = (line.entry_id, line.currency.clone(), line.currency_scale);
            let agg = self.groups.entry(key).or_default();
            let net = agg.net.get_or_insert_with(|| line_zero(line));
            match money.and_then(|money| {
                if line.side == SIDE_DEBIT {
                    Some(money.clone())
                } else {
                    negated(money)
                }
            }) {
                Some(signed) => net.add_posted(&signed),
                None => net.mark_untrusted(),
            }
            agg.line_count += 1;
            agg.payers.insert(line.payer_tenant_id);
        }
    }

    /// Flag any group whose net (`sum(DR) - sum(CR)`) is non-zero or that spans
    /// more than one payer.
    fn finalize(self) -> Vec<ImbalancedEntry> {
        let mut imbalanced = Vec::new();
        for ((entry_id, currency, scale), agg) in self.groups {
            let payer_count = u64::try_from(agg.payers.len()).unwrap_or(u64::MAX);
            let net = agg
                .net
                .unwrap_or_else(|| GrainAmount::zero(&currency, u8::try_from(scale).unwrap_or(0)));
            if !net.is_zero() || payer_count > 1 {
                imbalanced.push(ImbalancedEntry {
                    entry_id,
                    currency,
                    net,
                    line_count: agg.line_count,
                    payer_count,
                });
            }
        }
        imbalanced
    }
}

/// Re-check the no-negative invariant: an `account_balance` row whose balance is
/// negative is a defect when its class is guarded ([`AccountClass::GUARDED`]) OR
/// unknown/unparseable (fail loud on a corrupt class). Classes outside the
/// guarded set may legitimately go negative.
fn negative_grains(balances: &[account_balance::Model]) -> Vec<NegativeGrain> {
    balances
        .iter()
        .filter(|b| {
            b.account_class
                .parse::<AccountClass>()
                .map_or(true, AccountClass::is_guarded)
        })
        .filter_map(|b| {
            // A corrupt stored balance is surfaced by the cache variance pass.
            let balance = decode_money(&b.balance, &b.currency, b.currency_scale).ok()?;
            balance.amount().is_sign_negative().then(|| NegativeGrain {
                account_id: b.account_id,
                currency: b.currency.clone(),
                balance,
            })
        })
        .collect()
}

// ────────────────────────────────────────────────────────────────────────────
// Incremental tie-out: (grain, grain_key) string-space projection.
//
// The full `tie_out_on` folds ALL of a tenant's lines (O(history)). The
// incremental path verifies `baseline + fold(open periods) == cache` instead:
// the baseline is the cumulative VERIFIED balance through the last closed
// period (snapshotted at close, when a clean full tie-out has just proven the
// cache), and the open fold covers only the OPEN-period lines. Both the cache
// projection and the line fold land in the SAME `(grain, grain_key)` string
// space the baseline is stored in, so the three compare like-for-like.
// ────────────────────────────────────────────────────────────────────────────

/// Project the derived caches into `(grain, grain_key) -> balance` — the shared
/// representation the baseline is stored in and the open fold is compared
/// against. Mirrors the cache-map building in `recompute_sub_grain_variances`.
fn cache_grains(
    balances: &[account_balance::Model],
    ar_payer: &[ar_payer_balance::Model],
    ar_invoice: &[ar_invoice_balance::Model],
    tax: &[tax_subbalance::Model],
    unallocated: &[unallocated_balance::Model],
    reusable_credit: &[reusable_credit_subbalance::Model],
) -> HashMap<(&'static str, String), GrainAmount> {
    let mut m: HashMap<(&'static str, String), GrainAmount> = HashMap::new();
    for b in balances {
        m.insert(
            (GRAIN_ACCOUNT, key_account(b.account_id, &b.currency)),
            cached_amount(b, "balance", &b.balance),
        );
    }
    for r in ar_payer {
        m.insert(
            (
                GRAIN_AR_PAYER,
                key_payer_account_ccy(r.payer_tenant_id, r.account_id, &r.currency),
            ),
            cached_amount(r, "balance", &r.balance),
        );
    }
    for r in ar_invoice {
        let key = key_payer_account_invoice(r.payer_tenant_id, r.account_id, &r.invoice_id);
        m.insert(
            (GRAIN_AR_INVOICE, key.clone()),
            cached_amount(r, "balance", &r.balance),
        );
        m.insert(
            (GRAIN_AR_INVOICE_DISPUTED, key),
            cached_amount(r, "disputed", &r.disputed),
        );
    }
    for r in tax {
        m.insert(
            (
                GRAIN_TAX,
                key_tax(r.account_id, &r.tax_jurisdiction, &r.tax_filing_period),
            ),
            cached_amount(r, "balance", &r.balance),
        );
    }
    for r in unallocated {
        m.insert(
            (
                GRAIN_UNALLOCATED,
                key_payer_account_ccy(r.payer_tenant_id, r.account_id, &r.currency),
            ),
            cached_amount(r, "balance", &r.balance),
        );
    }
    for r in reusable_credit {
        m.insert(
            (
                GRAIN_REUSABLE_CREDIT,
                key_reusable(
                    r.payer_tenant_id,
                    r.account_id,
                    &r.currency,
                    &r.credit_grant_event_type,
                ),
            ),
            cached_amount(r, "balance", &r.balance),
        );
    }
    m
}

/// Convert a cache projection into baseline rows to snapshot (absolute totals).
/// Used at period close to persist the freshly-verified cache as the baseline.
///
/// # Errors
/// A corrupt or out-of-contract cached total cannot be snapshotted as verified.
fn cache_baseline_rows(
    cache: &HashMap<(&'static str, String), GrainAmount>,
) -> anyhow::Result<Vec<BaselineRow>> {
    cache
        .iter()
        .map(|((grain, key), balance)| {
            Ok(BaselineRow {
                grain: (*grain).to_owned(),
                grain_key: key.clone(),
                balance: balance
                    .to_posted()
                    .map_err(|e| anyhow::anyhow!("baseline grain {grain}/{key}: {e}"))?,
            })
        })
        .collect()
}

/// Fold journal lines into `(grain, grain_key) -> signed sum`, using the SAME
/// signed-delta rule and per-class grain derivation as the full fold (see
/// [`signed`] and `recompute_sub_grain_variances`). A line whose account lacks a
/// `normal_side` is skipped (the full backstop force-flags it); incremental is a
/// cache-tie-out, not the defect scanner.
fn fold_grains(
    lines: &[journal_line::Model],
    normal_side_map: &HashMap<Uuid, String>,
) -> HashMap<(&'static str, String), GrainAmount> {
    let mut m: HashMap<(&'static str, String), GrainAmount> = HashMap::new();
    // Decoded inline, one line at a time: this fold is the only consumer of the
    // open-period lines, so a `DecodedPage` would only keep every decoded
    // posting alive at once (it pays off where several accumulators share a page).
    for line in lines {
        let Some(delta) =
            line_money(line).and_then(|money| signed_owned(line, money, normal_side_map))
        else {
            continue;
        };
        let mut bump = |key: (&'static str, String)| {
            m.entry(key)
                .or_insert_with(|| line_zero(line))
                .add_posted(&delta);
        };
        // account_balance — every line.
        bump((GRAIN_ACCOUNT, key_account(line.account_id, &line.currency)));

        if line.account_class == CLASS_AR {
            bump((
                GRAIN_AR_PAYER,
                key_payer_account_ccy(line.payer_tenant_id, line.account_id, &line.currency),
            ));
            if let Some(invoice_id) = &line.invoice_id {
                let key =
                    key_payer_account_invoice(line.payer_tenant_id, line.account_id, invoice_id);
                bump((GRAIN_AR_INVOICE, key.clone()));
                if line.ar_status.as_deref() == Some(AR_STATUS_DISPUTED) {
                    bump((GRAIN_AR_INVOICE_DISPUTED, key));
                }
            }
        }

        if line.account_class == CLASS_TAX_PAYABLE
            && let (Some(juris), Some(filing)) = (&line.tax_jurisdiction, &line.tax_filing_period)
        {
            bump((GRAIN_TAX, key_tax(line.account_id, juris, filing)));
        }

        if line.account_class == CLASS_UNALLOCATED {
            bump((
                GRAIN_UNALLOCATED,
                key_payer_account_ccy(line.payer_tenant_id, line.account_id, &line.currency),
            ));
        }

        if line.account_class == CLASS_REUSABLE_CREDIT {
            let event_type = line.credit_grant_event_type.clone().unwrap_or_default();
            bump((
                GRAIN_REUSABLE_CREDIT,
                key_reusable(
                    line.payer_tenant_id,
                    line.account_id,
                    &line.currency,
                    &event_type,
                ),
            ));
        }
    }
    m
}

/// Verify `baseline + open_fold == cache` per grain, emitting a
/// [`SubGrainVariance`] for every mismatch (over the union of all keys, so a
/// stray cache row, a missing baseline grain, or an open-only grain all
/// surface). A clean ledger yields an empty vec.
fn verify_incremental(
    baseline: &HashMap<(&'static str, String), GrainAmount>,
    open_fold: &HashMap<(&'static str, String), GrainAmount>,
    cache: &HashMap<(&'static str, String), GrainAmount>,
) -> Vec<SubGrainVariance> {
    let mut keys: HashSet<&(&'static str, String)> = HashSet::new();
    keys.extend(baseline.keys());
    keys.extend(open_fold.keys());
    keys.extend(cache.keys());

    let mut variances = Vec::new();
    for key in keys {
        // The currency metadata of whichever side exists seeds the zeros, so an
        // absent side compares as an exact zero of the same grain.
        let like = baseline
            .get(key)
            .or_else(|| open_fold.get(key))
            .or_else(|| cache.get(key))
            .map_or_else(|| GrainAmount::zero("", 0), Clone::clone);
        let mut computed = baseline
            .get(key)
            .cloned()
            .unwrap_or_else(|| GrainAmount::zero(like.currency(), like.currency_scale()));
        if let Some(open) = open_fold.get(key) {
            computed.add_grain(open);
        }
        let cached = cache
            .get(key)
            .cloned()
            .unwrap_or_else(|| GrainAmount::zero(like.currency(), like.currency_scale()));
        if computed.differs_from(&cached) {
            variances.push(SubGrainVariance {
                grain: key.0,
                key: key.1.clone(),
                computed,
                cached,
            });
        }
    }
    variances
}

/// The result of an incremental tie-out: the cache-vs-(baseline+open)
/// disagreements, plus how many open-period lines were folded (diagnostics).
#[derive(Clone, Debug, Default)]
pub struct IncrementalReport {
    /// Per-grain disagreements (`computed = baseline + open_fold`, `cached`).
    pub sub_grain_variances: Vec<SubGrainVariance>,
    /// Number of open-period lines folded (the bounded cost vs all-time).
    pub open_line_count: u64,
    /// Max `created_seq` the baseline is verified through (the incremental
    /// boundary) — advanced onto `reconciliation_run.watermark` by the recon tick.
    pub watermark: Option<i64>,
}

impl IncrementalReport {
    /// `true` when no grain diverged.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.sub_grain_variances.is_empty()
    }

    /// Adapt to a [`TieOutReport`] so the shared tolerance/alarm paths consume it
    /// unchanged: the per-grain disagreements land in `sub_grain_variances` (the
    /// account grain included, keyed by its `GRAIN_ACCOUNT` discriminator) and
    /// `posted_line_count` carries the open-line count. The full-fold-only defect
    /// classes (imbalanced entries, negative grains, PENDING lines) are empty —
    /// incremental is a cache tie-out, not the defect scanner (the periodic full
    /// backstop and the posting-time guards cover those).
    #[must_use]
    pub fn into_tie_out_report(self, tenant_id: Uuid) -> TieOutReport {
        TieOutReport {
            tenant_id,
            posted_line_count: self.open_line_count,
            account_balance_variances: Vec::new(),
            sub_grain_variances: self.sub_grain_variances,
            imbalanced_entries: Vec::new(),
            negative_grains: Vec::new(),
            payment_counter_variances: Vec::new(),
            pending_lines: 0,
        }
    }
}

impl TieOutJob {
    /// Incremental tie-out: verify `baseline + fold(open periods) ==
    /// cache`, bounding the daily/recon cost to the open period instead of
    /// folding all of history. Returns `Ok(None)` — signalling the caller to run
    /// the full [`tie_out_on`](Self::tie_out_on) — when the tenant has no stored
    /// baseline yet (never closed a period) OR carries a period in a transitional
    /// state (anything other than `OPEN`/`CLOSED`, e.g. a `REOPENED` period whose
    /// once-closed contribution the cumulative baseline can no longer isolate; the
    /// next clean close re-snapshots a fresh baseline and incremental resumes).
    ///
    /// # Errors
    /// Returns `Err` only on an infrastructure failure (DB unreachable / read
    /// failure); tie-out *defects* are reported in the [`IncrementalReport`].
    pub async fn tie_out_incremental<R: DBRunner>(
        &self,
        runner: &R,
        tenant_id: Uuid,
    ) -> anyhow::Result<Option<IncrementalReport>> {
        let scope = AccessScope::for_tenant(tenant_id);

        // 1. Baseline. Empty → never closed a period → full fold.
        let baseline_rows = VerifiedBalanceRepo::new(self.db.clone())
            .load_baseline(runner, &scope, tenant_id)
            .await
            .map_err(|e| anyhow::anyhow!("incremental tie-out: load baseline: {e}"))?;
        if baseline_rows.is_empty() {
            return Ok(None);
        }

        // 2. Period partition. A non-OPEN/CLOSED status (transitional / REOPENED)
        //    means the cumulative baseline may no longer isolate the closed
        //    contribution — fall back to the full fold until the next close.
        let periods = fiscal_period::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(fiscal_period::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("incremental tie-out: read fiscal_period: {e}"))?;
        let mut open_period_ids: Vec<String> = Vec::new();
        for p in &periods {
            match p.status.as_str() {
                PERIOD_STATUS_OPEN => open_period_ids.push(p.period_id.clone()),
                PERIOD_STATUS_CLOSED => {}
                _ => return Ok(None),
            }
        }

        // 3. Open-period lines only (the bounded read) + the chart for normal_side.
        let open_lines = if open_period_ids.is_empty() {
            Vec::new()
        } else {
            journal_line::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(
                    Condition::all()
                        .add(journal_line::Column::TenantId.eq(tenant_id))
                        .add(journal_line::Column::PeriodId.is_in(open_period_ids)),
                )
                .all(runner)
                .await
                .map_err(|e| anyhow::anyhow!("incremental tie-out: read open journal_line: {e}"))?
        };
        let accounts = tenant_account::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(tenant_account::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("incremental tie-out: read tenant_account: {e}"))?;

        // 4. The caches (all-time totals — the verify target).
        let balances = account_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(account_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("incremental tie-out: read account_balance: {e}"))?;
        let ar_payer_cache = ar_payer_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(ar_payer_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("incremental tie-out: read ar_payer_balance: {e}"))?;
        let ar_invoice_cache = ar_invoice_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(ar_invoice_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("incremental tie-out: read ar_invoice_balance: {e}"))?;
        let tax_cache = tax_subbalance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(tax_subbalance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("incremental tie-out: read tax_subbalance: {e}"))?;
        let unallocated_cache = unallocated_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(unallocated_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("incremental tie-out: read unallocated_balance: {e}"))?;
        let reusable_credit_cache = reusable_credit_subbalance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all().add(reusable_credit_subbalance::Column::TenantId.eq(tenant_id)),
            )
            .all(runner)
            .await
            .map_err(|e| {
                anyhow::anyhow!("incremental tie-out: read reusable_credit_subbalance: {e}")
            })?;

        // 5. Build the three maps and verify.
        let normal_side_map: HashMap<Uuid, String> = accounts
            .iter()
            .map(|a| (a.account_id, a.normal_side.clone()))
            .collect();
        let baseline_map: HashMap<(&'static str, String), GrainAmount> = baseline_rows
            .iter()
            .filter_map(|r| {
                grain_label(&r.grain).map(|g| {
                    (
                        (g, r.grain_key.clone()),
                        GrainAmount::from_posted(&r.balance),
                    )
                })
            })
            .collect();
        let open_fold = fold_grains(&open_lines, &normal_side_map);
        let cache_map = cache_grains(
            &balances,
            &ar_payer_cache,
            &ar_invoice_cache,
            &tax_cache,
            &unallocated_cache,
            &reusable_credit_cache,
        );

        Ok(Some(IncrementalReport {
            sub_grain_variances: verify_incremental(&baseline_map, &open_fold, &cache_map),
            open_line_count: u64::try_from(open_lines.len()).unwrap_or(u64::MAX),
            watermark: baseline_rows.iter().map(|r| r.watermark_seq).max(),
        }))
    }

    /// Snapshot the current caches as the cumulative VERIFIED baseline for
    /// `tenant_id` through `through_period`, in the caller's (close) transaction.
    /// Called right after a clean full tie-out in the period-close SERIALIZABLE
    /// txn — the caches are proven there, so they ARE the verified cumulative
    /// total through the closing period; the snapshot rolls back with the close
    /// txn on abort. `watermark_seq` is the max `created_seq` among the closing
    /// period's entries (the incremental boundary recorded for diagnostics).
    ///
    /// # Errors
    /// Returns `Err` on an infrastructure / storage failure.
    pub async fn snapshot_baseline<R: DBRunner>(
        &self,
        runner: &R,
        tenant_id: Uuid,
        through_period: &str,
    ) -> anyhow::Result<()> {
        let scope = AccessScope::for_tenant(tenant_id);
        let balances = account_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(account_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("snapshot baseline: read account_balance: {e}"))?;
        let ar_payer_cache = ar_payer_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(ar_payer_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("snapshot baseline: read ar_payer_balance: {e}"))?;
        let ar_invoice_cache = ar_invoice_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(ar_invoice_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("snapshot baseline: read ar_invoice_balance: {e}"))?;
        let tax_cache = tax_subbalance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(tax_subbalance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("snapshot baseline: read tax_subbalance: {e}"))?;
        let unallocated_cache = unallocated_balance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(unallocated_balance::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("snapshot baseline: read unallocated_balance: {e}"))?;
        let reusable_credit_cache = reusable_credit_subbalance::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all().add(reusable_credit_subbalance::Column::TenantId.eq(tenant_id)),
            )
            .all(runner)
            .await
            .map_err(|e| {
                anyhow::anyhow!("snapshot baseline: read reusable_credit_subbalance: {e}")
            })?;

        let cache_map = cache_grains(
            &balances,
            &ar_payer_cache,
            &ar_invoice_cache,
            &tax_cache,
            &unallocated_cache,
            &reusable_credit_cache,
        );
        let rows = cache_baseline_rows(&cache_map)?;

        // Watermark = max created_seq among the closing period's entries.
        let entries = journal_entry::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::TenantId.eq(tenant_id))
                    .add(journal_entry::Column::PeriodId.eq(through_period)),
            )
            .all(runner)
            .await
            .map_err(|e| anyhow::anyhow!("snapshot baseline: read journal_entry: {e}"))?;
        let watermark_seq = entries.iter().map(|e| e.created_seq).max().unwrap_or(0);

        VerifiedBalanceRepo::new(self.db.clone())
            .snapshot(
                runner,
                &scope,
                tenant_id,
                through_period,
                watermark_seq,
                &rows,
            )
            .await
            .map_err(|e| anyhow::anyhow!("snapshot baseline: persist: {e}"))?;
        Ok(())
    }
}

/// Map a stored grain discriminator string back to its `'static` label so the
/// baseline keys live in the same space as the cache/fold maps. Returns `None`
/// for an unknown discriminator (a corrupt row — excluded so it surfaces as a
/// missing-baseline variance rather than panicking).
fn grain_label(grain: &str) -> Option<&'static str> {
    match grain {
        GRAIN_ACCOUNT => Some(GRAIN_ACCOUNT),
        GRAIN_AR_PAYER => Some(GRAIN_AR_PAYER),
        GRAIN_AR_INVOICE => Some(GRAIN_AR_INVOICE),
        GRAIN_AR_INVOICE_DISPUTED => Some(GRAIN_AR_INVOICE_DISPUTED),
        GRAIN_TAX => Some(GRAIN_TAX),
        GRAIN_UNALLOCATED => Some(GRAIN_UNALLOCATED),
        GRAIN_REUSABLE_CREDIT => Some(GRAIN_REUSABLE_CREDIT),
        _ => None,
    }
}

#[cfg(test)]
#[path = "tieout_tests.rs"]
mod tests;
