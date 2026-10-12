//! `JournalRepo` — append-only insert of an entry + its lines inside a
//! single passed-in transaction, plus a scoped read-back. Tenant
//! isolation runs through the `SecureORM` layer (`secure_insert` /
//! `.secure().scope_with(scope)`); P1 reads take an explicit
//! `AccessScope` that later phases populate from the PDP.

use bss_ledger_sdk::{
    AccountClass, ArInvoiceBalanceView, BalanceView, CurrencySpec, MappingStatus, MoneyError,
    PostedMoney, Side, SourceDocType,
};
use rust_decimal::Decimal;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, Order};
use toolkit_db::odata::sea_orm_filter::{LimitCfg, paginate_odata};
use toolkit_db::secure::{AccessScope, DBRunner, DbTx, SecureEntityExt, secure_insert};
use toolkit_db::{DBProvider, DbError};
use toolkit_odata::filter::FilterField;
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;

use crate::domain::exact_money::{ExactAmount, ExactError};
use crate::domain::model::{
    EntryKey, EntryRecord, EntryRef, LineRecord, NewEntry, NewLine, RepoError,
};
use crate::infra::storage::entity::{
    account_balance, ar_invoice_balance, journal_entry, journal_line,
};
use crate::infra::storage::money_text::{decode_money, decode_optional_money, encode_amount};
use crate::infra::storage::odata_mapping::{
    BalanceODataMapper, JournalEntryODataMapper, JournalLineODataMapper,
};
use crate::odata::{BalanceFilterField, JournalEntryFilterField, JournalLineFilterField};

// Settlement journal markers for the period-scoped PSP-settled fold (C2) — mirror
// `bss_ledger_sdk::{SourceDocType, AccountClass, Side}` as stored on the rows.
const SETTLE_DOC: &str = "PAYMENT_SETTLE";
const RETURN_DOC: &str = "SETTLEMENT_RETURN";
const UNALLOCATED_CLASS: &str = "UNALLOCATED";
const CREDIT_SIDE: &str = "CR";
const DEBIT_SIDE: &str = "DR";

/// Per-endpoint pagination bounds for `GET /bss-ledger/v1/journal-lines`
/// (preserves the foundation's `DEFAULT_LINE_LIMIT` / `MAX_LINE_LIMIT`).
const LINE_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 200,
};

/// Per-endpoint pagination bounds for `GET /bss-ledger/v1/journal-entries` (the
/// entry-HEADER list, R5). One header per posted entry, so the same generous
/// default as the line list keeps the common single-page read one round-trip.
const ENTRY_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 200,
};

/// Per-endpoint pagination bounds for `GET /bss-ledger/v1/balances`. The chart
/// of accounts is small per tenant; a generous default keeps the common
/// single-page read one round-trip.
const BALANCE_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 200,
};

/// Inject a default keyset order on a bare list (no `$orderby`, no cursor) so
/// `paginate_odata` resolves a stable order. `field` is the entity's default
/// keyset column (a recognised `FilterField` variant); the `$filter` is left
/// intact because `paginate_odata` applies it itself (the ledger pre-applies
/// only the `tenant_id` predicate to the secured select, not the user filter).
/// Shared with [`super::reference_repo`]'s `list_accounts`.
pub(crate) fn query_with_default_order(query: &ODataQuery, field: &str) -> ODataQuery {
    let mut out = query.clone();
    if out.cursor.is_none() && out.order.is_empty() {
        out = out.with_order(ODataOrderBy(vec![OrderKey {
            field: field.to_owned(),
            dir: SortDir::Asc,
        }]));
    }
    out
}

/// Append the key halves a walk needs to land on a unique row.
///
/// `paginate_odata` appends exactly one tiebreaker, which is enough only where
/// that tiebreaker is the table's whole primary key. On a composite key it is
/// not: a caller's `$orderby` leaves the effective order `[field, tiebreaker]`,
/// and two rows sharing that pair collide in the keyset predicate, so one of
/// them is dropped at the page boundary and never appears on any page.
/// Pricing's `query_with_unique_order`, for the same reason.
///
/// The extractor refuses `$orderby` beside a cursor, so page 2 rebuilds its
/// order from `cursor.s` — which already carries this suffix, and
/// [`ODataOrderBy::ensure_tiebreaker`] skips a field already present, so
/// applying this after [`query_with_default_order`] is idempotent.
pub(crate) fn query_with_unique_order<F: FilterField>(
    query: &ODataQuery,
    suffix: &[F],
) -> ODataQuery {
    let mut order = query.order.clone();
    for field in suffix {
        order = order.ensure_tiebreaker(field.name(), SortDir::Asc);
    }
    query.clone().with_order(order)
}

