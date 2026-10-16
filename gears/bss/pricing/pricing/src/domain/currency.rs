//! The currencies pricing knows and the scale of each.
use bss_money::CurrencySpec;
use std::collections::BTreeMap;

/// The scales pricing resolves a currency code to: the ISO 4217 table, plus the deployment's own
/// scales for codes that table does not list.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrencyScales {
    overrides: BTreeMap<String, u8>,
    /// The list [`Self::known`] returns, built once from the two tables.
    known: Vec<KnownCurrency>,
}
impl Default for CurrencyScales {
    /// The ISO 4217 table alone.
    fn default() -> Self {
        Self::holding(BTreeMap::new())
    }
}
/// Why the deployment's table of currency scales was refused. Each refusal names the key, so an
/// operator knows which entry to fix.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CurrencyScalesError {
    /// The key is not spelled as a currency code.
    #[error("currency_scales key {code:?} is not a currency code: three uppercase letters")]
    NotACode {
        /// The refused key.
        code: String,
    },
    /// The key is an ISO 4217 code: its scale comes from ISO 4217 and is never overridden.
    #[error(
        "currency_scales key {code} is an ISO 4217 code: its scale comes from ISO 4217 and is \
         never listed here"
    )]
    IsoCode {
        /// The refused key.
        code: String,
    },
    /// The scale is above 28.
    #[error("currency_scales key {code} has scale {scale}: a scale is at most 28")]
    ScaleOutOfRange {
        /// The key whose scale is refused.
        code: String,
        /// The refused scale.
        scale: u8,
    },
}
impl CurrencyScalesError {
    /// The stable code of every refusal of the table.
    pub const CODE: &'static str = "CURRENCY_SCALES_INVALID";
}
impl CurrencyScales {
    /// Hold `overrides`, the scales of non-ISO codes. Every entry is judged.
    /// # Errors
    /// The first refused entry in key order: [`CurrencyScalesError::NotACode`] for a key that is
    /// not three uppercase letters, [`CurrencyScalesError::IsoCode`] for an ISO 4217 code, and
    /// [`CurrencyScalesError::ScaleOutOfRange`] for a scale above 28.
    pub fn try_new(overrides: BTreeMap<String, u8>) -> Result<Self, CurrencyScalesError> {
        for (code, scale) in &overrides {
            if !super::book::currency_code(code) {
                return Err(CurrencyScalesError::NotACode { code: code.clone() });
            }
            if bss_money::iso_scale(code).is_some() {
                return Err(CurrencyScalesError::IsoCode { code: code.clone() });
            }
            if CurrencySpec::try_new(code.clone(), *scale).is_err() {
                return Err(CurrencyScalesError::ScaleOutOfRange {
                    code: code.clone(),
                    scale: *scale,
                });
            }
        }
        Ok(Self::holding(overrides))
    }
    /// Builds the table from `overrides`, which the caller has already checked.
    fn holding(overrides: BTreeMap<String, u8>) -> Self {
        let iso = bss_money::iso_codes().map(|(code, scale)| (code, scale, true));
        let own = overrides
            .iter()
            .map(|(code, scale)| (code.as_str(), *scale, false));
        let mut known: Vec<KnownCurrency> = iso
            .chain(own)
            .filter_map(|(code, scale, iso)| {
                let spec = CurrencySpec::try_new(code.to_owned(), scale).ok()?;
                Some(KnownCurrency { spec, iso })
            })
            .collect();
        known.sort_by(|a, b| a.spec.code().cmp(b.spec.code()));
        Self { overrides, known }
    }
    /// The ISO scale of `code`, else the override, as a validated spec; `None` when unknown.
    #[must_use]
    pub fn resolve(&self, code: &str) -> Option<CurrencySpec> {
        let scale = bss_money::iso_scale(code).or_else(|| self.overrides.get(code).copied())?;
        CurrencySpec::try_new(code.to_owned(), scale).ok()
    }
    /// Every code that [`Self::resolve`] accepts, with its spec, once each and sorted by code:
    /// the ISO 4217 codes and the deployment's own (D-537).
    #[must_use]
    pub fn known(&self) -> &[KnownCurrency] {
        &self.known
    }
}
/// One currency the table accepts (D-537).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownCurrency {
    /// The code, and the number of decimal places a new price in it uses.
    pub spec: CurrencySpec,
    /// True when ISO 4217 defines the scale; false when the deployment's `currency_scales`
    /// config defines it.
    pub iso: bool,
}
#[cfg(test)]
#[path = "currency_tests.rs"]
mod tests;
