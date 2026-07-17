//! `EcbRateProvider` — HTTP fetch + XML parse + `rate_micro` conversion over the
//! ECB daily reference-rate feed. Direct pairs only (design O-3): a requested
//! pair the feed does not publish is omitted, never synthesized or inverted.

use std::collections::HashSet;

use async_trait::async_trait;
use bss_ledger_sdk::{CurrencyPair, ProviderRate, RateProviderError, RateProviderV1};
use bss_rate_provider_sdk::conversion::rate_to_micro;
use bss_rate_provider_sdk::error::map_http_error;
use bss_rate_provider_sdk::fetch::fetch_and_parse;
use bss_rate_provider_sdk::metrics::SharedFetchMetrics;
use chrono::{NaiveDate, NaiveTime};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use toolkit_http::HttpClient;
use toolkit_security::SecurityContext;

/// ECB publishes EUR-based pairs (EUR -> currency).
const ECB_BASE: &str = "EUR";

/// A parsed ECB feed: the publication date plus its raw `(currency, rate)` pairs.
type EcbTable = (NaiveDate, Vec<(String, String)>);

/// Parse the ECB daily XML into a publication date and raw `(currency, rate)` pairs.
///
/// The feed is expected to carry exactly one publication date and one entry
/// per currency. Neither is fatal if violated (a structurally-odd-but-valid
/// document shouldn't take the whole fetch down): a second distinct date is
/// ignored (the first date wins) and a duplicate currency entry is ignored
/// (the first occurrence wins) — both are logged so the anomaly is visible.
///
/// # Errors
/// [`RateProviderError::Internal`] on malformed XML, a missing `time` date, or an
/// empty rate table.
pub fn parse_ecb_xml(bytes: &[u8]) -> Result<EcbTable, RateProviderError> {
    let mut reader = Reader::from_reader(bytes);
    let mut buf = Vec::new();
    let mut date: Option<NaiveDate> = None;
    let mut rates: Vec<(String, String)> = Vec::new();
    let mut seen_currencies: HashSet<String> = HashSet::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| RateProviderError::Internal(format!("ECB XML parse: {e}")))?;
        match event {
            // The date lives on the outer `<Cube time="...">`; each inner
            // `<Cube currency="X" rate="Y"/>` is one EUR-based pair. Both are
            // `Cube` elements, so inspect the attributes of every `Cube`.
            Event::Empty(e) | Event::Start(e) if e.local_name().as_ref() == b"Cube" => {
                let (time, currency, rate) = read_cube_attributes(&e);
                if let Some(value) = time {
                    update_publication_date(&mut date, &value);
                }
                if let (Some(c), Some(r)) = (currency, rate) {
                    record_currency_rate(&mut seen_currencies, &mut rates, c, r);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    let date = date.ok_or_else(|| {
        RateProviderError::Internal("ECB XML missing publication date".to_owned())
    })?;
    if rates.is_empty() {
        return Err(RateProviderError::Internal(
            "ECB XML contained no rate entries".to_owned(),
        ));
    }
    Ok((date, rates))
}

/// Pull the `time`, `currency`, and `rate` attribute values off one `<Cube>`
/// element (a real ECB `Cube` only ever carries `time` XOR `currency`+`rate`,
/// but nothing stops reading all three from whichever element shows up).
fn read_cube_attributes(e: &BytesStart<'_>) -> (Option<String>, Option<String>, Option<String>) {
    let mut time = None;
    let mut currency = None;
    let mut rate = None;
    for attr in e.attributes().flatten() {
        let value = String::from_utf8_lossy(attr.value.as_ref()).into_owned();
        match attr.key.as_ref() {
            b"time" => time = Some(value),
            b"currency" => currency = Some(value),
            b"rate" => rate = Some(value),
            _ => {}
        }
    }
    (time, currency, rate)
}

/// Keep the first publication date seen. A second, different date doesn't
/// fail parsing — it's logged, see [`parse_ecb_xml`]'s doc for why.
fn update_publication_date(date: &mut Option<NaiveDate>, value: &str) {
    let Ok(parsed) = NaiveDate::parse_from_str(value, "%Y-%m-%d") else {
        return;
    };
    match *date {
        None => *date = Some(parsed),
        Some(existing) if existing != parsed => tracing::warn!(
            previous = %existing,
            found = %parsed,
            "bss-rate-provider-ecb: feed contains more than one publication date; keeping the first"
        ),
        Some(_) => {}
    }
}

/// Keep the first entry for a given currency. A duplicate doesn't fail
/// parsing — it's logged, see [`parse_ecb_xml`]'s doc for why.
fn record_currency_rate(
    seen: &mut HashSet<String>,
    rates: &mut Vec<(String, String)>,
    currency: String,
    rate: String,
) {
    if seen.insert(currency.clone()) {
        rates.push((currency, rate));
    } else {
        tracing::warn!(
            currency = %currency,
            "bss-rate-provider-ecb: duplicate currency entry in feed; keeping the first"
        );
    }
}

/// Convert parsed ECB rows into `ProviderRate`s (base = EUR). When `pairs` is
/// non-empty, keep only the requested EUR-based quotes (others omitted, not an
/// error). `as_of` is the publication date at 00:00:00 UTC.
///
/// # Errors
/// [`RateProviderError::Internal`] if a rate string fails exact-decimal conversion.
pub fn ecb_rates_to_provider_rates(
    date: NaiveDate,
    raw: &[(String, String)],
    pairs: &[CurrencyPair],
) -> Result<Vec<ProviderRate>, RateProviderError> {
    let as_of = date.and_time(NaiveTime::MIN).and_utc();
    let mut out = Vec::with_capacity(raw.len());
    for (quote, rate_str) in raw {
        // ISO 4217 codes are ASCII; compare case-insensitively so a caller
        // that didn't normalize casing (e.g. "usd" instead of "USD") still
        // matches a pair ECB actually publishes, rather than it being
        // indistinguishable from a genuinely unpublished pair.
        if !pairs.is_empty()
            && !pairs.iter().any(|p| {
                p.base.eq_ignore_ascii_case(ECB_BASE) && p.quote.eq_ignore_ascii_case(quote)
            })
        {
            continue;
        }
        let rate_micro = rate_to_micro(rate_str)?;
        out.push(ProviderRate {
            base: ECB_BASE.to_owned(),
            quote: quote.clone(),
            rate_micro,
            as_of,
        });
    }
    Ok(out)
}

/// The ECB source.
pub struct EcbRateProvider {
    id: String,
    base_url: String,
    client: HttpClient,
    metrics: SharedFetchMetrics,
}

impl EcbRateProvider {
    /// Build the source over a shared HTTP client.
    #[must_use]
    pub fn new(
        id: String,
        base_url: String,
        client: HttpClient,
        metrics: SharedFetchMetrics,
    ) -> Self {
        Self {
            id,
            base_url,
            client,
            metrics,
        }
    }
}

#[async_trait]
impl RateProviderV1 for EcbRateProvider {
    fn provider_id(&self) -> &str {
        &self.id
    }

    async fn fetch_latest(
        &self,
        _ctx: &SecurityContext,
        pairs: &[CurrencyPair],
        _request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError> {
        let request = self.client.get(&self.base_url);
        fetch_and_parse(request, &self.id, self.metrics.as_ref(), |bytes| {
            let (date, raw) = parse_ecb_xml(bytes)?;
            let rates = ecb_rates_to_provider_rates(date, &raw, pairs)?;
            let as_of_unix = date.and_time(NaiveTime::MIN).and_utc().timestamp();
            Ok((rates, as_of_unix))
        })
        .await
    }

    /// Cheap reachability probe: a HEAD request, never a full fetch+parse of
    /// the published table (the SDK's default `health()` would do exactly
    /// that). ECB's static feed serves HEAD the same as GET, minus the body.
    async fn health(
        &self,
        _ctx: &SecurityContext,
        _request_id: &str,
    ) -> Result<(), RateProviderError> {
        let resp = self
            .client
            .head(&self.base_url)
            .send()
            .await
            .map_err(|e| map_http_error(&e))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(RateProviderError::UpstreamStatus(resp.status().as_u16()))
        }
    }
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
