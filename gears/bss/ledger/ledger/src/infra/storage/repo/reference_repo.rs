//! `ReferenceRepo` — CRUD over the reference tables a posting consults
//! (currency-scale registry, chart of accounts). Tenant isolation runs
//! through the `SecureORM` layer; P1 reads take an explicit `AccessScope`.

use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use toolkit_db::odata::sea_orm_filter::{LimitCfg, paginate_odata};
use toolkit_db::secure::{
    AccessScope, DbTx, ScopeError, SecureEntityExt, SecureInsertExt, TxConfig, secure_insert,
};
use toolkit_db::{DBProvider, DbError};
use toolkit_odata::{ODataQuery, Page, SortDir};
use uuid::Uuid;

use crate::domain::model::{
    AccountRow, CurrencyScaleRow, FiscalCalendarRow, FiscalPeriodRow, RepoError,
};
use crate::domain::money::scale_fits_headroom;
use crate::infra::storage::entity::{
    currency_scale_registry, fiscal_calendar, fiscal_period, journal_line, tenant_account,
    tenant_posting_lock,
};
use crate::infra::storage::odata_mapping::AccountInfoODataMapper;
use crate::infra::storage::repo::journal_repo::{
    OdataPageError, map_odata_err, query_with_default_order,
};
use crate::odata::AccountInfoFilterField;

/// Per-endpoint pagination bounds for `GET /bss-ledger/v1/accounts`. The chart
/// of accounts is small per tenant, so the platform-standard default 25 still
/// keeps the common read one round-trip.
const ACCOUNT_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 200,
};

/// SeaORM-backed reference-data repository.
#[derive(Clone)]
pub struct ReferenceRepo {
    db: DBProvider<DbError>,
}

impl ReferenceRepo {
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Insert or update a currency-scale registry row keyed by
    /// `(tenant_id, currency)`. Rejects an out-of-headroom scale at
    /// registration (`ScaleOutOfRange`) and a changed scale once postings
    /// exist for the currency (`CurrencyScaleLocked`, architecture I-1);
    /// the same scale is an idempotent no-op.
    ///
    /// # Errors
    /// [`RepoError::ScaleOutOfRange`] / [`RepoError::CurrencyScaleLocked`]
    /// per the guards above, or [`RepoError::Db`] on a storage failure.
    pub async fn upsert_currency_scale(&self, row: CurrencyScaleRow) -> Result<(), RepoError> {
        if !scale_fits_headroom(row.minor_units, row.plausible_max_major) {
            return Err(RepoError::ScaleOutOfRange(row.currency));
        }
        // ONE `SERIALIZABLE` transaction so the scale-immutability check-then-act
        // is atomic: the `posted?` probe and a concurrent first posting for the
        // currency form a read-write conflict under Postgres SSI, so a post that
        // lands between the probe and the upsert aborts the loser — a changed
        // scale can never be committed once postings exist. `Locked` carries the
        // rejection out on the commit path (it writes nothing, so committing is a
        // harmless no-op), avoiding a sentinel round-trip.
        let currency = row.currency.clone();
        let result: Result<ScaleUpsertResult, DbError> = self
            .db
            .db()
            .transaction_with_retry(TxConfig::serializable(), as_db_err, move |txn| {
                let row = row.clone();
                Box::pin(async move { upsert_currency_scale_in_txn(txn, row).await })
            })
            .await;
        match result {
            Ok(ScaleUpsertResult::Done) => Ok(()),
            Ok(ScaleUpsertResult::Locked) => Err(RepoError::CurrencyScaleLocked(currency)),
            Err(db_err) => Err(RepoError::Db(format!(
                "upsert currency_scale txn: {db_err}"
            ))),
        }
    }

