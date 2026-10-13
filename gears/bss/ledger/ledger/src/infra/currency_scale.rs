//! Resolves the currency posting scale for a (tenant, currency): the ISO 4217 scale from
//! `bss_money::iso_scale` first, then the registry row for a code with no ISO 4217 minor-unit
//! scale. The
//! registry never overrides an ISO scale (provisioning refuses such a row, and a stored one
//! is ignored); a non-ISO currency with no row is an error (no implicit scale).

use bss_money::iso_scale;
use toolkit_db::secure::{AccessScope, DBRunner};
use uuid::Uuid;

use crate::domain::money::ScaleError;
use crate::infra::storage::repo::ReferenceRepo;

/// Registry-backed currency-scale resolver.
pub struct CurrencyScaleResolver {
    reference: ReferenceRepo,
}

impl CurrencyScaleResolver {
    #[must_use]
    pub fn new(reference: ReferenceRepo) -> Self {
        Self { reference }
    }

    /// Resolve the scale for `(tenant_id, currency)`: the ISO-4217 scale of an ISO
    /// code (the registry is not read), else the registry row, else
    /// [`ScaleError::UnknownCurrencyScale`].
    ///
    /// # Errors
    /// [`ScaleError::Repo`] on a storage failure; [`ScaleError::UnknownCurrencyScale`]
    /// for a non-ISO currency with no registry row. Invalid stored metadata is
    /// rejected by the repository before a scale is returned.
    pub async fn resolve(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        currency: &str,
    ) -> Result<u8, ScaleError> {
        if let Some(scale) = iso_scale(currency) {
            return Ok(scale);
        }
        self.reference
            .find_currency_scale(scope, tenant_id, currency)
            .await
            .map_err(ScaleError::from)?
            .map(|row| row.currency_scale)
            .ok_or_else(|| ScaleError::UnknownCurrencyScale(currency.to_owned()))
    }

    /// Resolve authoritative scale inside the caller's transaction snapshot.
    ///
    /// Stored historical/reversal values must use their own metadata instead.
    ///
    /// # Errors
    /// Returns a repository failure or `UnknownCurrencyScale` with no implicit default.
    pub async fn resolve_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant_id: Uuid,
        currency: &str,
    ) -> Result<u8, ScaleError> {
        if let Some(scale) = iso_scale(currency) {
            return Ok(scale);
        }
        self.reference
            .find_currency_scale_in(runner, scope, tenant_id, currency)
            .await
            .map_err(ScaleError::from)?
            .map(|row| row.currency_scale)
            .ok_or_else(|| ScaleError::UnknownCurrencyScale(currency.to_owned()))
    }
}
