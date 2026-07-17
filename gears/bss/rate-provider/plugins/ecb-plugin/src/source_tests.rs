//! ECB parser + mapping tests over the daily-XML fixture.

use bss_ledger_sdk::CurrencyPair;
use chrono::{Datelike, TimeZone, Utc};

use super::{ecb_rates_to_provider_rates, parse_ecb_xml};

const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/eurofxref-daily.xml");

#[test]
fn parses_date_and_all_pairs() {
    let (date, raw) = parse_ecb_xml(FIXTURE).unwrap();
    assert_eq!((date.year(), date.month(), date.day()), (2026, 7, 21));
    assert_eq!(raw.len(), 3);
    assert!(raw.iter().any(|(c, r)| c == "USD" && r == "1.0856"));
}

#[test]
fn whole_table_when_no_pairs_requested() {
    let (date, raw) = parse_ecb_xml(FIXTURE).unwrap();
    let rates = ecb_rates_to_provider_rates(date, &raw, &[]).unwrap();
    assert_eq!(rates.len(), 3);
    let usd = rates.iter().find(|r| r.quote == "USD").unwrap();
    assert_eq!(usd.base, "EUR");
    assert_eq!(usd.rate_micro, 1_085_600);
    assert_eq!(
        usd.as_of,
        Utc.with_ymd_and_hms(2026, 7, 21, 0, 0, 0).unwrap()
    );
}

#[test]
fn requested_pair_filters_and_omits_unavailable() {
    let (date, raw) = parse_ecb_xml(FIXTURE).unwrap();
    let want = vec![
        CurrencyPair {
            base: "EUR".to_owned(),
            quote: "USD".to_owned(),
        },
        CurrencyPair {
            base: "EUR".to_owned(),
            quote: "CHF".to_owned(),
        },
    ];
    let rates = ecb_rates_to_provider_rates(date, &raw, &want).unwrap();
    assert_eq!(rates.len(), 1);
    assert_eq!(rates[0].quote, "USD");
}

#[test]
fn garbage_is_internal_error() {
    assert!(parse_ecb_xml(b"not xml").is_err() || parse_ecb_xml(b"<a></a>").is_err());
}

#[test]
fn case_insensitive_pair_request_still_matches() {
    // ECB's own codes are uppercase; a caller that didn't normalize casing
    // (e.g. "usd") must still match the published pair, not be treated as
    // unpublished.
    let (date, raw) = parse_ecb_xml(FIXTURE).unwrap();
    let want = vec![CurrencyPair {
        base: "eur".to_owned(),
        quote: "usd".to_owned(),
    }];
    let rates = ecb_rates_to_provider_rates(date, &raw, &want).unwrap();
    assert_eq!(rates.len(), 1);
    assert_eq!(rates[0].quote, "USD");
}

#[test]
fn reversed_pair_is_omitted_not_inverted() {
    // ECB only publishes EUR->X. Requesting the inverse leg (X->EUR, as a
    // non-EUR-functional tenant's ledger would need) must be omitted here,
    // never synthesized — that inversion is the ledger's triangulation job.
    let (date, raw) = parse_ecb_xml(FIXTURE).unwrap();
    let want = vec![CurrencyPair {
        base: "USD".to_owned(),
        quote: "EUR".to_owned(),
    }];
    let rates = ecb_rates_to_provider_rates(date, &raw, &want).unwrap();
    assert!(rates.is_empty());
}

#[test]
fn refetching_the_identical_document_is_deterministic() {
    // A non-publication day (weekend/holiday) re-serves the same document
    // unchanged; the adapter must never fabricate a fresher `as_of`.
    let (date_a, raw_a) = parse_ecb_xml(FIXTURE).unwrap();
    let (date_b, raw_b) = parse_ecb_xml(FIXTURE).unwrap();
    let rates_a = ecb_rates_to_provider_rates(date_a, &raw_a, &[]).unwrap();
    let rates_b = ecb_rates_to_provider_rates(date_b, &raw_b, &[]).unwrap();
    assert_eq!(rates_a, rates_b);
}

#[test]
fn duplicate_currency_entry_keeps_first_and_does_not_error() {
    const DUPLICATE_CURRENCY_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
  <Cube>
    <Cube time="2026-07-21">
      <Cube currency="USD" rate="1.0856"/>
      <Cube currency="USD" rate="1.2000"/>
    </Cube>
  </Cube>
</gesmes:Envelope>
"#;
    let (_date, raw) = parse_ecb_xml(DUPLICATE_CURRENCY_XML).unwrap();
    let usd_entries: Vec<_> = raw.iter().filter(|(c, _)| c == "USD").collect();
    assert_eq!(
        usd_entries.len(),
        1,
        "duplicate currency entry must be deduped, not doubled"
    );
    assert_eq!(usd_entries[0].1, "1.0856", "the first occurrence must win");
}

#[test]
fn multiple_dates_keeps_first_and_does_not_error() {
    const MULTI_DATE_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
  <Cube>
    <Cube time="2026-07-21">
      <Cube currency="USD" rate="1.0856"/>
    </Cube>
    <Cube time="2026-07-20">
      <Cube currency="JPY" rate="160.85"/>
    </Cube>
  </Cube>
</gesmes:Envelope>
"#;
    let (date, raw) = parse_ecb_xml(MULTI_DATE_XML).unwrap();
    assert_eq!(
        (date.year(), date.month(), date.day()),
        (2026, 7, 21),
        "the first date must win"
    );
    // Both currency rows still parse — only the ambiguous date is resolved by
    // "first wins"; rows are not dropped just because a later date showed up.
    assert_eq!(raw.len(), 2);
}
