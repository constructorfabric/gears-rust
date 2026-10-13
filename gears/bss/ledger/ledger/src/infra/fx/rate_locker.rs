//! Lock exact FX evidence and stamp functional money on a caller-owned posting attempt.
//! Identity requires both currency code and stored scale. New-post registry admission
//! remains the posting caller's responsibility; historical evidence is never relabelled.

use crate::domain::fx::translate::{FxLine, FxTranslateError, ensure_same_spec, translate_entry};
use crate::domain::{error::DomainError, exact_money::ExactError, model::NewLine};
use crate::infra::fx::rate_source::{RateSource, ResolvedRate};
use crate::infra::posting::retry::AttemptError;
use crate::infra::storage::repo::{FxRepo, NewRateSnapshot};
use bss_ledger_sdk::{AccountClass, CurrencySpec, PostedMoney};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner};
use uuid::Uuid;

/// Resolves and freezes one quote and both actual specs for an entry.
#[derive(Clone)]
pub struct RateLocker {
    source: RateSource,
    repo: FxRepo,
}
impl RateLocker {
    /// Build a locker over the local reference-rate repository.
    #[must_use]
    pub fn new(source: RateSource, repo: FxRepo) -> Self {
        Self { source, repo }
    }

    /// Standalone non-posting convenience; authoritative posting uses `lock_and_stamp_in`.
    /// Failed metadata validation, translation or persistence never changes caller lines.
    ///
    /// # Errors
    /// A metadata [`DomainError`] from `validate_metadata` when a line disagrees with the
    /// transaction / functional specs; [`DomainError::FxRateUnavailable`] /
    /// [`DomainError::FxRateStaleNotAllowed`] propagated from [`RateSource::resolve`]; the
    /// translation error when the pure translation rejects the input; the posting-transport
    /// [`DomainError`] (an infrastructure fault or database contention) when the snapshot
    /// insert fails.
    pub async fn lock_and_stamp(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        lines: &mut [NewLine],
        transaction: &CurrencySpec,
        functional: &CurrencySpec,
        now: OffsetDateTime,
    ) -> Result<Option<Uuid>, DomainError> {
        validate_metadata(lines, transaction, functional)?;
        if transaction == functional {
            return Ok(None);
        }
        let resolved = self
            .source
            .resolve(scope, tenant, transaction.code(), functional.code(), now)
            .await?;
        let translated = translated(lines, &resolved, functional)?;
        let id = self
            .repo
            .insert_snapshot(scope, &snapshot(tenant, transaction, functional, &resolved))
            .await
            .map_err(crate::infra::posting::error_transport::repo_to_domain)?;
        stamp(lines, translated);
        Ok(Some(id))
    }

    /// Read, translate and insert immutable evidence on the caller's runner.
    /// The caller owns the sole retry and commits snapshot, journal and sidecars together.
    /// All lines are validated before identity; no lines change on any returned error.
    pub(crate) async fn lock_and_stamp_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        lines: &mut [NewLine],
        transaction: &CurrencySpec,
        functional: &CurrencySpec,
        now: OffsetDateTime,
    ) -> Result<Option<Uuid>, AttemptError> {
        validate_metadata(lines, transaction, functional)?;
        if transaction == functional {
            return Ok(None);
        }
        let resolved = self
            .source
            .resolve_in(
                runner,
                scope,
                tenant,
                transaction.code(),
                functional.code(),
                now,
            )
            .await?;
        let translated = translated(lines, &resolved, functional)?;
        let id = self
            .repo
            .insert_snapshot_in(
                runner,
                scope,
                &snapshot(tenant, transaction, functional, &resolved),
            )
            .await?;
        stamp(lines, translated);
        Ok(Some(id))
    }
}

/// Verify every stored input spec, including zero/identity and existing functional evidence.
fn validate_metadata(
    lines: &[NewLine],
    transaction: &CurrencySpec,
    functional: &CurrencySpec,
) -> Result<(), DomainError> {
    for line in lines {
        ensure_same_spec(line.money.currency(), transaction).map_err(map_exact_err)?;
        if let Some(existing) = &line.functional_money {
            ensure_same_spec(existing.currency(), functional).map_err(map_exact_err)?;
        }
    }
    Ok(())
}
/// Calculate all values before any persistence or in-place stamping.
fn translated(
    lines: &[NewLine],
    resolved: &ResolvedRate,
    functional: &CurrencySpec,
) -> Result<Vec<PostedMoney>, DomainError> {
    let inputs: Vec<_> = lines
        .iter()
        .map(|line| FxLine {
            amount: line.money.clone(),
            side: line.side,
        })
        .collect();
    let anchor = lines
        .iter()
        .position(|line| line.account_class == AccountClass::Ar)
        .unwrap_or(0);
    let result = translate_entry(&inputs, resolved.rate, functional.clone(), anchor)
        .map_err(map_translate_err)?;
    // Existing evidence may be repeated, but never silently replaced by a new value.
    for (line, value) in lines.iter().zip(&result) {
        if line
            .functional_money
            .as_ref()
            .is_some_and(|existing| existing != value)
        {
            return Err(DomainError::InvalidRequest(
                "existing functional evidence differs from locked translation".into(),
            ));
        }
    }
    Ok(result)
}
/// Freeze complete quote provenance and both stored specs.
fn snapshot(
    tenant: Uuid,
    transaction: &CurrencySpec,
    functional: &CurrencySpec,
    resolved: &ResolvedRate,
) -> NewRateSnapshot {
    NewRateSnapshot {
        tenant_id: tenant,
        base_currency: transaction.clone(),
        quote_currency: functional.clone(),
        rate: resolved.rate,
        as_of: resolved.as_of,
        provider: resolved.provider.clone(),
        stale: resolved.stale,
        fallback_order: resolved.fallback_order,
        triangulated_via: resolved.triangulated_via.clone(),
    }
}
/// Infallible final stamping in original line order.
fn stamp(lines: &mut [NewLine], translated: Vec<PostedMoney>) {
    for (line, value) in lines.iter_mut().zip(translated) {
        line.functional_money = Some(value);
    }
}
/// The gear's one exact-arithmetic table, so an FX overflow reports the same
/// category as every other money path (no DB-transport round trip).
fn map_exact_err(error: ExactError) -> DomainError {
    crate::domain::exact_money::map_exact_error(error)
}
/// Keep numeric failures distinct from invalid entry shape/anchor policy.
fn map_translate_err(error: FxTranslateError) -> DomainError {
    match error {
        FxTranslateError::Exact(error) => map_exact_err(error),
        other => DomainError::Internal(format!("FX translation rejected the entry: {other}")),
    }
}
#[cfg(test)]
#[path = "rate_locker_tests.rs"]
mod rate_locker_tests;
