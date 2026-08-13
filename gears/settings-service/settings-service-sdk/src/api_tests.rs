// Created: 2026-08-25 by Constructor Tech
//! Tests for the client trait contracts.
//!
//! These pin promises the signatures make but cannot enforce: that a bulk read
//! resolves each key independently, that every outcome identifies itself, and
//! that a failed read stays a failure rather than becoming a Schema Default.
//!
//! A fake reader stands in for the gear. The point is not to test the fake --
//! it is that a conforming implementation has room to honour the contract and a
//! consumer has the means to observe it.

use std::collections::HashMap;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use super::{BulkOutcome, BulkSelector, SettingsReaderClient};
use crate::models::{EffectiveSource, EffectiveValueResponse, GetEffectiveRequest};
use crate::{SecretHandle, SettingKey};

const BOOL_TYPE: &str = "gts.cf.settings.types.bool_flag.v1~";

fn key(name: &str) -> SettingKey {
    SettingKey::parse(&format!("{BOOL_TYPE}acme.settings.network.{name}.v1")).expect("fixture key")
}

fn value_for(key: &SettingKey, source: EffectiveSource) -> EffectiveValueResponse {
    EffectiveValueResponse {
        key: key.clone(),
        scope: "tenant-a".to_owned(),
        value: serde_json::json!(true),
        source,
        source_scope: match source {
            EffectiveSource::SchemaDefault => None,
            _ => Some("tenant-a".to_owned()),
        },
    }
}

/// A reader whose per-key behaviour the test dictates.
#[derive(Default)]
struct FakeReader {
    /// Keys that fail, and the category read's key set when a category is asked for.
    failing: Vec<String>,
    category_keys: Vec<SettingKey>,
    sources: HashMap<String, EffectiveSource>,
}

impl FakeReader {
    fn outcome_for(&self, key: SettingKey) -> BulkOutcome {
        let result = if self.failing.iter().any(|f| f == key.leaf_slug()) {
            Err(CanonicalError::service_unavailable().create())
        } else {
            let source = self
                .sources
                .get(key.leaf_slug())
                .copied()
                .unwrap_or(EffectiveSource::OwnOverride);
            Ok(value_for(&key, source))
        };
        BulkOutcome { key, result }
    }
}

#[async_trait]
impl SettingsReaderClient for FakeReader {
    async fn get_effective(
        &self,
        _ctx: &SecurityContext,
        req: GetEffectiveRequest,
    ) -> Result<EffectiveValueResponse, CanonicalError> {
        self.outcome_for(req.key).result
    }

    async fn get_effective_bulk(
        &self,
        _ctx: &SecurityContext,
        selector: BulkSelector,
        _scope: String,
    ) -> Vec<BulkOutcome> {
        let keys = match selector {
            BulkSelector::Keys(keys) => keys,
            BulkSelector::Category(_) => self.category_keys.clone(),
        };
        keys.into_iter().map(|k| self.outcome_for(k)).collect()
    }

    async fn resolve_secret(
        &self,
        _ctx: &SecurityContext,
        _handle: SecretHandle,
    ) -> Result<String, CanonicalError> {
        Err(CanonicalError::service_unavailable().create())
    }
}

#[tokio::test]
async fn one_failing_key_does_not_fail_the_others() {
    // The reason the method returns outcomes rather than `Result<Vec<_>>`: a
    // consumer reading twenty settings at boot must not lose nineteen of them
    // to one that is unavailable.
    let reader = FakeReader {
        failing: vec!["proxy".to_owned()],
        ..FakeReader::default()
    };
    let outcomes = reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![key("timeout"), key("proxy"), key("retries")]),
            "tenant-a".to_owned(),
        )
        .await;

    assert_eq!(outcomes.len(), 3);
    let failed: Vec<&str> = outcomes
        .iter()
        .filter(|o| o.result.is_err())
        .map(|o| o.key.leaf_slug())
        .collect();
    assert_eq!(failed, ["proxy"], "exactly the one key must fail");
    assert_eq!(outcomes.iter().filter(|o| o.result.is_ok()).count(), 2);
}

