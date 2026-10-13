//! Exact canonical reference quotes and immutable currency-aware lock snapshots.
//! Repository operations never retry; callers own the whole transaction attempt.

use crate::domain::model::RepoError;
use crate::infra::posting::retry::{insert_to_repo, scope_to_repo};
use crate::infra::storage::{
    entity::{fx_rate, fx_rate_snapshot},
    money_text::{decode_currency, decode_rate},
};
use bss_ledger_sdk::{CurrencySpec, MoneyError, canonical_decimal, parse_decimal};
use rust_decimal::Decimal;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureOnConflict,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

/// Immutable quote evidence; currency scales describe posted units, not quote precision.
#[derive(Clone, Debug)]
pub struct NewRateSnapshot {
    pub tenant_id: Uuid,
    pub base_currency: CurrencySpec,
    pub quote_currency: CurrencySpec,
    pub rate: Decimal,
    pub as_of: OffsetDateTime,
    pub provider: String,
    pub stale: bool,
    pub fallback_order: i32,
    pub triangulated_via: Option<String>,
}

/// Mutable reference quote, retaining the established tenant/pair/provider key.
#[derive(Clone, Debug)]
pub struct NewFxRate {
    pub tenant_id: Uuid,
    pub base_currency: String,
    pub quote_currency: String,
    pub provider: String,
    pub rate: Decimal,
    pub as_of: OffsetDateTime,
    pub fallback_order: i32,
}

/// Validated reference quote; provider ordering and staleness remain resolver policy.
#[derive(Clone, Debug)]
pub struct FxRateRow {
    pub tenant_id: Uuid,
    pub base_currency: String,
    pub quote_currency: String,
    pub provider: String,
    pub rate: Decimal,
    pub as_of: OffsetDateTime,
    pub fallback_order: i32,
    pub updated_at: OffsetDateTime,
}

/// Validated frozen evidence restored from its own historical metadata.
#[derive(Clone, Debug)]
pub struct RateSnapshotRow {
    pub rate_id: Uuid,
    pub quote: NewRateSnapshot,
}

/// Validate a caller quote without rounding or a currency-scale precision cap.
fn encode_rate(rate: Decimal) -> Result<String, RepoError> {
    let text = canonical_decimal(rate);
    parse_decimal(&text)?;
    if rate <= Decimal::ZERO {
        return Err(RepoError::Money(MoneyError::InvalidDecimal));
    }
    Ok(text)
}

/// Decode all snapshot monetary evidence, including both historical specs.
fn snapshot_row(row: fx_rate_snapshot::Model) -> Result<RateSnapshotRow, RepoError> {
    Ok(RateSnapshotRow {
        rate_id: row.rate_id,
        quote: NewRateSnapshot {
            tenant_id: row.tenant_id,
            base_currency: decode_currency(&row.base_currency, row.base_currency_scale)?,
            quote_currency: decode_currency(&row.quote_currency, row.quote_currency_scale)?,
            rate: decode_rate(&row.rate)?,
            as_of: row.as_of,
            provider: row.provider,
            stale: row.stale,
            fallback_order: row.fallback_order,
            triangulated_via: row.triangulated_via,
        },
    })
}