/// Error of an `OData`-paginated list read. The two arms keep the caller-facing
/// `$filter` / cursor failures (a client error ⇒ canonical 400) separable from
/// a genuine storage fault (⇒ 500): the local client maps `Odata` through the
/// platform `CanonicalError::from(toolkit_odata::Error)` projection and `Db`
/// through an `Internal`. Mirrors RBAC's `map_odata_err_to_domain` split.
#[derive(Debug, thiserror::Error)]
pub enum OdataPageError {
    /// Storage / connection failure (driver text bounded by the caller).
    #[error("ledger list db error: {0}")]
    Db(String),
    /// Malformed `$filter` / `$orderby` / cursor — a client error.
    #[error("ledger list odata error: {0}")]
    Odata(#[from] toolkit_odata::Error),
}

/// Map a `paginate_odata` failure into [`OdataPageError`]. The `Db` arm drops
/// the driver text into the `Db` variant (the caller redacts it for the
/// audit-side diagnostic); every parse/cursor variant stays as `Odata` so the
/// caller can project it to a canonical 400. Shared with
/// [`super::reference_repo`]'s `list_accounts`.
pub(crate) fn map_odata_err(err: toolkit_odata::Error) -> OdataPageError {
    match err {
        toolkit_odata::Error::Db(d) => OdataPageError::Db(d),
        other => OdataPageError::Odata(other),
    }
}

/// Currency-qualified PSP control total, preserving the settlement-entry count basis.
#[derive(Debug)]
pub struct PeriodSettledTotal {
    pub settled: PostedMoney,
    pub settlement_count: usize,
}

/// SeaORM-backed journal repository.
#[derive(Clone)]
pub struct JournalRepo {
    db: DBProvider<DbError>,
}

impl JournalRepo {
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Insert a journal entry header and all its lines inside the same
    /// passed-in transaction. `created_seq` is DB-generated and read
    /// back from the inserted header row. The caller is responsible for
    /// passing an already-balanced entry (the deferrable balance trigger
    /// enforces this on Postgres; `SQLite` relies on the P3 app-level
    /// assertion).
    ///
    /// # Errors
    /// Returns [`RepoError::Db`] if a scoped insert or read-back fails, or
    /// [`RepoError::RowVanished`] if the inserted header cannot be read back.
    pub async fn insert_entry_with_lines(
        &self,
        txn: &DbTx<'_>,
        entry: NewEntry,
        lines: Vec<NewLine>,
    ) -> Result<EntryRef, RepoError> {
        let entry_id = entry.entry_id;
        let tenant_id = entry.tenant_id;
        let period_id = entry.period_id.clone();
        // One rate per entry (§4.3): the entry's locked snapshot is stamped onto
        // every line below. `None` for a single-currency entry. Captured before
        // the header consumes `entry`.
        let rate_snapshot_ref = entry.rate_snapshot_ref;
        // The insert scope is the entry's own tenant: a posting may only
        // write rows it owns. allow-all is not used here on purpose.
        let scope = AccessScope::for_tenant(tenant_id);

        let header = journal_entry::ActiveModel {
            entry_id: Set(entry.entry_id),
            tenant_id: Set(entry.tenant_id),
            legal_entity_id: Set(entry.legal_entity_id),
            period_id: Set(entry.period_id),
            entry_currency: Set(entry.entry_currency),
            source_doc_type: Set(entry.source_doc_type.as_str().to_owned()),
            source_business_id: Set(entry.source_business_id),
            reverses_entry_id: Set(entry.reverses_entry_id),
            reverses_period_id: Set(entry.reverses_period_id),
            posted_at_utc: Set(entry.posted_at_utc),
            effective_at: Set(entry.effective_at),
            origin: Set(entry.origin),
            posted_by_actor_id: Set(entry.posted_by_actor_id),
            correlation_id: Set(entry.correlation_id),
            rounding_evidence: Set(entry.rounding_evidence),
            // DB-generated: never set on insert.
            created_seq: sea_orm::ActiveValue::NotSet,
            row_hash: Set(None),
            prev_hash: Set(None),
            // Chain pointers are sealed by the tamper-evidence chain step, not
            // at insert (mirrors `row_hash` / `prev_hash`).
            prev_entry_id: Set(None),
            prev_period_id: Set(None),
        };

        secure_insert::<journal_entry::Entity>(header, &scope, txn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;

        for line in lines {
            let am = journal_line::ActiveModel {
                line_id: Set(line.line_id),
                entry_id: Set(entry_id),
                tenant_id: Set(tenant_id),
                period_id: Set(period_id.clone()),
                payer_tenant_id: Set(line.payer_tenant_id),
                seller_tenant_id: Set(line.seller_tenant_id),
                resource_tenant_id: Set(line.resource_tenant_id),
                account_id: Set(line.account_id),
                account_class: Set(line.account_class.as_str().to_owned()),
                gl_code: Set(line.gl_code),
                side: Set(line.side.as_str().to_owned()),
                amount: Set(encode_amount(&line.money)),
                currency: Set(line.money.currency().code().to_owned()),
                currency_scale: Set(i16::from(line.money.currency().scale())),
                invoice_id: Set(line.invoice_id),
                due_date: Set(line.due_date),
                revenue_stream: Set(line.revenue_stream),
                mapping_status: Set(line.mapping_status.as_str().to_owned()),
                functional_amount: Set(line.functional_money.as_ref().map(encode_amount)),
                functional_currency: Set(line
                    .functional_money
                    .as_ref()
                    .map(|money| money.currency().code().to_owned())),
                functional_currency_scale: Set(line
                    .functional_money
                    .as_ref()
                    .map(|money| i16::from(money.currency().scale()))),
                tax_jurisdiction: Set(line.tax_jurisdiction),
                tax_filing_period: Set(line.tax_filing_period),
                tax_rate_ref: Set(line.tax_rate_ref),
                legal_entity_id: Set(line.legal_entity_id),
                invoice_item_ref: Set(line.invoice_item_ref),
                sku_or_plan_ref: Set(line.sku_or_plan_ref),
                price_id: Set(line.price_id),
                pricing_snapshot_ref: Set(line.pricing_snapshot_ref),
                po_allocation_group: Set(line.po_allocation_group),
                credit_grant_event_type: Set(line.credit_grant_event_type),
                ar_status: Set(line.ar_status),
                // Slice 5: the entry's locked rate (one per entry, §4.3), stamped
                // onto every line; `None` on a single-currency entry.
                rate_snapshot_ref: Set(rate_snapshot_ref),
            };
            secure_insert::<journal_line::Entity>(am, &scope, txn)
                .await
                .map_err(|e| {
                    crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend())
                })?;
        }

        // Re-read the header to obtain the DB-generated `created_seq`:
        // sea-orm does not project non-PK server-defaults back into the
        // returned Model (and the SQLite sequence is trigger-assigned),
        // so the freshly written value must be read inside the same txn.
        let written = journal_entry::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::EntryId.eq(entry_id))
                    .add(journal_entry::Column::TenantId.eq(tenant_id))
                    .add(journal_entry::Column::PeriodId.eq(period_id)),
            )
            .one(txn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?
            .ok_or_else(|| RepoError::RowVanished(format!("entry {entry_id}")))?;

        Ok(EntryRef {
            entry_id: written.entry_id,
            created_seq: written.created_seq,
        })
    }