    /// Read a currency-scale row under the supplied scope.
    pub async fn find_currency_scale(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        currency: &str,
    ) -> Result<Option<CurrencyScaleRow>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let row = currency_scale_registry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(currency_scale_registry::Column::TenantId.eq(tenant_id))
                    .add(currency_scale_registry::Column::Currency.eq(currency)),
            )
            .one(&conn)
            .await
            .map_err(|e| RepoError::Db(format!("find currency_scale: {e}")))?;
        Ok(row.map(|m| CurrencyScaleRow {
            tenant_id: m.tenant_id,
            currency: m.currency,
            minor_units: m.minor_units,
            plausible_max_major: m.plausible_max_major,
            source: m.source,
        }))
    }

    /// Insert a chart-of-accounts row.
    pub async fn insert_account(&self, row: AccountRow) -> Result<(), RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let scope = AccessScope::for_tenant(row.tenant_id);

        let am = tenant_account::ActiveModel {
            account_id: Set(row.account_id),
            tenant_id: Set(row.tenant_id),
            legal_entity_id: Set(row.legal_entity_id),
            account_class: Set(row.account_class),
            currency: Set(row.currency),
            revenue_stream: Set(row.revenue_stream),
            normal_side: Set(row.normal_side),
            may_go_negative: Set(row.may_go_negative),
            lifecycle_state: Set(row.lifecycle_state),
        };

        secure_insert::<tenant_account::Entity>(am, &scope, &conn)
            .await
            .map_err(|e| RepoError::Db(format!("insert tenant_account: {e}")))?;
        Ok(())
    }

    /// Read a chart-of-accounts row by id under the supplied scope.
    /// Whether the tenant's posting kill-switch (`tenant_posting_lock`) is
    /// currently held (design §3.2 PostingService pre-transaction gate). A
    /// missing row means never locked (the table is written only when a lock is
    /// set / cleared), so absence reads as `false`. Read on its own connection
    /// BEFORE the post transaction, mirroring the account-lifecycle pre-check:
    /// tolerable that a lock set CONCURRENTLY (after this read, before COMMIT) is
    /// not caught, since termination is a rare admin op.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a connection or query failure.
    pub async fn is_tenant_posting_locked(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<bool, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let row = tenant_posting_lock::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(tenant_posting_lock::Column::TenantId.eq(tenant_id)))
            .one(&conn)
            .await
            .map_err(|e| RepoError::Db(format!("find tenant_posting_lock: {e}")))?;
        Ok(row.is_some_and(|m| m.locked))
    }

    pub async fn find_account(
        &self,
        scope: &AccessScope,
        account_id: Uuid,
    ) -> Result<Option<AccountRow>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let row = tenant_account::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(tenant_account::Column::AccountId.eq(account_id)))
            .one(&conn)
            .await
            .map_err(|e| RepoError::Db(format!("find tenant_account: {e}")))?;
        Ok(row.map(|m| AccountRow {
            account_id: m.account_id,
            tenant_id: m.tenant_id,
            legal_entity_id: m.legal_entity_id,
            account_class: m.account_class,
            currency: m.currency,
            revenue_stream: m.revenue_stream,
            normal_side: m.normal_side,
            may_go_negative: m.may_go_negative,
            lifecycle_state: m.lifecycle_state,
        }))
    }

    /// Full chart-of-accounts read for the tenant — **NOT paginated**. The
    /// invoice-post chart resolver needs every account to bind line ids, so it
    /// bypasses the `OData` page of the REST `list_accounts` (a page would
    /// truncate a chart with many per-stream Revenue accounts). SecureORM-scoped
    /// (BOLA): a foreign tenant yields an empty `Vec`.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage / connection failure.
    pub async fn all_accounts(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<tenant_account::Model>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        tenant_account::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(tenant_account::Column::TenantId.eq(tenant_id)))
            .all(&conn)
            .await
            .map_err(|e| RepoError::Db(format!("all_accounts: {e}")))
    }

    /// List the chart-of-accounts rows for a tenant under `scope`, cursor-
    /// paginated via the canonical `query` (`$filter` over `account_class` /
    /// `currency` / `revenue_stream` / `lifecycle_state`, `$orderby` / `limit` /
    /// `cursor`). The tenant predicate is pre-applied to the secured select; the
    /// user `$filter` is **additive over** it (SQL-level BOLA — a tenant outside
    /// the caller's scope yields an empty page, and a foreign filter value still
    /// ANDs the scope). A bare list defaults to `account_id ASC`. Returns the
    /// raw `tenant_account` model page; the caller projects each row to the SDK
    /// `AccountInfo`.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure;
    /// [`OdataPageError::Odata`] on a malformed `$filter` / `$orderby` / cursor.
    pub async fn list_accounts(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<tenant_account::Model>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(format!("conn: {e}")))?;

        let base_select = tenant_account::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(tenant_account::Column::TenantId.eq(tenant_id)));

        let query = query_with_default_order(query, "account_id");
        paginate_odata::<
            AccountInfoFilterField,
            AccountInfoODataMapper,
            tenant_account::Entity,
            tenant_account::Model,
            _,
            _,
        >(
            base_select,
            &conn,
            &query,
            ("account_id", SortDir::Asc),
            ACCOUNT_LIMIT_CFG,
            |m| m,
        )
        .await
        .map_err(map_odata_err)
    }

    /// List ALL fiscal-calendar rows across every tenant — a **cross-tenant
    /// system read** for the period-open job. Uses the secure layer with
    /// [`AccessScope::allow_all`] (the sanctioned all-tenants system scope,
    /// same as AM's reaper/lease paths), NOT a per-request tenant scope; the
    /// per-calendar period insert is then scoped by
    /// `insert_fiscal_period_if_absent_txn`.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure.
    pub async fn list_all_fiscal_calendars(&self) -> Result<Vec<FiscalCalendarRow>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let rows = fiscal_calendar::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await
            .map_err(|e| RepoError::Db(format!("list fiscal_calendar: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|m| FiscalCalendarRow {
                tenant_id: m.tenant_id,
                legal_entity_id: m.legal_entity_id,
                fiscal_tz: m.fiscal_tz,
                granularity: m.granularity,
                fy_start_month: m.fy_start_month,
                functional_currency: m.functional_currency,
            })
            .collect())
    }

    /// The tenant's legal-entity functional currency (S5-F3), or `None` when the
    /// tenant has no fiscal calendar, or its calendar carries no functional
    /// currency (a single-currency tenant — the `RateLocker` then short-circuits).
    /// v1 assumes one legal entity per tenant (decision 5): the tenant's single
    /// calendar row carries the functional currency; a multi-LE tenant (deferred)
    /// needs the per-`legal_entity_id` lookup. Reference data, tenant-axis scope.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure.
    pub async fn functional_currency(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<String>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let row = fiscal_calendar::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(fiscal_calendar::Column::TenantId.eq(tenant_id)))
            .one(&conn)
            .await
            .map_err(|e| RepoError::Db(format!("find fiscal_calendar functional_currency: {e}")))?;
        Ok(row.and_then(|m| m.functional_currency))
    }

    // --- Additive seed methods (run inside one provisioning transaction) ---
    //
    // Each takes the active `DbTx` so reads + inserts share the one txn (no
    // fresh `self.db.conn()` — that would open a new connection and trip the
    // in-tx guard). Idempotency is per-row SELECT-then-INSERT keyed on the
    // natural key; existing rows are left untouched (no `on_conflict`).

    /// Find a chart-of-accounts row by its coordinate key under `scope`,
    /// running on the supplied transaction. Returns the `account_id` if a
    /// matching row exists. The `revenue_stream` arm matches the
    /// `COALESCE(revenue_stream,'-')` unique-index semantics: `Some(s)` →
    /// equality, `None` → `IS NULL`.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure.
    // The CoA natural key is wide (tenant + legal-entity + class + currency +
    // revenue-stream); passing it positionally mirrors the unique index columns.
    #[allow(clippy::too_many_arguments)]
    pub async fn find_account_by_key_txn(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        legal_entity_id: Uuid,
        account_class: &str,
        currency: &str,
        revenue_stream: Option<&str>,
    ) -> Result<Option<Uuid>, RepoError> {
        let mut condition = Condition::all()
            .add(tenant_account::Column::TenantId.eq(tenant_id))
            .add(tenant_account::Column::LegalEntityId.eq(legal_entity_id))
            .add(tenant_account::Column::AccountClass.eq(account_class))
            .add(tenant_account::Column::Currency.eq(currency));
        condition = match revenue_stream {
            Some(s) => condition.add(tenant_account::Column::RevenueStream.eq(s)),
            None => condition.add(tenant_account::Column::RevenueStream.is_null()),
        };
        let row = tenant_account::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(condition)
            .one(txn)
            .await
            .map_err(|e| RepoError::Db(format!("find tenant_account by key: {e}")))?;
        Ok(row.map(|m| m.account_id))
    }

    /// Insert a chart-of-accounts row if absent (keyed on its coordinate),
    /// running on the supplied transaction. Returns the persistent `account_id`
    /// (the existing row's when present, else the freshly inserted one) and
    /// `true` when a new row was inserted (`false` = already existed, no-op).
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure.
    pub async fn insert_account_if_absent_txn(
        &self,
        txn: &DbTx<'_>,
        row: AccountRow,
    ) -> Result<(Uuid, bool), RepoError> {
        let scope = AccessScope::for_tenant(row.tenant_id);
        if let Some(existing_id) = self
            .find_account_by_key_txn(
                txn,
                &scope,
                row.tenant_id,
                row.legal_entity_id,
                &row.account_class,
                &row.currency,
                row.revenue_stream.as_deref(),
            )
            .await?
        {
            return Ok((existing_id, false));
        }

        let new_id = row.account_id;
        let am = tenant_account::ActiveModel {
            account_id: Set(row.account_id),
            tenant_id: Set(row.tenant_id),
            legal_entity_id: Set(row.legal_entity_id),
            account_class: Set(row.account_class),
            currency: Set(row.currency),
            revenue_stream: Set(row.revenue_stream),
            normal_side: Set(row.normal_side),
            may_go_negative: Set(row.may_go_negative),
            lifecycle_state: Set(row.lifecycle_state),
        };
        secure_insert::<tenant_account::Entity>(am, &scope, txn)
            .await
            .map_err(|e| RepoError::Db(format!("insert tenant_account: {e}")))?;
        Ok((new_id, true))
    }

    /// Insert a currency-scale registry row if absent (keyed on
    /// `(tenant_id, currency)`), running on the supplied transaction. Validates
    /// headroom before any write. Returns `true` when a new row was inserted,
    /// `false` when one already existed (no-op — the existing scale is left
    /// untouched; scale-immutability is `upsert_currency_scale`'s concern).
    ///
    /// # Errors
    /// [`RepoError::ScaleOutOfRange`] when the scale exceeds `i64` headroom, or
    /// [`RepoError::Db`] on a storage failure.
    pub async fn insert_currency_scale_if_absent_txn(
        &self,
        txn: &DbTx<'_>,
        row: CurrencyScaleRow,
    ) -> Result<bool, RepoError> {
        if !scale_fits_headroom(row.minor_units, row.plausible_max_major) {
            return Err(RepoError::ScaleOutOfRange(row.currency));
        }
        let scope = AccessScope::for_tenant(row.tenant_id);
        let existing = currency_scale_registry::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(currency_scale_registry::Column::TenantId.eq(row.tenant_id))
                    .add(currency_scale_registry::Column::Currency.eq(row.currency.clone())),
            )
            .one(txn)
            .await
            .map_err(|e| RepoError::Db(format!("find currency_scale: {e}")))?;
        if existing.is_some() {
            return Ok(false);
        }

        let am = currency_scale_registry::ActiveModel {
            tenant_id: Set(row.tenant_id),
            currency: Set(row.currency),
            minor_units: Set(row.minor_units),
            plausible_max_major: Set(row.plausible_max_major),
            source: Set(row.source),
        };
        currency_scale_registry::Entity::insert(am.clone())
            .secure()
            .scope_with_model(&scope, &am)
            .map_err(|e| RepoError::Db(format!("insert currency_scale scope: {e}")))?
            .exec(txn)
            .await
            .map_err(|e| RepoError::Db(format!("insert currency_scale: {e}")))?;
        Ok(true)
    }

    /// Insert a fiscal-calendar config row if absent (keyed on
    /// `(tenant_id, legal_entity_id)`), running on the supplied transaction.
    /// Returns `true` when a new row was inserted, `false` when one already
    /// existed (no-op — additive).
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure.
    pub async fn upsert_fiscal_calendar_if_absent_txn(
        &self,
        txn: &DbTx<'_>,
        row: FiscalCalendarRow,
    ) -> Result<bool, RepoError> {
        let scope = AccessScope::for_tenant(row.tenant_id);
        let existing = fiscal_calendar::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(fiscal_calendar::Column::TenantId.eq(row.tenant_id))
                    .add(fiscal_calendar::Column::LegalEntityId.eq(row.legal_entity_id)),
            )
            .one(txn)
            .await
            .map_err(|e| RepoError::Db(format!("find fiscal_calendar: {e}")))?;
        if existing.is_some() {
            return Ok(false);
        }

        let am = fiscal_calendar::ActiveModel {
            tenant_id: Set(row.tenant_id),
            legal_entity_id: Set(row.legal_entity_id),
            fiscal_tz: Set(row.fiscal_tz),
            granularity: Set(row.granularity),
            fy_start_month: Set(row.fy_start_month),
            functional_currency: Set(row.functional_currency),
        };
        fiscal_calendar::Entity::insert(am.clone())
            .secure()
            .scope_with_model(&scope, &am)
            .map_err(|e| RepoError::Db(format!("insert fiscal_calendar scope: {e}")))?
            .exec(txn)
            .await
            .map_err(|e| RepoError::Db(format!("insert fiscal_calendar: {e}")))?;
        Ok(true)
    }

    /// Insert a fiscal-period row if absent (keyed on
    /// `(tenant_id, legal_entity_id, period_id)`), running on the supplied
    /// transaction. Returns `true` when a new row was inserted, `false` when
    /// one already existed (no-op — additive).
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure.
    pub async fn insert_fiscal_period_if_absent_txn(
        &self,
        txn: &DbTx<'_>,
        row: FiscalPeriodRow,
    ) -> Result<bool, RepoError> {
        let scope = AccessScope::for_tenant(row.tenant_id);
        let existing = fiscal_period::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(fiscal_period::Column::TenantId.eq(row.tenant_id))
                    .add(fiscal_period::Column::LegalEntityId.eq(row.legal_entity_id))
                    .add(fiscal_period::Column::PeriodId.eq(row.period_id.clone())),
            )
            .one(txn)
            .await
            .map_err(|e| RepoError::Db(format!("find fiscal_period: {e}")))?;
        if existing.is_some() {
            return Ok(false);
        }

        let am = fiscal_period::ActiveModel {
            tenant_id: Set(row.tenant_id),
            legal_entity_id: Set(row.legal_entity_id),
            period_id: Set(row.period_id),
            fiscal_tz: Set(row.fiscal_tz),
            status: Set(row.status),
        };
        fiscal_period::Entity::insert(am.clone())
            .secure()
            .scope_with_model(&scope, &am)
            .map_err(|e| RepoError::Db(format!("insert fiscal_period scope: {e}")))?
            .exec(txn)
            .await
            .map_err(|e| RepoError::Db(format!("insert fiscal_period: {e}")))?;
        Ok(true)
    }
}

