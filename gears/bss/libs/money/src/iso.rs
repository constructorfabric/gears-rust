//! ISO 4217 minor-unit scales for the active currency codes.
//!
//! One table for every BSS gear. An ISO code always takes the scale [`iso_scale`] gives.
//! A gear asks its own registry only for a code for which [`iso_scale`] gives `None`, and
//! no registry row overrides an ISO scale.

/// ISO 4217 minor-unit scale of an active currency code, or `None` for any other code.
///
/// The match is exact: a code must be three uppercase ASCII letters as ISO writes it
/// (`eur` and ` EUR` give `None`). Codes with no minor unit in ISO 4217 (`XAU`, `XAG`,
/// `XDR`, `XTS`, `XXX` and the other "N.A." entries) give `None`, as do retired and
/// non-ISO codes such as `BTC`. The caller then asks its own registry.
///
/// The list is the ISO 4217 "list one" of active codes (published 2026-09-17). Every code
/// is listed with its scale. There is no catch-all scale, so a new ISO code needs a code
/// change.
#[must_use]
pub fn iso_scale(code: &str) -> Option<u8> {
    match code {
        // Exponent 0.
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => Some(0),
        // Exponent 3.
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => Some(3),
        // Exponent 4.
        "CLF" | "UYW" => Some(4),
        // Exponent 2.
        "AED" | "AFN" | "ALL" | "AMD" | "AOA" | "ARS" | "AUD" | "AWG" | "AZN" | "BAM" | "BBD"
        | "BDT" | "BMD" | "BND" | "BOB" | "BOV" | "BRL" | "BSD" | "BTN" | "BWP" | "BYN" | "BZD"
        | "CAD" | "CDF" | "CHE" | "CHF" | "CHW" | "CNY" | "COP" | "COU" | "CRC" | "CUP" | "CVE"
        | "CZK" | "DKK" | "DOP" | "DZD" | "EGP" | "ERN" | "ETB" | "EUR" | "FJD" | "FKP" | "GBP"
        | "GEL" | "GHS" | "GIP" | "GMD" | "GTQ" | "GYD" | "HKD" | "HNL" | "HTG" | "HUF" | "IDR"
        | "ILS" | "INR" | "IRR" | "JMD" | "KES" | "KGS" | "KHR" | "KPW" | "KYD" | "KZT" | "LAK"
        | "LBP" | "LKR" | "LRD" | "LSL" | "MAD" | "MDL" | "MGA" | "MKD" | "MMK" | "MNT" | "MOP"
        | "MRU" | "MUR" | "MVR" | "MWK" | "MXN" | "MXV" | "MYR" | "MZN" | "NAD" | "NGN" | "NIO"
        | "NOK" | "NPR" | "NZD" | "PAB" | "PEN" | "PGK" | "PHP" | "PKR" | "PLN" | "QAR" | "RON"
        | "RSD" | "RUB" | "SAR" | "SBD" | "SCR" | "SDG" | "SEK" | "SGD" | "SHP" | "SLE" | "SOS"
        | "SRD" | "SSP" | "STN" | "SVC" | "SYP" | "SZL" | "THB" | "TJS" | "TMT" | "TOP" | "TRY"
        | "TTD" | "TWD" | "TZS" | "UAH" | "USD" | "USN" | "UYU" | "UZS" | "VED" | "VES" | "WST"
        | "XAD" | "XCD" | "XCG" | "YER" | "ZAR" | "ZMW" | "ZWG" => Some(2),
        _ => None,
    }
}

#[cfg(test)]
#[path = "iso_tests.rs"]
mod iso_tests;
