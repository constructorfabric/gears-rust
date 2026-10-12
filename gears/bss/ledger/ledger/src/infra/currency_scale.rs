//! Resolves the currency posting scale for a (tenant, currency): registry row
//! first (tenant overrides + non-ISO codes), then the ISO-4217 default;
//! a non-ISO currency with no row is an error (no implicit scale).

use toolkit_db::secure::{AccessScope, DBRunner};
use uuid::Uuid;

use crate::domain::money::ScaleError;
use crate::domain::scale::iso_default_scale;
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

    /// Resolve the scale for `(tenant_id, currency)`: a registry row wins,
    /// else the ISO-4217 default, else [`ScaleError::UnknownCurrencyScale`].
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
        if let Some(row) = self
            .reference
            .find_currency_scale(scope, tenant_id, currency)
            .await
            .map_err(ScaleError::from)?
        {
            return Ok(row.currency_scale);
        }
        iso_default_scale(currency)
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
        if let Some(row) = self
            .reference
            .find_currency_scale_in(runner, scope, tenant_id, currency)
            .await
            .map_err(ScaleError::from)?
        {
            return Ok(row.currency_scale);
        }
        iso_default_scale(currency)
            .ok_or_else(|| ScaleError::UnknownCurrencyScale(currency.to_owned()))
    }
}