    /// Read back an entry and its lines under the supplied scope. The
    /// `SecureORM` `scope_with` narrows by tenant; the key triple pins the
    /// exact row.
    ///
    /// # Errors
    /// Returns [`RepoError::Db`] if acquiring a connection or reading the entry
    /// and its lines fails.
    pub async fn find_entry(
        &self,
        scope: &AccessScope,
        key: EntryKey,
    ) -> Result<Option<EntryRecord>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;

        let header = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::EntryId.eq(key.entry_id))
                    .add(journal_entry::Column::TenantId.eq(key.tenant_id))
                    .add(journal_entry::Column::PeriodId.eq(key.period_id.clone())),
            )
            .one(&conn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;

        let Some(header) = header else {
            return Ok(None);
        };

        let line_rows = journal_line::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_line::Column::EntryId.eq(key.entry_id))
                    .add(journal_line::Column::TenantId.eq(key.tenant_id))
                    .add(journal_line::Column::PeriodId.eq(key.period_id)),
            )
            .all(&conn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;

        let lines = line_rows
            .into_iter()
            .map(line_to_record)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(entry_to_record(header, lines)))
    }

    /// Read an entry and its lines by `(tenant_id, entry_id)` under `scope`,
    /// without the `period_id` (the `get_entry` read seam supplies only the
    /// tenant + id). The unique `entry_id` pins exactly one header. SQL-level
    /// BOLA: a foreign-tenant scope yields `None`.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure.
    pub async fn find_entry_with_lines(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        entry_id: Uuid,
    ) -> Result<Option<EntryRecord>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;

        self.find_entry_with_lines_in(&conn, scope, tenant_id, entry_id)
            .await
    }

    /// Scoped complete journal evidence on the caller's runner, including actual period.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn find_entry_with_lines_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant_id: Uuid,
        entry_id: Uuid,
    ) -> Result<Option<EntryRecord>, RepoError> {
        let header = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::EntryId.eq(entry_id))
                    .add(journal_entry::Column::TenantId.eq(tenant_id)),
            )
            .one(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;

        let Some(header) = header else {
            return Ok(None);
        };

        let line_rows = journal_line::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_line::Column::EntryId.eq(entry_id))
                    .add(journal_line::Column::TenantId.eq(tenant_id)),
            )
            .order_by(journal_line::Column::LineId, Order::Asc)
            .all(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;

        let lines = line_rows
            .into_iter()
            .map(line_to_record)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(entry_to_record(header, lines)))
    }

    /// Read and decode the lines of an entry being reversed, on the caller's runner.
    ///
    /// Returns the rate snapshot the lines share and the decoded lines; the vector is
    /// empty when `scope` sees no line of `entry_id` in `period_id`.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] when a stored line does not
    /// decode or the lines disagree on their rate snapshot.
    pub async fn read_lines_to_reverse_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant_id: Uuid,
        entry_id: Uuid,
        period_id: &str,
    ) -> Result<(Option<Uuid>, Vec<NewLine>), RepoError> {
        let rows = journal_line::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_line::Column::TenantId.eq(tenant_id))
                    .add(journal_line::Column::EntryId.eq(entry_id))
                    .add(journal_line::Column::PeriodId.eq(period_id)),
            )
            .all(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        let rate_snapshot = rows.first().and_then(|row| row.rate_snapshot_ref);
        if rows
            .iter()
            .any(|row| row.rate_snapshot_ref != rate_snapshot)
        {
            return Err(RepoError::InvalidStoredMoney(format!(
                "journal entry {entry_id} has lines with inconsistent rate snapshots"
            )));
        }
        let lines = rows.iter().map(decode_line).collect::<Result<_, _>>()?;
        Ok((rate_snapshot, lines))
    }

    /// List every entry (with its lines) for `(tenant, source_doc_type)` whose
    /// `source_business_id` starts with `business_id_prefix`, under `scope` — the
    /// unrealized-revaluation reversal's lookup of all per-payer `FX_REVALUATION`
    /// entries for one `period:scope:` (each payer is its own entry, so the
    /// reversal fans out over them). Ordered by `entry_id` for a deterministic
    /// reversal order. SQL-level BOLA: a foreign tenant yields no entries.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure.
    pub async fn list_entries_with_lines_by_doc_prefix(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        source_doc_type: &str,
        business_id_prefix: &str,
    ) -> Result<Vec<EntryRecord>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let headers = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::TenantId.eq(tenant_id))
                    .add(journal_entry::Column::SourceDocType.eq(source_doc_type))
                    .add(journal_entry::Column::SourceBusinessId.starts_with(business_id_prefix)),
            )
            .order_by(journal_entry::Column::EntryId, Order::Asc)
            .all(&conn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;

        let mut out = Vec::with_capacity(headers.len());
        for header in headers {
            let entry_id = header.entry_id;
            let line_rows = journal_line::Entity::find()
                .secure()
                .scope_with(scope)
                .filter(
                    Condition::all()
                        .add(journal_line::Column::EntryId.eq(entry_id))
                        .add(journal_line::Column::TenantId.eq(tenant_id)),
                )
                .order_by(journal_line::Column::LineId, Order::Asc)
                .all(&conn)
                .await
                .map_err(|e| {
                    crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend())
                })?;
            let lines = line_rows
                .into_iter()
                .map(line_to_record)
                .collect::<Result<Vec<_>, _>>()?;
            out.push(entry_to_record(header, lines));
        }
        Ok(out)
    }

    /// Period-scoped NET settled total for the PSP reconciliation (C2). Sums the
    /// `UNALLOCATED` legs on the period's settlement journal — `CR` on each
    /// `PAYMENT_SETTLE` (`+gross`) minus `DR` on each `SETTLEMENT_RETURN`
    /// (`−returned`) — yielding the period's net-of-returns settled, the SAME basis
    /// the PSP report carries (`SettlementReport.settled` is net of
    /// refunds/returns). This replaces the lifetime per-payment
    /// `payment_settlement.settled` counter (PK `(tenant, payment_id)`, no
    /// period column), which put the two recon sides on different bases. In-memory
    /// fold (the gear exposes no SQL aggregate); the scoped read keeps SQL-level
    /// BOLA. Runs on the caller's `runner` (the recon txn) so it joins that snapshot.
    /// Only headers in the report currency contribute to both amount and count;
    /// each contributing line must match the supplied stored scale.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope / storage failure; [`RepoError::InvalidStoredMoney`]
    /// for corrupt money. Exact sums are currency-qualified and narrowed only
    /// after every leg has contributed; range and metadata errors remain typed.
    pub async fn sum_period_settled_net<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        period_id: &str,
        currency: &CurrencySpec,
    ) -> Result<PeriodSettledTotal, RepoError> {
        let entries = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::TenantId.eq(tenant))
                    .add(journal_entry::Column::PeriodId.eq(period_id))
                    .add(journal_entry::Column::EntryCurrency.eq(currency.code()))
                    .add(journal_entry::Column::SourceDocType.is_in([SETTLE_DOC, RETURN_DOC])),
            )
            .all(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        if entries.is_empty() {
            return Ok(PeriodSettledTotal {
                settled: PostedMoney::try_new(Decimal::ZERO, currency.clone())?,
                settlement_count: 0,
            });
        }
        // PAYMENT_SETTLE entry count — the per-1000 rounding-tolerance basis the
        // caller uses (returns excluded; they reduce the total, not the count).
        let settle_count = entries
            .iter()
            .filter(|e| e.source_doc_type == SETTLE_DOC)
            .count();
        let entry_ids: Vec<Uuid> = entries.iter().map(|e| e.entry_id).collect();
        let lines = journal_line::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_line::Column::TenantId.eq(tenant))
                    .add(journal_line::Column::EntryId.is_in(entry_ids)),
            )
            .all(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        // UNALLOCATED legs only: the settle's CR (gross) adds, a return's DR
        // (reversal) subtracts — netting to the period's settled total.
        Ok(PeriodSettledTotal {
            settled: settled_total(&lines, currency)?,
            settlement_count: settle_count,
        })
    }

    /// List journal lines for `tenant_id` under `scope`, cursor-paginated via
    /// the canonical `query` (`$filter` / `$orderby` / `limit` / `cursor`). The
    /// caller's `tenant_id` predicate is pre-applied to the secured select; the
    /// user `$filter` is **additive over** that scope (`paginate_odata` ANDs it
    /// in), so a foreign filter value still ANDs the `tenant_id` + `SecureORM`
    /// scope (SQL-level BOLA). Returns the canonical [`Page`] envelope. A bare
    /// list defaults to `line_id ASC` (the entity PK), matching the foundation.
    ///
    /// Mirrors RBAC's `RoleAssignmentRepository::list`:
    /// `Entity::find().secure().scope_with(scope)` pre-filtered by the
    /// caller-derived predicate, then handed to `paginate_odata`.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure;
    /// [`OdataPageError::Odata`] on a malformed `$filter` / `$orderby` / cursor
    /// (the caller projects it to a canonical 400).
    pub async fn list_lines(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<LineRecord>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(format!("conn: {e}")))?;

        // Pre-apply the tenant predicate to the secured select; the user
        // `$filter` is applied additively by `paginate_odata` (it never
        // replaces this scope — BOLA preserved).
        let base_select = journal_line::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(journal_line::Column::TenantId.eq(tenant_id)));

        let query = query_with_default_order(query, "line_id");
        let page = paginate_odata::<
            JournalLineFilterField,
            JournalLineODataMapper,
            journal_line::Entity,
            journal_line::Model,
            _,
            _,
        >(
            base_select,
            &conn,
            &query,
            ("line_id", SortDir::Asc),
            LINE_LIMIT_CFG,
            |m| m,
        )
        .await
        .map_err(map_odata_err)?;
        decode_page(page, line_to_record)
    }

    /// List journal entry HEADERS for `tenant_id` under `scope`, cursor-paginated
    /// via the canonical `query` (`$filter` over `source_doc_type` /
    /// `source_business_id` / `period_id`, `$orderby` / `limit` / `cursor`). This
    /// is the header-only list (R5) — a separate collection over `journal_entry`
    /// (NOT a new `journal_line` filter) because `source_doc_type` /
    /// `source_business_id` are columns on the entry HEADER, never on the line, so
    /// "list all `MANUAL_ADJUSTMENT` entries" / "all `REFUND` / `CREDIT_NOTE`
    /// entries" can only be served from the header table. The caller's `tenant_id`
    /// predicate is pre-applied to the secured select; the user `$filter` is
    /// **additive over** that scope (`paginate_odata` ANDs it in), so a foreign
    /// filter value still ANDs the `tenant_id` + `SecureORM` scope (SQL-level
    /// BOLA). Returns the canonical [`Page`] envelope of `journal_entry` headers
    /// (NO lines — a caller reads the full entry+lines via `get_entry`). A bare
    /// list defaults to `entry_id ASC` (the `journal_entry` PK's keyset leg).
    /// Mirrors [`Self::list_lines`].
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure;
    /// [`OdataPageError::Odata`] on a malformed `$filter` / `$orderby` / cursor
    /// (the caller projects it to a canonical 400).
    pub async fn list_entries(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<journal_entry::Model>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(format!("conn: {e}")))?;

        // Pre-apply the tenant predicate to the secured select; the user
        // `$filter` is applied additively by `paginate_odata` (it never
        // replaces this scope — BOLA preserved).
        let base_select = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(journal_entry::Column::TenantId.eq(tenant_id)));

        let query = query_with_default_order(query, "entry_id");
        paginate_odata::<
            JournalEntryFilterField,
            JournalEntryODataMapper,
            journal_entry::Entity,
            journal_entry::Model,
            _,
            _,
        >(
            base_select,
            &conn,
            &query,
            ("entry_id", SortDir::Asc),
            ENTRY_LIMIT_CFG,
            |m| m,
        )
        .await
        .map_err(map_odata_err)
    }

    /// List the `account_balance` cache rows for `tenant_id` under `scope`,
    /// cursor-paginated via the canonical `query` (`$filter` over
    /// `account_class` / `currency`, `$orderby` / `limit` / `cursor`), each row
    /// decoded into the SDK view with its own stored currency and scale and its
    /// parsed account class ([`decode_balance_view`]). The tenant predicate is
    /// pre-applied to the secured select; the user `$filter` is additive over it
    /// (SQL-level BOLA — a foreign value still ANDs the scope). A bare list
    /// defaults to `account_id ASC`.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure or a stored row
    /// that does not decode; [`OdataPageError::Odata`] on a malformed `$filter` /
    /// `$orderby` / cursor (the caller projects it to a canonical 400).
    pub async fn list_balance_records(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<BalanceView>, OdataPageError> {
        let page = self.balance_models(scope, tenant_id, query).await?;
        decode_page(page, decode_balance_view)
    }

    /// [`Self::list_balance_records`] as raw cache rows, each row's money
    /// validated but left as stored text (storage-level readers only).
    ///
    /// # Errors
    /// As [`Self::list_balance_records`].
    pub async fn list_balances(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<account_balance::Model>, OdataPageError> {
        let page = self.balance_models(scope, tenant_id, query).await?;
        for row in &page.items {
            balance_money(row).map_err(|error| OdataPageError::Db(error.to_string()))?;
        }
        Ok(page)
    }

    /// One page of undecoded `account_balance` rows (see [`Self::list_balance_records`]).
    async fn balance_models(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<account_balance::Model>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(format!("conn: {e}")))?;

        let base_select = account_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(account_balance::Column::TenantId.eq(tenant_id)));

        let query = query_with_default_order(query, "account_id");
        // `account_id` is the tiebreaker below and only the middle third of the
        // `(tenant_id, account_id, currency)` key: an account carried in two
        // currencies has two rows sharing it, so a caller's `$orderby` would
        // leave `[field, account_id]` — the pair collides in the keyset
        // predicate and the second currency falls out at the page boundary.
        let query = query_with_unique_order(&query, &[BalanceFilterField::Currency]);
        paginate_odata::<
            BalanceFilterField,
            BalanceODataMapper,
            account_balance::Entity,
            account_balance::Model,
            _,
            _,
        >(
            base_select,
            &conn,
            &query,
            ("account_id", SortDir::Asc),
            BALANCE_LIMIT_CFG,
            |m| m,
        )
        .await
        .map_err(map_odata_err)
    }

    /// The balance of one account, decoded with its own stored currency and
    /// scale, or `None` when the account has no balance row yet. v1 carries one
    /// currency per account, so `(tenant_id, account_id)` names one row.
    /// SQL-level BOLA: a foreign-tenant scope yields `None`.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage
    /// failure; [`RepoError::Conflict`] on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when the stored balance is malformed.
    pub async fn read_account_balance(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        account_id: Uuid,
    ) -> Result<Option<PostedMoney>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let row = account_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(account_balance::Column::TenantId.eq(tenant_id))
                    .add(account_balance::Column::AccountId.eq(account_id)),
            )
            .one(&conn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        // The one `account_balance` money decoder (shared with the list and the
        // payment carried-balance read), so a corrupt functional triple is
        // refused here too.
        row.map(|r| balance_money(&r).map(|(balance, _)| balance))
            .transpose()
    }

    /// List the `ar_invoice_balance` cache rows for `tenant_id` under `scope`,
    /// optionally narrowed to one `payer_tenant_id`, each decoded with its own
    /// stored currency and scale. SQL-level BOLA: a foreign-tenant scope yields
    /// no rows.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure; [`RepoError::InvalidStoredMoney`]
    /// when a stored amount is malformed or the disputed amount leaves the balance.
    pub async fn list_ar_invoice_records(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        payer_tenant_id: Option<Uuid>,
    ) -> Result<Vec<ArInvoiceBalanceView>, RepoError> {
        self.ar_invoice_models(scope, tenant_id, payer_tenant_id)
            .await?
            .into_iter()
            .map(decode_ar_invoice_view)
            .collect()
    }

    /// [`Self::list_ar_invoice_records`] as raw cache rows, each row's money
    /// validated but left as stored text (storage-level readers only).
    ///
    /// # Errors
    /// As [`Self::list_ar_invoice_records`].
    pub async fn list_ar_invoice_balances(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        payer_tenant_id: Option<Uuid>,
    ) -> Result<Vec<ar_invoice_balance::Model>, RepoError> {
        let rows = self
            .ar_invoice_models(scope, tenant_id, payer_tenant_id)
            .await?;
        for row in &rows {
            ar_invoice_money(row)?;
        }
        Ok(rows)
    }

    /// The undecoded `ar_invoice_balance` rows (see [`Self::list_ar_invoice_records`]).
    async fn ar_invoice_models(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        payer_tenant_id: Option<Uuid>,
    ) -> Result<Vec<ar_invoice_balance::Model>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let mut condition =
            Condition::all().add(ar_invoice_balance::Column::TenantId.eq(tenant_id));
        if let Some(payer) = payer_tenant_id {
            condition = condition.add(ar_invoice_balance::Column::PayerTenantId.eq(payer));
        }
        ar_invoice_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(condition)
            .order_by(ar_invoice_balance::Column::InvoiceId, Order::Asc)
            .all(&conn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))
    }

    /// The posted entry for `(tenant_id, source_doc_type, source_business_id)`
    /// under `scope`, on the caller's runner, or `None` when there is none (or
    /// the scope cannot see it).
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on
    /// classified database contention.
    pub async fn find_entry_id_by_business_key_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant_id: Uuid,
        source_doc_type: SourceDocType,
        source_business_id: &str,
    ) -> Result<Option<Uuid>, RepoError> {
        let row = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::TenantId.eq(tenant_id))
                    .add(journal_entry::Column::SourceDocType.eq(source_doc_type.as_str()))
                    .add(journal_entry::Column::SourceBusinessId.eq(source_business_id)),
            )
            .one(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        Ok(row.map(|r| r.entry_id))
    }

    /// Resolve the `entry_id`s for a `(tenant_id, source_business_id)` under
    /// `scope` (the header carries `source_business_id`, the line does not).
    /// Used by the `list_lines` read seam when the SDK filter pins a business
    /// document. SQL-level BOLA: a foreign-tenant scope yields no ids.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure.
    pub async fn entry_ids_for_business_id(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        source_business_id: &str,
    ) -> Result<Vec<Uuid>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let rows = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::TenantId.eq(tenant_id))
                    .add(journal_entry::Column::SourceBusinessId.eq(source_business_id)),
            )
            .all(&conn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        Ok(rows.into_iter().map(|r| r.entry_id).collect())
    }

    /// The locked FX quote evidence of the posted entry identified by
    /// `(source_business_id, source_doc_type)`, read from any cross-currency line's
    /// `rate_snapshot_ref` (one rate per entry, §4.3) -> `fx_rate_snapshot`. `None` for
    /// an absent operation or a single-currency entry (no snapshot); a dangling lock is
    /// corruption.
    ///
    /// The entry, line and snapshot reads are separate autocommit statements on a
    /// pooled handle, not one snapshot. That is sound for these rows only: an entry and
    /// its lines are append-only and commit together, and a line commits after (or
    /// with) the snapshot it references, so a later read never misses what an earlier
    /// read saw.
    /// Lets the dual-control gate value the threshold at the OPERATION's own locked rate
    /// (design D2), not a fresh gate-time rate.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when an operation's locked FX snapshot is missing (a
    /// dangling lock is corruption) or a stored snapshot is malformed.
    pub async fn locked_rate_evidence_for(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        source_business_id: &str,
        source_doc_type: &str,
    ) -> Result<Option<crate::infra::storage::repo::fx_repo::RateSnapshotRow>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let entry_ids: Vec<Uuid> = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::TenantId.eq(tenant_id))
                    .add(journal_entry::Column::SourceBusinessId.eq(source_business_id))
                    .add(journal_entry::Column::SourceDocType.eq(source_doc_type)),
            )
            .all(&conn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?
            .into_iter()
            .map(|r| r.entry_id)
            .collect();
        if entry_ids.is_empty() {
            return Ok(None);
        }
        // Any cross-currency line of the entry carries the lock's snapshot ref.
        let line = journal_line::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_line::Column::TenantId.eq(tenant_id))
                    .add(journal_line::Column::EntryId.is_in(entry_ids))
                    .add(journal_line::Column::RateSnapshotRef.is_not_null()),
            )
            .one(&conn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        let Some(rate_id) = line.and_then(|l| l.rate_snapshot_ref) else {
            return Ok(None);
        };
        let snapshot = crate::infra::storage::repo::FxRepo::new(self.db.clone())
            .read_snapshot_in(&conn, scope, tenant_id, rate_id)
            .await?;
        snapshot.map(Some).ok_or_else(|| {
            RepoError::InvalidStoredMoney("operation locked FX snapshot missing".into())
        })
    }
}