/// SeaORM-backed quote persistence, using caller runners for posting transactions.
#[derive(Clone)]
pub struct FxRepo {
    db: DBProvider<DbError>,
}
impl FxRepo {
    /// Bind the backend-aware storage adapters.
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Standalone reference sync; preserves the latest-known overwrite policy.
    /// # Errors
    /// Invalid quotes return Money; scope/storage failures remain repository errors.
    pub async fn upsert_rate(&self, rate: &NewFxRate) -> Result<(), RepoError> {
        let text = encode_rate(rate.rate)?;
        CurrencySpec::try_new(rate.base_currency.clone(), 0)?;
        CurrencySpec::try_new(rate.quote_currency.clone(), 0)?;
        let conn = self.db.conn().map_err(|e| RepoError::Db(e.to_string()))?;
        let scope = AccessScope::for_tenant(rate.tenant_id);
        let am = fx_rate::ActiveModel {
            tenant_id: Set(rate.tenant_id),
            base_currency: Set(rate.base_currency.clone()),
            quote_currency: Set(rate.quote_currency.clone()),
            provider: Set(rate.provider.clone()),
            rate: Set(text),
            as_of: Set(rate.as_of),
            fallback_order: Set(rate.fallback_order),
            updated_at: Set(OffsetDateTime::now_utc()),
        };
        let conflict = SecureOnConflict::<fx_rate::Entity>::columns([
            fx_rate::Column::TenantId,
            fx_rate::Column::BaseCurrency,
            fx_rate::Column::QuoteCurrency,
            fx_rate::Column::Provider,
        ])
        .update_columns([
            fx_rate::Column::Rate,
            fx_rate::Column::AsOf,
            fx_rate::Column::FallbackOrder,
            fx_rate::Column::UpdatedAt,
        ])
        .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        fx_rate::Entity::insert(am.clone())
            .secure()
            .scope_with_model(&scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .on_conflict(conflict)
            .exec(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Standalone candidate read; selection remains the resolver's job.
    /// # Errors
    /// Corrupt stored rates are InvalidStoredMoney, never default zero.
    pub async fn latest_rates(
        &self,
        tenant: Uuid,
        base: &str,
        quote: &str,
    ) -> Result<Vec<FxRateRow>, RepoError> {
        let conn = self.db.conn().map_err(|e| RepoError::Db(e.to_string()))?;
        self.latest_rates_in(&conn, &AccessScope::for_tenant(tenant), tenant, base, quote)
            .await
    }

    /// Read candidates on the caller's runner inside the whole locking attempt.
    /// # Errors
    /// Scope, driver contention and corrupt historical rates remain distinct.
    pub async fn latest_rates_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        base: &str,
        quote: &str,
    ) -> Result<Vec<FxRateRow>, RepoError> {
        let rows = fx_rate::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(fx_rate::Column::TenantId.eq(tenant))
                    .add(fx_rate::Column::BaseCurrency.eq(base))
                    .add(fx_rate::Column::QuoteCurrency.eq(quote)),
            )
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        rows.into_iter()
            .map(|row| {
                decode_currency(&row.base_currency, 0)?;
                decode_currency(&row.quote_currency, 0)?;
                Ok(FxRateRow {
                    tenant_id: row.tenant_id,
                    base_currency: row.base_currency,
                    quote_currency: row.quote_currency,
                    provider: row.provider,
                    rate: decode_rate(&row.rate)?,
                    as_of: row.as_of,
                    fallback_order: row.fallback_order,
                    updated_at: row.updated_at,
                })
            })
            .collect()
    }

    /// Standalone freeze without retries; posting callers use insert_snapshot_in.
    /// # Errors
    /// Concurrent identity insertion is Conflict and requires whole-attempt recovery.
    pub async fn insert_snapshot(
        &self,
        scope: &AccessScope,
        snap: &NewRateSnapshot,
    ) -> Result<Uuid, RepoError> {
        let conn = self.db.conn().map_err(|e| RepoError::Db(e.to_string()))?;
        self.insert_snapshot_in(&conn, scope, snap).await
    }

    /// Freeze or reuse the same canonical quote and both currency specs.
    /// Existing evidence is decoded before reuse. A concurrent unique insert aborts
    /// the attempt; never catch it and continue a damaged PostgreSQL transaction.
    /// # Errors
    /// Invalid input, corrupt evidence, scope/storage failures or typed Conflict.
    pub async fn insert_snapshot_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        snap: &NewRateSnapshot,
    ) -> Result<Uuid, RepoError> {
        let rate = encode_rate(snap.rate)?;
        let existing = fx_rate_snapshot::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(fx_rate_snapshot::Column::TenantId.eq(snap.tenant_id))
                    .add(fx_rate_snapshot::Column::BaseCurrency.eq(snap.base_currency.code()))
                    .add(
                        fx_rate_snapshot::Column::BaseCurrencyScale
                            .eq(i16::from(snap.base_currency.scale())),
                    )
                    .add(fx_rate_snapshot::Column::QuoteCurrency.eq(snap.quote_currency.code()))
                    .add(
                        fx_rate_snapshot::Column::QuoteCurrencyScale
                            .eq(i16::from(snap.quote_currency.scale())),
                    )
                    .add(fx_rate_snapshot::Column::Provider.eq(snap.provider.clone()))
                    .add(fx_rate_snapshot::Column::AsOf.eq(snap.as_of))
                    .add(fx_rate_snapshot::Column::FallbackOrder.eq(snap.fallback_order))
                    .add(fx_rate_snapshot::Column::Rate.eq(rate.clone())),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        if let Some(row) = existing {
            return Ok(snapshot_row(row)?.rate_id);
        }
        let rate_id = Uuid::now_v7();
        let am = fx_rate_snapshot::ActiveModel {
            tenant_id: Set(snap.tenant_id),
            rate_id: Set(rate_id),
            base_currency: Set(snap.base_currency.code().to_owned()),
            base_currency_scale: Set(i16::from(snap.base_currency.scale())),
            quote_currency: Set(snap.quote_currency.code().to_owned()),
            quote_currency_scale: Set(i16::from(snap.quote_currency.scale())),
            rate: Set(rate),
            as_of: Set(snap.as_of),
            provider: Set(snap.provider.clone()),
            stale: Set(snap.stale),
            fallback_order: Set(snap.fallback_order),
            triangulated_via: Set(snap.triangulated_via.clone()),
        };
        fx_rate_snapshot::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(runner)
            .await
            .map_err(|e| insert_to_repo(e, self.db.db().backend()))?;
        Ok(rate_id)
    }

    /// Standalone scoped audit read with historical currency metadata.
    /// # Errors
    /// Invalid stored quotes or specs return InvalidStoredMoney.
    pub async fn read_snapshot(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        rate_id: Uuid,
    ) -> Result<Option<RateSnapshotRow>, RepoError> {
        let conn = self.db.conn().map_err(|e| RepoError::Db(e.to_string()))?;
        self.read_snapshot_in(&conn, scope, tenant, rate_id).await
    }

    /// Audit read on the supplied runner; foreign tenants produce no row.
    /// # Errors
    /// Invalid stored quotes or specs return InvalidStoredMoney.
    pub async fn read_snapshot_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        rate_id: Uuid,
    ) -> Result<Option<RateSnapshotRow>, RepoError> {
        fx_rate_snapshot::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(fx_rate_snapshot::Column::TenantId.eq(tenant))
                    .add(fx_rate_snapshot::Column::RateId.eq(rate_id)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(snapshot_row)
            .transpose()
    }
}

#[cfg(test)]
#[path = "fx_repo_tests.rs"]
mod tests;
