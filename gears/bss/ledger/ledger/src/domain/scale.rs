//! ISO-4217 default minor-unit scales. A `(tenant, currency)` registry
//! row overrides these; a non-ISO currency with no row has no implicit
//! scale (resolution errors). This table is the fallback only.

/// ISO-4217 minor-unit exponent for well-known currencies. `None` means
/// "not a known ISO code" — the caller must consult the registry.
#[must_use]
pub fn iso_default_scale(currency: &str) -> Option<u8> {
    let scale = match currency {
        // exponent 0
        "JPY" | "KRW" | "CLP" | "VND" | "ISK" | "HUF" => 0,
        // exponent 3
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        // exponent 2 — the common case
        "USD" | "EUR" | "GBP" | "CHF" | "CAD" | "AUD" | "CNY" | "SEK" | "NOK" | "DKK" | "PLN"
        | "INR" | "BRL" | "ZAR" => 2,
        _ => return None,
    };
    Some(scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_iso_scales() {
        assert_eq!(iso_default_scale("USD"), Some(2));
        assert_eq!(iso_default_scale("JPY"), Some(0));
        assert_eq!(iso_default_scale("BHD"), Some(3));
    }

    #[test]
    fn unknown_currency_has_no_default() {
        assert_eq!(iso_default_scale("XBT"), None);
        assert_eq!(iso_default_scale("ZZZ"), None);
    }
}