// The repo-side `LineReadFilter` / `BalanceReadFilter` structs are gone: the
// list endpoints now take a canonical `toolkit_odata::ODataQuery` (`$filter` /
// `$orderby` / `cursor`) which `paginate_odata` lowers to a `WHERE`/`ORDER BY`
// against the mapped columns, additive over the SecureORM tenant scope.

/// Decode an `account_balance` cache row into the SDK [`BalanceView`]: its money
/// restored with the stored currency and scale (never reinterpreted through the
/// registry), its class parsed from the stored literal. An unknown literal is
/// stored corruption, reported like any other undecodable row.
pub(crate) fn decode_balance_view(row: account_balance::Model) -> Result<BalanceView, RepoError> {
    let (balance, functional_balance) = balance_money(&row)?;
    let account_class = row.account_class.parse::<AccountClass>().map_err(|_| {
        RepoError::InvalidStoredMoney(format!(
            "account_balance {} account_class: unknown stored literal {:?}",
            row.account_id, row.account_class
        ))
    })?;
    Ok(BalanceView {
        account_id: row.account_id,
        account_class,
        balance,
        functional_balance,
    })
}

/// Decode an `ar_invoice_balance` cache row (the AR-aging input) into the SDK
/// [`ArInvoiceBalanceView`], rejecting a disputed amount outside its balance.
pub(crate) fn decode_ar_invoice_view(
    row: ar_invoice_balance::Model,
) -> Result<ArInvoiceBalanceView, RepoError> {
    let (balance, _, _) = ar_invoice_money(&row)?;
    Ok(ArInvoiceBalanceView {
        payer_tenant_id: row.payer_tenant_id,
        account_id: row.account_id,
        invoice_id: row.invoice_id,
        balance,
        due_date: row.due_date,
    })
}

