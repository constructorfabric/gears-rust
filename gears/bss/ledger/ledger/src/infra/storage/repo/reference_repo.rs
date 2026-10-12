//! `ReferenceRepo` — CRUD over the reference tables a posting consults
//! (currency-scale registry, chart of accounts). Tenant isolation runs
//! through the `SecureORM` layer; P1 reads take an explicit `AccessScope`.

use bss_ledger_sdk::CurrencySpec;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use toolkit_db::odata::sea_orm_filter::{LimitCfg, paginate_odata};
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, ScopeError, SecureEntityExt, SecureInsertExt, TxConfig,
    secure_insert,
};
use toolkit_db::{DBProvider, DbError};
use toolkit_odata::{ODataQuery, Page, SortDir};
use uuid::Uuid;

use crate::domain::model::{
    AccountRow, CurrencyScaleRow, FiscalCalendarRow, FiscalPeriodRow, RepoError,
};
use crate::infra::storage::entity::{
    currency_scale_registry, fiscal_calendar, fiscal_period, journal_line, tenant_account,
    tenant_posting_lock,
};
use crate::infra::storage::money_text::decode_currency;
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
    /// `(tenant_id, currency)`. Rejects an unsupported scale at
    /// registration (`ScaleOutOfRange`) and a changed scale once postings
    /// exist for the currency (`CurrencyScaleLocked`, architecture I-1);
    /// the same scale is an idempotent no-op.
    ///
    /// # Errors
    /// [`RepoError::ScaleOutOfRange`] / [`RepoError::CurrencyScaleLocked`]
    /// per the guards above, or [`RepoError::Db`] on a storage failure.
    pub async fn upsert_currency_scale(&self, row: CurrencyScaleRow) -> Result<(), RepoError> {
        if row.currency_scale > 28 {
            return Err(RepoError::ScaleOutOfRange(row.currency));
        }
        CurrencySpec::try_new(row.currency.clone(), row.currency_scale)?;
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
            Err(db_err) => Err(crate::infra::posting::retry::db_to_repo(
                db_err,
                self.db.db().backend(),
            )),
        }
    }

    /// Read a currency-scale row under the supplied scope.
    ///
    /// # Errors
    /// Returns [`RepoError::Db`] if acquiring a connection or reading the scale
    /// row fails.
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
        self.find_currency_scale_in(&conn, scope, tenant_id, currency)
            .await
    }

    /// Read currency metadata on the caller's snapshot, including a posting transaction.
    ///
    /// # Errors
    /// Returns a repository error for database access or corrupt stored metadata.
    pub async fn find_currency_scale_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant_id: Uuid,
        currency: &str,
    ) -> Result<Option<CurrencyScaleRow>, RepoError> {
        let row = currency_scale_registry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(currency_scale_registry::Column::TenantId.eq(tenant_id))
                    .add(currency_scale_registry::Column::Currency.eq(currency)),
            )
            .one(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        row.map(|m| {
            let spec = decode_currency(&m.currency, m.currency_scale)?;
            Ok(CurrencyScaleRow {
                tenant_id: m.tenant_id,
                currency: m.currency,
                currency_scale: spec.scale(),
                source: m.source,
            })
        })
        .transpose()
    }

    /// Insert a chart-of-accounts row.
    ///
    /// # Errors
    /// Returns [`RepoError::Db`] if acquiring a connection or performing the
    /// scoped insert fails.
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
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Whether the tenant's posting kill-switch is held. A missing row means
    /// unlocked. Posting uses the runner-bound sibling to pin this value in
    /// each serializable attempt; this wrapper serves standalone readers.
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
            .map_err(|e| crate::infra::posting::retry::db_to_repo(e, self.db.db().backend()))?;
        self.is_tenant_posting_locked_in(&conn, scope, tenant_id)
            .await
    }

    /// Read on the caller's snapshot, preserving access scope and contention.
    /// # Errors
    /// Returns a repository error on storage or scope failure.
    pub async fn is_tenant_posting_locked_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<bool, RepoError> {
        let row = tenant_posting_lock::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(tenant_posting_lock::Column::TenantId.eq(tenant_id)))
            .one(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        Ok(row.is_some_and(|m| m.locked))
    }

    /// Read a chart-of-accounts row by id under the supplied scope.
    ///
    /// # Errors
    /// Returns [`RepoError::Db`] if acquiring a connection or reading the
    /// account row fails.
    pub async fn find_account(
        &self,
        scope: &AccessScope,
        account_id: Uuid,
    ) -> Result<Option<AccountRow>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| crate::infra::posting::retry::db_to_repo(e, self.db.db().backend()))?;
        self.find_account_in(&conn, scope, account_id).await
    }

    /// Read on the caller's snapshot, preserving access scope and contention.
    /// # Errors
    /// Returns a repository error on storage or scope failure.
    pub async fn find_account_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        account_id: Uuid,
    ) -> Result<Option<AccountRow>, RepoError> {
        let row = tenant_account::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(tenant_account::Column::AccountId.eq(account_id)))
            .one(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
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
        self.all_accounts_in(&conn, scope, tenant_id).await
    }

    /// Read the authoritative chart on the caller's operation runner.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn all_accounts_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<tenant_account::Model>, RepoError> {
        tenant_account::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(tenant_account::Column::TenantId.eq(tenant_id)))
            .all(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))
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
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
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
        self.functional_currency_in(&conn, scope, tenant_id).await
    }

    /// Read the configured functional currency on the authoritative attempt runner.
    /// # Errors
    /// Returns typed contention or storage failures.
    pub async fn functional_currency_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<String>, RepoError> {
        let row = fiscal_calendar::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(fiscal_calendar::Column::TenantId.eq(tenant_id)))
            .one(runner)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
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
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
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
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        Ok((new_id, true))
    }

    /// Insert a currency-scale registry row if absent (keyed on
    /// `(tenant_id, currency)`), running on the supplied transaction. Validates
    /// metadata before any write. Returns `true` when a new row was inserted,
    /// `false` when one already existed (no-op — the existing scale is left
    /// untouched). A first registration is refused when the tenant's journal
    /// lines visible to `txn` carry the currency, as transaction or functional
    /// money, under another scale: provisioning a scale that disagrees with
    /// posted history fails rather than re-scaling it.
    ///
    /// The check and the insert take no lock. At the default READ COMMITTED
    /// isolation they guard committed history only: a posting that resolved the
    /// ISO default scale and has not committed yet is not seen, so both can
    /// commit. A caller that must also exclude that in-flight posting runs `txn`
    /// SERIALIZABLE, as [`Self::upsert_currency_scale`], provisioning and the
    /// posting path do, so the registry-read / journal-write cycle aborts one side.
    ///
    /// # Errors
    /// [`RepoError::ScaleOutOfRange`] when the scale exceeds 28;
    /// [`RepoError::Money`] when the currency code is malformed;
    /// [`RepoError::CurrencyScaleLocked`] when posted lines carry the currency under
    /// another scale; [`RepoError::Conflict`] on a concurrent first registration or
    /// classified database contention; [`RepoError::Db`] on any other storage failure.
    pub async fn insert_currency_scale_if_absent_txn(
        &self,
        txn: &DbTx<'_>,
        row: CurrencyScaleRow,
    ) -> Result<bool, RepoError> {
        if row.currency_scale > 28 {
            return Err(RepoError::ScaleOutOfRange(row.currency));
        }
        CurrencySpec::try_new(row.currency.clone(), row.currency_scale)?;
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
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
        if existing.is_some() {
            return Ok(false);
        }

        if currency_scale_is_locked(txn, &scope, &row, None)
            .await
            .map_err(|error| {
                crate::infra::posting::retry::db_to_repo(error, self.db.db().backend())
            })?
        {
            return Err(RepoError::CurrencyScaleLocked(row.currency));
        }
        let am = currency_scale_registry::ActiveModel {
            tenant_id: Set(row.tenant_id),
            currency: Set(row.currency),
            currency_scale: Set(i16::from(row.currency_scale)),
            source: Set(row.source),
        };
        currency_scale_registry::Entity::insert(am.clone())
            .secure()
            .scope_with_model(&scope, &am)
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| crate::infra::posting::retry::insert_to_repo(e, self.db.db().backend()))?;
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
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
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
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
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
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
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
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| crate::infra::posting::retry::scope_to_repo(e, self.db.db().backend()))?;
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

    if currency_scale_is_locked(txn, &scope, &row, existing.map(|r| r.currency_scale)).await? {
        return Ok(ScaleUpsertResult::Locked);
    }

    let am = currency_scale_registry::ActiveModel {
        tenant_id: Set(row.tenant_id),
        currency: Set(row.currency.clone()),
        currency_scale: Set(i16::from(row.currency_scale)),
        source: Set(row.source.clone()),
    };
    let on_conflict = OnConflict::columns([
        currency_scale_registry::Column::TenantId,
        currency_scale_registry::Column::Currency,
    ])
    .update_columns([
        currency_scale_registry::Column::CurrencyScale,
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

/// Check both historical money columns, even before the first explicit registry row.
async fn currency_scale_is_locked(
    txn: &DbTx<'_>,
    scope: &AccessScope,
    row: &CurrencyScaleRow,
    existing_scale: Option<i16>,
) -> Result<bool, DbError> {
    let new_scale = i16::from(row.currency_scale);
    let mut transaction =
        Condition::all().add(journal_line::Column::Currency.eq(row.currency.clone()));
    let mut functional =
        Condition::all().add(journal_line::Column::FunctionalCurrency.eq(row.currency.clone()));
    if existing_scale.is_none_or(|scale| scale == new_scale) {
        transaction = transaction.add(journal_line::Column::CurrencyScale.ne(new_scale));
        functional = functional.add(
            Condition::any()
                .add(journal_line::Column::FunctionalCurrencyScale.ne(new_scale))
                .add(journal_line::Column::FunctionalCurrencyScale.is_null()),
        );
    }
    Ok(journal_line::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(journal_line::Column::TenantId.eq(row.tenant_id))
                .add(Condition::any().add(transaction).add(functional)),
        )
        .one(txn)
        .await
        .map_err(scope_to_db)?
        .is_some())
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

#[cfg(test)]
#[path = "reference_repo_scale_lock_tests.rs"]
mod scale_lock_tests;
