//! Signed cumulative tie-out baselines, replaced in the caller's close transaction.
use std::collections::HashMap;

use bss_ledger_sdk::{MoneyError, PostedMoney};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::domain::model::RepoError;
use crate::infra::posting::retry::{insert_to_repo, scope_to_repo};
use crate::infra::storage::entity::verified_balance::{self, Column as C};
use crate::infra::storage::money_text::{decode_money, encode_amount};

/// The full signed balance to snapshot, never a delta or unsigned magnitude.
#[derive(Clone, Debug)]
pub struct BaselineRow {
    pub grain: String,
    pub grain_key: String,
    pub balance: PostedMoney,
}

/// Validated historical baseline with its mutation token and close evidence.
#[derive(Clone, Debug)]
pub struct VerifiedBalanceView {
    pub tenant_id: Uuid,
    pub grain: String,
    pub grain_key: String,
    pub balance: PostedMoney,
    pub version: i64,
    pub through_period: String,
    pub watermark_seq: i64,
    pub updated_at_utc: OffsetDateTime,
}

/// Scoped baseline persistence; the caller owns transaction rollback and retry.
#[derive(Clone)]
pub struct VerifiedBalanceRepo {
    db: DBProvider<DbError>,
}

impl VerifiedBalanceRepo {
    /// Bind backend-aware error classification to the repository.
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Read all stored baselines without consulting the live currency registry.
    /// Returns invalid-stored-money for any malformed baseline.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] when a stored balance is
    /// malformed or its grain key fails validation.
    pub async fn load_baseline<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<VerifiedBalanceView>, RepoError> {
        verified_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(C::TenantId.eq(tenant)))
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .into_iter()
            .map(decode)
            .collect()
    }

    /// Replace each complete balance through an observed-version CAS, or seed version 0.
    /// The caller must abort the whole close attempt on any error.
    ///
    /// # Errors
    /// [`RepoError::InvalidRequest`] when the watermark is negative, a row's grain key or
    /// balance fails validation, or a grain is duplicated; [`RepoError::Conflict`] when an
    /// observed baseline version changed underneath, or on classified database contention;
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::InvalidStoredMoney`] when
    /// a stored balance is malformed or its version overflows.
    pub async fn snapshot<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        tenant: Uuid,
        through_period: &str,
        watermark_seq: i64,
        rows: &[BaselineRow],
    ) -> Result<(), RepoError> {
        if watermark_seq < 0 {
            return Err(RepoError::InvalidRequest(
                "negative baseline watermark".into(),
            ));
        }
        let mut keys = std::collections::HashSet::new();
        for row in rows {
            validate_key(&row.grain, &row.grain_key, &row.balance)
                .map_err(RepoError::InvalidRequest)?;
            if !keys.insert((&row.grain, &row.grain_key)) {
                return Err(RepoError::InvalidRequest("duplicate baseline grain".into()));
            }
        }
        // One read of the tenant's stored baselines instead of one per grain; only
        // the grains being replaced are decoded, so a corrupt row still aborts
        // exactly the snapshot that would overwrite it.
        let mut stored: HashMap<(String, String), verified_balance::Model> =
            verified_balance::Entity::find()
                .secure()
                .scope_with(scope)
                .filter(Condition::all().add(C::TenantId.eq(tenant)))
                .all(txn)
                .await
                .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
                .into_iter()
                .map(|m| ((m.grain.clone(), m.grain_key.clone()), m))
                .collect();
        for row in rows {
            let previous = stored
                .remove(&(row.grain.clone(), row.grain_key.clone()))
                .map(decode)
                .transpose()?;
            if let Some(previous) = previous {
                self.replace_observed(txn, scope, &previous, row, through_period, watermark_seq)
                    .await?;
            } else {
                self.seed(txn, scope, tenant, row, through_period, watermark_seq)
                    .await?;
            }
        }
        Ok(())
    }

    /// Insert a missing business grain. A competing insert aborts this attempt.
    async fn seed<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        tenant: Uuid,
        row: &BaselineRow,
        through_period: &str,
        watermark_seq: i64,
    ) -> Result<(), RepoError> {
        let am = verified_balance::ActiveModel {
            currency: Set(row.balance.currency().code().to_owned()),
            currency_scale: Set(i16::from(row.balance.currency().scale())),
            tenant_id: Set(tenant),
            grain: Set(row.grain.clone()),
            grain_key: Set(row.grain_key.clone()),
            verified_balance: Set(encode_amount(&row.balance)),
            version: Set(0),
            through_period: Set(through_period.to_owned()),
            watermark_seq: Set(watermark_seq),
            updated_at_utc: Set(OffsetDateTime::now_utc()),
        };
        verified_balance::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| insert_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Literal replacement guarded by the version read in this transaction.
    async fn replace_observed<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        previous: &VerifiedBalanceView,
        row: &BaselineRow,
        through_period: &str,
        watermark_seq: i64,
    ) -> Result<(), RepoError> {
        if previous.balance.currency().code() != row.balance.currency().code() {
            return Err(MoneyError::CurrencyMismatch.into());
        }
        if previous.balance.currency().scale() != row.balance.currency().scale() {
            return Err(MoneyError::ScaleMismatch.into());
        }
        let next_version = previous
            .version
            .checked_add(1)
            .ok_or_else(|| RepoError::InvalidStoredMoney("baseline version overflow".into()))?;
        let changed = verified_balance::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(C::VerifiedBalance, Expr::value(encode_amount(&row.balance)))
            .col_expr(C::Version, Expr::value(next_version))
            .col_expr(C::ThroughPeriod, Expr::value(through_period))
            .col_expr(C::WatermarkSeq, Expr::value(watermark_seq))
            .col_expr(C::UpdatedAtUtc, Expr::value(OffsetDateTime::now_utc()))
            .filter(
                key(previous.tenant_id, &previous.grain, &previous.grain_key)
                    .add(C::Version.eq(previous.version)),
            )
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        if changed.rows_affected != 1 {
            return Err(RepoError::Conflict("verified baseline changed".into()));
        }
        Ok(())
    }
}

/// Preserve the existing business key; stored scale is never a key dimension.
fn key(tenant: Uuid, grain: &str, grain_key: &str) -> Condition {
    Condition::all()
        .add(C::TenantId.eq(tenant))
        .add(C::Grain.eq(grain))
        .add(C::GrainKey.eq(grain_key))
}

// The grain-key codec: the builders the tie-out writes keys with and the
// currency position the baseline decoder reads back, kept in one place so a
// layout change cannot drift between writer and reader.

/// `grain_key` of the `account_balance` grain: `account|currency`.
pub(crate) fn key_account(account_id: Uuid, currency: &str) -> String {
    format!("{account_id}|{currency}")
}
/// `grain_key` of the `(payer, account, currency)` grains (`ar_payer_balance`,
/// `unallocated_balance`): `payer|account|currency`.
pub(crate) fn key_payer_account_ccy(payer: Uuid, account: Uuid, currency: &str) -> String {
    format!("{payer}|{account}|{currency}")
}
/// `grain_key` of the `(payer, account, invoice)` grains (`ar_invoice` balance and
/// disputed): `payer|account|invoice`, no currency.
pub(crate) fn key_payer_account_invoice(payer: Uuid, account: Uuid, invoice: &str) -> String {
    format!("{payer}|{account}|{invoice}")
}
/// `grain_key` of the `tax_subbalance` grain: `account|jurisdiction|filing`, no currency.
pub(crate) fn key_tax(account: Uuid, juris: &str, filing: &str) -> String {
    format!("{account}|{juris}|{filing}")
}
/// `grain_key` of the `reusable_credit_subbalance` grain:
/// `payer|account|currency|event_type`.
pub(crate) fn key_reusable(payer: Uuid, account: Uuid, currency: &str, event_type: &str) -> String {
    format!("{payer}|{account}|{currency}|{event_type}")
}

/// Where the builders above put the currency in a grain's key: `Ok(None)` for a
/// key without one, `Err` for an unknown grain.
fn currency_position(grain: &str) -> Result<Option<usize>, String> {
    use verified_balance::{
        GRAIN_ACCOUNT, GRAIN_AR_INVOICE, GRAIN_AR_INVOICE_DISPUTED, GRAIN_AR_PAYER,
        GRAIN_REUSABLE_CREDIT, GRAIN_TAX, GRAIN_UNALLOCATED,
    };
    match grain {
        GRAIN_ACCOUNT => Ok(Some(1)),
        GRAIN_AR_PAYER | GRAIN_UNALLOCATED | GRAIN_REUSABLE_CREDIT => Ok(Some(2)),
        GRAIN_AR_INVOICE | GRAIN_AR_INVOICE_DISPUTED | GRAIN_TAX => Ok(None),
        _ => Err("unknown verified balance grain".into()),
    }
}

/// Validate known grain labels and currency where the established key encodes it.
fn validate_key(grain: &str, grain_key: &str, money: &PostedMoney) -> Result<(), String> {
    let currency_index = currency_position(grain)?;
    if grain_key.is_empty()
        || currency_index
            .is_some_and(|i| grain_key.split('|').nth(i) != Some(money.currency().code()))
    {
        return Err("baseline grain key disagrees with currency metadata".into());
    }
    Ok(())
}

/// Decode before replacing, so a snapshot cannot silently repair corrupt history.
fn decode(row: verified_balance::Model) -> Result<VerifiedBalanceView, RepoError> {
    let balance = decode_money(&row.verified_balance, &row.currency, row.currency_scale)?;
    validate_key(&row.grain, &row.grain_key, &balance).map_err(RepoError::InvalidStoredMoney)?;
    if row.version < 0 || row.watermark_seq < 0 {
        return Err(RepoError::InvalidStoredMoney(
            "negative baseline version".into(),
        ));
    }
    Ok(VerifiedBalanceView {
        tenant_id: row.tenant_id,
        grain: row.grain,
        grain_key: row.grain_key,
        balance,
        version: row.version,
        through_period: row.through_period,
        watermark_seq: row.watermark_seq,
        updated_at_utc: row.updated_at_utc,
    })
}

#[cfg(test)]
#[path = "verified_balance_repo_tests.rs"]
mod tests;
