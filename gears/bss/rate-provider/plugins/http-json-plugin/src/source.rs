//! `HttpJsonRateProvider` — config-driven GET-JSON fetch + field mapping. Direct
//! pairs only — a requested pair whose base is not the feed base is omitted.

use std::borrow::Cow;

use async_trait::async_trait;
use bss_ledger_sdk::{CurrencyPair, ProviderRate, RateProviderError, RateProviderV1};
use bss_rate_provider_sdk::conversion::rate_to_micro;
use bss_rate_provider_sdk::fetch::fetch_and_parse;
use bss_rate_provider_sdk::metrics::SharedFetchMetrics;
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use toolkit_http::HttpClient;
use toolkit_security::SecurityContext;

use crate::config::{AuthKind, Mapping};

/// Custom header used for [`AuthKind::HeaderKey`].
const API_KEY_HEADER: &str = "X-API-Key";

/// Walk a dotted path (`a.b.c`) through nested JSON objects.
#[must_use]
pub fn json_lookup<'a>(value: &'a Value, dotted: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in dotted.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

/// Apply the mapping to a parsed JSON document.
///
/// An entry that fails mapping is skipped (counted via a warning), never
/// fabricated. A syntactically valid document from which ZERO entries map is a
/// [`RateProviderError::Internal`] (returning `Ok([])` would read as success and
/// suppress the composite fallback).
///
/// # Errors
/// [`RateProviderError::Internal`] if the `rates`/`as_of` paths are absent or no
/// entry maps.
pub fn map_json_document(
    body: &Value,
    mapping: &Mapping,
) -> Result<Vec<ProviderRate>, RateProviderError> {
    let as_of_raw = json_lookup(body, &mapping.as_of)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RateProviderError::Internal(format!("as_of path '{}' missing", mapping.as_of))
        })?;
    let as_of: DateTime<Utc> = as_of_raw.parse().map_err(|e| {
        RateProviderError::Internal(format!("as_of '{as_of_raw}' not RFC3339: {e}"))
    })?;
    let rates_obj = json_lookup(body, &mapping.rates)
        .and_then(Value::as_object)
        .ok_or_else(|| {
            RateProviderError::Internal(format!(
                "rates path '{}' missing or not an object",
                mapping.rates
            ))
        })?;

    let mut out = Vec::new();
    let mut skipped: u64 = 0;
    for (quote, entry) in rates_obj {
        let Some(rate_val) = entry.get(&mapping.rate) else {
            skipped += 1;
            continue;
        };
        let rate_str: Cow<'_, str> = match rate_val {
            Value::String(s) => Cow::Borrowed(s.as_str()),
            Value::Number(n) => Cow::Owned(n.to_string()),
            _ => {
                skipped += 1;
                continue;
            }
        };
        match rate_to_micro(&rate_str) {
            Ok(rate_micro) => out.push(ProviderRate {
                base: mapping.base.clone(),
                quote: quote.clone(),
                rate_micro,
                as_of,
            }),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            "bss-rate-provider-http-json: skipped unmappable entries"
        );
    }
    if out.is_empty() {
        return Err(RateProviderError::Internal(
            "http-json document produced zero mappable rates".to_owned(),
        ));
    }
    Ok(out)
}

/// The generic http-json source.
pub struct HttpJsonRateProvider {
    id: String,
    base_url: String,
    client: HttpClient,
    api_key: Option<SecretString>,
    auth: AuthKind,
    mapping: Mapping,
    metrics: SharedFetchMetrics,
}

impl HttpJsonRateProvider {
    /// Build the source. `mapping` is required for this kind.
    #[must_use]
    pub fn new(
        id: String,
        base_url: String,
        client: HttpClient,
        api_key: Option<SecretString>,
        auth: AuthKind,
        mapping: Mapping,
        metrics: SharedFetchMetrics,
    ) -> Self {
        Self {
            id,
            base_url,
            client,
            api_key,
            auth,
            mapping,
            metrics,
        }
    }
}

#[async_trait]
impl RateProviderV1 for HttpJsonRateProvider {
    fn provider_id(&self) -> &str {
        &self.id
    }

    async fn fetch_latest(
        &self,
        _ctx: &SecurityContext,
        pairs: &[CurrencyPair],
        _request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError> {
        let mut request = self.client.get(&self.base_url);
        if let Some(key) = &self.api_key {
            request = match self.auth {
                AuthKind::None => request,
                AuthKind::Bearer => {
                    request.header("Authorization", &format!("Bearer {}", key.expose_secret()))
                }
                AuthKind::HeaderKey => request.header(API_KEY_HEADER, key.expose_secret()),
            };
        }
        fetch_and_parse(request, &self.id, self.metrics.as_ref(), |bytes| {
            let body: Value = serde_json::from_slice(bytes)
                .map_err(|e| RateProviderError::Internal(format!("invalid JSON: {e}")))?;
            let mut rates = map_json_document(&body, &self.mapping)?;
            let as_of_unix = rates.first().map_or(0, |r| r.as_of.timestamp());
            if !pairs.is_empty() {
                rates.retain(|r| pairs.iter().any(|p| p.base == r.base && p.quote == r.quote));
            }
            Ok((rates, as_of_unix))
        })
        .await
    }
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