/// Decode every row of a page; a row that does not decode is stored corruption,
/// reported as a storage failure (its text stays server-side).
fn decode_page<M, T>(
    page: Page<M>,
    decode: impl Fn(M) -> Result<T, RepoError>,
) -> Result<Page<T>, OdataPageError> {
    let items = page
        .items
        .into_iter()
        .map(decode)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| OdataPageError::Db(error.to_string()))?;
    Ok(Page {
        items,
        page_info: page.page_info,
    })
}

/// Map a `journal_entry` row + its lines into the domain record.
fn entry_to_record(row: journal_entry::Model, lines: Vec<LineRecord>) -> EntryRecord {
    EntryRecord {
        entry_id: row.entry_id,
        tenant_id: row.tenant_id,
        legal_entity_id: row.legal_entity_id,
        period_id: row.period_id,
        entry_currency: row.entry_currency,
        source_doc_type: row.source_doc_type,
        source_business_id: row.source_business_id,
        reverses_entry_id: row.reverses_entry_id,
        reverses_period_id: row.reverses_period_id,
        posted_at_utc: row.posted_at_utc,
        effective_at: row.effective_at,
        origin: row.origin,
        posted_by_actor_id: row.posted_by_actor_id,
        correlation_id: row.correlation_id,
        rounding_evidence: row.rounding_evidence,
        created_seq: row.created_seq,
        lines,
    }
}

