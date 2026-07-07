//! Resolves the minor-unit scale for a (tenant, currency): registry row
//! first (tenant overrides + non-ISO codes), then the ISO-4217 default;
//! a non-ISO currency with no row is an error (no implicit scale).

use toolkit_db::secure::AccessScope;
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
    /// for a non-ISO currency with no registry row; [`ScaleError::CorruptStoredScale`]
    /// when a registry row's stored scale is itself out of range.
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
            .map_err(|e| ScaleError::Repo(e.to_string()))?
        {
            return u8::try_from(row.minor_units).map_err(|_| ScaleError::CorruptStoredScale {
                currency: currency.to_owned(),
                minor_units: row.minor_units,
            });
        }
        iso_default_scale(currency)
            .ok_or_else(|| ScaleError::UnknownCurrencyScale(currency.to_owned()))
    }
}