/// Carries the scale-upsert decision out of the `SERIALIZABLE` body on the
/// commit path: `Locked` writes nothing, so the transaction commits harmlessly.
enum ScaleUpsertResult {
    Done,
    Locked,
}

/// In-transaction body of [`ReferenceRepo::upsert_currency_scale`]: re-read the
/// existing scale, probe for postings on a changed scale, then upsert — all in
/// the one serializable snapshot so a concurrent first posting cannot slip
/// between the probe and the write.
async fn upsert_currency_scale_in_txn(
    txn: &DbTx<'_>,
    row: CurrencyScaleRow,
) -> Result<ScaleUpsertResult, DbError> {
    let scope = AccessScope::for_tenant(row.tenant_id);
    let existing = currency_scale_registry::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(
            Condition::all()
                .add(currency_scale_registry::Column::TenantId.eq(row.tenant_id))
                .add(currency_scale_registry::Column::Currency.eq(row.currency.clone())),
        )
        .one(txn)
        .await
        .map_err(scope_to_db)?;

    if let Some(existing) = existing
        && existing.minor_units != row.minor_units
    {
        let posted = journal_line::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(journal_line::Column::TenantId.eq(row.tenant_id))
                    .add(journal_line::Column::Currency.eq(row.currency.clone())),
            )
            .one(txn)
            .await
            .map_err(scope_to_db)?;
        if posted.is_some() {
            return Ok(ScaleUpsertResult::Locked);
        }
    }

    let am = currency_scale_registry::ActiveModel {
        tenant_id: Set(row.tenant_id),
        currency: Set(row.currency.clone()),
        minor_units: Set(row.minor_units),
        plausible_max_major: Set(row.plausible_max_major),
        source: Set(row.source.clone()),
    };
    let on_conflict = OnConflict::columns([
        currency_scale_registry::Column::TenantId,
        currency_scale_registry::Column::Currency,
    ])
    .update_columns([
        currency_scale_registry::Column::MinorUnits,
        currency_scale_registry::Column::PlausibleMaxMajor,
        currency_scale_registry::Column::Source,
    ])
    .to_owned();
    currency_scale_registry::Entity::insert(am.clone())
        .secure()
        .scope_with_model(&scope, &am)
        .map_err(scope_to_db)?
        .on_conflict_raw(on_conflict)
        .exec(txn)
        .await
        .map_err(scope_to_db)?;
    Ok(ScaleUpsertResult::Done)
}

fn as_db_err(e: &DbError) -> Option<&sea_orm::DbErr> {
    match e {
        DbError::Sea(db_err) => Some(db_err),
        _ => None,
    }
}

fn scope_to_db(e: ScopeError) -> DbError {
    match e {
        ScopeError::Db(db_err) => DbError::Sea(db_err),
        other => DbError::Other(anyhow::anyhow!("scope: {other}")),
    }
}