/// Map a `journal_line` row into the domain record.
pub(crate) fn line_to_record(row: journal_line::Model) -> Result<LineRecord, RepoError> {
    let (money, functional_money) = line_money(&row)?;
    Ok(LineRecord {
        line_id: row.line_id,
        entry_id: row.entry_id,
        tenant_id: row.tenant_id,
        period_id: row.period_id,
        payer_tenant_id: row.payer_tenant_id,
        seller_tenant_id: row.seller_tenant_id,
        resource_tenant_id: row.resource_tenant_id,
        account_id: row.account_id,
        account_class: row.account_class,
        gl_code: row.gl_code,
        side: row.side,
        money,
        invoice_id: row.invoice_id,
        due_date: row.due_date,
        revenue_stream: row.revenue_stream,
        mapping_status: row.mapping_status,
        functional_money,
        tax_jurisdiction: row.tax_jurisdiction,
        tax_filing_period: row.tax_filing_period,
        tax_rate_ref: row.tax_rate_ref,
        legal_entity_id: row.legal_entity_id,
        invoice_item_ref: row.invoice_item_ref,
        sku_or_plan_ref: row.sku_or_plan_ref,
        price_id: row.price_id,
        pricing_snapshot_ref: row.pricing_snapshot_ref,
        po_allocation_group: row.po_allocation_group,
        credit_grant_event_type: row.credit_grant_event_type,
        ar_status: row.ar_status,
    })
}