#[tokio::test]
async fn every_outcome_names_its_own_key_on_both_sides() {
    // Positional correspondence would be enough for a success, which carries its
    // key in the response -- but a `CanonicalError` carries none. An
    // unattributable failure leaves a consumer unable to say which setting broke.
    let reader = FakeReader {
        failing: vec!["proxy".to_owned()],
        ..FakeReader::default()
    };
    let outcomes = reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![key("proxy"), key("timeout")]),
            "tenant-a".to_owned(),
        )
        .await;

    for outcome in &outcomes {
        assert!(!outcome.key.leaf_slug().is_empty());
        if let Ok(response) = &outcome.result {
            assert_eq!(
                response.key, outcome.key,
                "a success must agree with the outcome it sits in"
            );
        }
    }
}

#[tokio::test]
async fn a_category_read_identifies_keys_the_caller_never_supplied() {
    // The caller does not know a category's key set in advance, so the outcome
    // is the only place the key can come from.
    let reader = FakeReader {
        category_keys: vec![key("timeout"), key("proxy")],
        ..FakeReader::default()
    };
    let outcomes = reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Category("network".to_owned()),
            "tenant-a".to_owned(),
        )
        .await;

    let named: Vec<&str> = outcomes.iter().map(|o| o.key.leaf_slug()).collect();
    assert_eq!(named, ["timeout", "proxy"]);
}

#[tokio::test]
async fn an_empty_key_set_yields_no_outcomes() {
    let reader = FakeReader::default();
    let outcomes = reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(Vec::new()),
            "tenant-a".to_owned(),
        )
        .await;
    assert!(outcomes.is_empty());
}

#[tokio::test]
async fn a_failed_read_is_an_error_not_a_schema_default() {
    // The degradation contract. The Schema Default lives in the same database
    // as the override, so on failure it is equally unreachable -- substituting
    // it would hand the caller a value the service never resolved.
    let reader = FakeReader {
        failing: vec!["proxy".to_owned()],
        ..FakeReader::default()
    };
    let result = reader
        .get_effective(
            &SecurityContext::anonymous(),
            GetEffectiveRequest {
                key: key("proxy"),
                scope: "tenant-a".to_owned(),
            },
        )
        .await;
    assert!(result.is_err(), "a failure must not carry a value at all");
}

#[tokio::test]
async fn a_failure_is_distinguishable_from_a_genuine_schema_default() {
    // Both are "no configured value here", and a consumer must be able to tell
    // them apart: one is a setting nobody has overridden, the other is a
    // service that could not answer.
    let mut sources = HashMap::new();
    sources.insert("timeout".to_owned(), EffectiveSource::SchemaDefault);
    let reader = FakeReader {
        failing: vec!["proxy".to_owned()],
        sources,
        ..FakeReader::default()
    };
    let ctx = SecurityContext::anonymous();

    let unconfigured = reader
        .get_effective(
            &ctx,
            GetEffectiveRequest {
                key: key("timeout"),
                scope: "tenant-a".to_owned(),
            },
        )
        .await
        .expect("an unconfigured setting still resolves");
    assert!(unconfigured.source.is_unconfigured());
    assert!(unconfigured.source_scope.is_none());

    let unavailable = reader
        .get_effective(
            &ctx,
            GetEffectiveRequest {
                key: key("proxy"),
                scope: "tenant-a".to_owned(),
            },
        )
        .await;
    assert!(unavailable.is_err());
}

#[test]
fn the_reader_trait_is_object_safe() {
    // `ClientHub` hands back `dyn SettingsReaderClient`; a trait that could not
    // be made into an object would fail there rather than here.
    let _: &dyn SettingsReaderClient = &FakeReader::default();
}