/// Decode journal transaction and functional money using only stored metadata.
fn line_money(row: &journal_line::Model) -> Result<(PostedMoney, Option<PostedMoney>), RepoError> {
    Ok((
        decode_money(&row.amount, &row.currency, row.currency_scale)?,
        decode_optional_money(
            row.functional_amount.as_deref(),
            row.functional_currency.as_deref(),
            row.functional_currency_scale,
        )?,
    ))
}

/// Decode the money of an `account_balance` row (balance and optional functional
/// triple) without reinterpreting its scale through the registry. The one decoder
/// of that row's money: the journal reads and the payment carried-balance read
/// ([`super::payment_repo::PaymentRepo::read_account_carried_in`]) both use it.
pub(crate) fn balance_money(
    row: &account_balance::Model,
) -> Result<(PostedMoney, Option<PostedMoney>), RepoError> {
    Ok((
        decode_money(&row.balance, &row.currency, row.currency_scale)?,
        decode_optional_money(
            row.functional_balance.as_deref(),
            row.functional_currency.as_deref(),
            row.functional_currency_scale,
        )?,
    ))
}

/// Restore all invoice cache money and reject corrupt correlated disputed amounts.
pub(crate) fn ar_invoice_money(
    row: &ar_invoice_balance::Model,
) -> Result<(PostedMoney, PostedMoney, Option<PostedMoney>), RepoError> {
    let balance = decode_money(&row.balance, &row.currency, row.currency_scale)?;
    let disputed = decode_money(&row.disputed, &row.currency, row.currency_scale)?;
    if disputed.amount() < Decimal::ZERO || disputed.amount() > balance.amount() {
        return Err(RepoError::InvalidStoredMoney(
            "invoice disputed amount outside balance".to_owned(),
        ));
    }
    Ok((
        balance,
        disputed,
        decode_optional_money(
            row.functional_balance.as_deref(),
            row.functional_currency.as_deref(),
            row.functional_currency_scale,
        )?,
    ))
}

/// Fold the report-currency PSP population exactly, validating each contributing scale.
fn settled_total(
    lines: &[journal_line::Model],
    currency: &CurrencySpec,
) -> Result<PostedMoney, RepoError> {
    let mut net = ExactAmount::from_decimal(Decimal::ZERO);
    for line in lines {
        if line.account_class != UNALLOCATED_CLASS {
            continue;
        }
        let (money, _) = line_money(line)?;
        if money.currency().code() != currency.code() {
            return Err(MoneyError::CurrencyMismatch.into());
        }
        if money.currency().scale() != currency.scale() {
            return Err(MoneyError::ScaleMismatch.into());
        }
        let amount = ExactAmount::from_decimal(money.amount());
        net = match line.side.as_str() {
            CREDIT_SIDE => net.checked_add(&amount),
            DEBIT_SIDE => net.checked_sub(&amount),
            _ => {
                return Err(RepoError::InvalidStoredMoney(
                    "invalid settlement journal side".to_owned(),
                ));
            }
        }
        .map_err(exact_repo_error)?;
    }
    net.into_posted_exact(currency.clone())
        .map_err(exact_repo_error)
}

/// Keep final money validation failures typed; arithmetic budget failures are internal.
fn exact_repo_error(error: ExactError) -> RepoError {
    match error {
        ExactError::Money(error) => RepoError::Money(error),
        other => RepoError::Db(format!("settlement total arithmetic: {other}")),
    }
}

/// Decode a stored journal line, money and enums, for reversal, recognition and
/// verifier reconstruction, without consulting the live registry.
///
/// # Errors
/// [`RepoError::InvalidStoredMoney`] naming the line, the column and the stored literal
/// when an enum, the amount or the functional money triple does not decode. The
/// diagnostic is server-side only.
pub(crate) fn decode_line(row: &journal_line::Model) -> Result<NewLine, RepoError> {
    Ok(NewLine {
        line_id: row.line_id,
        payer_tenant_id: row.payer_tenant_id,
        seller_tenant_id: row.seller_tenant_id,
        resource_tenant_id: row.resource_tenant_id,
        account_id: row.account_id,
        account_class: stored_enum::<AccountClass>(row, "account_class", &row.account_class)?,
        gl_code: row.gl_code.clone(),
        side: stored_enum::<Side>(row, "side", &row.side)?,
        money: decode_money(&row.amount, &row.currency, row.currency_scale)
            .map_err(|e| stored_line_error(row, "amount", &e))?,
        invoice_id: row.invoice_id.clone(),
        due_date: row.due_date,
        revenue_stream: row.revenue_stream.clone(),
        mapping_status: stored_enum::<MappingStatus>(row, "mapping_status", &row.mapping_status)?,
        functional_money: decode_optional_money(
            row.functional_amount.as_deref(),
            row.functional_currency.as_deref(),
            row.functional_currency_scale,
        )
        .map_err(|e| stored_line_error(row, "functional_amount", &e))?,
        tax_jurisdiction: row.tax_jurisdiction.clone(),
        tax_filing_period: row.tax_filing_period.clone(),
        tax_rate_ref: row.tax_rate_ref.clone(),
        legal_entity_id: row.legal_entity_id,
        invoice_item_ref: row.invoice_item_ref.clone(),
        sku_or_plan_ref: row.sku_or_plan_ref.clone(),
        price_id: row.price_id.clone(),
        pricing_snapshot_ref: row.pricing_snapshot_ref.clone(),
        po_allocation_group: row.po_allocation_group.clone(),
        credit_grant_event_type: row.credit_grant_event_type.clone(),
        ar_status: row.ar_status.clone(),
    })
}

/// Parse one stored enum column, naming the line, column and literal on failure.
fn stored_enum<T: std::str::FromStr>(
    row: &journal_line::Model,
    column: &str,
    literal: &str,
) -> Result<T, RepoError> {
    literal.parse::<T>().map_err(|_| {
        RepoError::InvalidStoredMoney(format!(
            "journal_line {} {column}: unknown stored literal {literal:?}",
            row.line_id
        ))
    })
}

/// Add the line and column to a stored-money decode failure.
fn stored_line_error(row: &journal_line::Model, column: &str, error: &RepoError) -> RepoError {
    let detail = match error {
        RepoError::InvalidStoredMoney(detail) => detail.clone(),
        other => other.to_string(),
    };
    RepoError::InvalidStoredMoney(format!("journal_line {} {column}: {detail}", row.line_id))
}

#[cfg(test)]
#[path = "journal_repo_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "stored_line_tests.rs"]
mod stored_line_tests;

#[cfg(test)]
#[path = "journal_repo_decimal_tests.rs"]
mod decimal_tests;

#[cfg(test)]
#[path = "journal_repo_read_model_tests.rs"]
mod read_model_tests;
