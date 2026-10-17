//! Tests for the personalization default read from the settings service, with
//! a stub settings client in `ClientHub`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use settings_service_sdk::models::{EffectiveValueResponse, GetEffectiveRequest};
use settings_service_sdk::precondition::SETTING_RETIRED;
use settings_service_sdk::{
    BulkOutcome, BulkSelector, EffectiveSource, SecretHandle, SecretString, SettingsReaderClient,
};
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::tenant_defaults::SettingsServiceDefaults;
use crate::domain::error::DomainError;
use crate::domain::subject_settings::TenantDefaults;
use crate::test_support::context_in;

const KEY: &str =
    "gts.cf.core.settings.setting_type.v1~cf.construct.personalization.default_enabled.v1~";

#[resource_error("gts.cf.core.settings.declaration.v1~")]
struct DeclarationError;

#[resource_error("gts.cf.core.settings.category.v1~")]
struct CategoryError;

/// Answers every read with the same outcome and records the requests.
struct StubReader {
    answer: fn() -> Result<serde_json::Value, CanonicalError>,
    asked: Mutex<Vec<GetEffectiveRequest>>,
}

#[async_trait]
impl SettingsReaderClient for StubReader {
    async fn get_effective(
        &self,
        _ctx: &SecurityContext,
        req: GetEffectiveRequest,
    ) -> Result<EffectiveValueResponse, CanonicalError> {
        self.asked.lock().unwrap().push(req.clone());
        let value = (self.answer)()?;
        Ok(EffectiveValueResponse {
            key: req.key,
            scope: req.scope,
            value,
            source: EffectiveSource::OwnOverride,
            source_scope: None,
            traits: serde_json::Value::Null,
            inheritance_trail: Vec::new(),
        })
    }

    async fn get_effective_bulk(
        &self,
        _ctx: &SecurityContext,
        _selector: BulkSelector,
        _scope: String,
    ) -> Result<Vec<BulkOutcome>, CanonicalError> {
        Err(CanonicalError::internal("not used by Construct".to_owned()).create())
    }

    async fn resolve_secret(
        &self,
        _ctx: &SecurityContext,
        _handle: SecretHandle,
    ) -> Result<SecretString, CanonicalError> {
        Err(CanonicalError::internal("not used by Construct".to_owned()).create())
    }
}

/// A hub with a stub settings client, and the stub to inspect.
fn hub_answering(
    answer: fn() -> Result<serde_json::Value, CanonicalError>,
) -> (Arc<ClientHub>, Arc<StubReader>) {
    let hub = Arc::new(ClientHub::new());
    let reader = Arc::new(StubReader {
        answer,
        asked: Mutex::new(Vec::new()),
    });
    hub.register::<dyn SettingsReaderClient>(reader.clone());
    (hub, reader)
}

async fn default_with(hub: Arc<ClientHub>, fallback: bool) -> Result<bool, DomainError> {
    SettingsServiceDefaults::new(hub, fallback)
        .expect("the key parses")
        .personalization_default(&context_in(Uuid::new_v4()), Uuid::new_v4())
        .await
}

#[tokio::test]
async fn without_a_settings_client_the_configured_default_stands() {
    for fallback in [true, false] {
        let got = default_with(Arc::new(ClientHub::new()), fallback).await;
        assert_eq!(got.expect("default"), fallback);
    }
}

#[tokio::test]
async fn the_tenant_value_wins_over_the_configured_default() {
    let (hub, _) = hub_answering(|| Ok(serde_json::Value::Bool(true)));
    assert!(default_with(hub, false).await.expect("default"));

    let (hub, _) = hub_answering(|| Ok(serde_json::Value::Bool(false)));
    assert!(!default_with(hub, true).await.expect("default"));
}

#[tokio::test]
async fn reads_the_construct_key_for_the_tenant_scope() {
    let (hub, reader) = hub_answering(|| Ok(serde_json::Value::Bool(true)));
    let tenant = Uuid::new_v4();

    SettingsServiceDefaults::new(hub, false)
        .expect("the key parses")
        .personalization_default(&context_in(tenant), tenant)
        .await
        .expect("default");

    let asked = reader.asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].key.as_str(), KEY);
    assert_eq!(asked[0].scope, format!("/tenants/{tenant}"));
}

#[tokio::test]
async fn an_undeclared_setting_falls_back_to_the_configured_default() {
    let (hub, _) = hub_answering(|| {
        Err(DeclarationError::not_found("no declaration for key")
            .with_resource(KEY)
            .create())
    });
    assert!(default_with(hub, true).await.expect("default"));
}

#[tokio::test]
async fn a_retired_setting_falls_back_to_the_configured_default() {
    let (hub, _) = hub_answering(|| {
        Err(DeclarationError::failed_precondition()
            .with_precondition_violation("setting", "declaration was retired", SETTING_RETIRED)
            .create())
    });
    assert!(default_with(hub, true).await.expect("default"));
}

#[tokio::test]
async fn an_unavailable_settings_service_is_an_unavailable_error() {
    let (hub, _) = hub_answering(|| {
        Err(CanonicalError::service_unavailable()
            .with_detail("database unreachable")
            .create())
    });
    let got = default_with(hub, true).await;
    assert!(
        matches!(got, Err(DomainError::Unavailable(_))),
        "got {got:?}"
    );
}

#[tokio::test]
async fn any_other_failure_is_an_internal_error() {
    let (hub, _) = hub_answering(|| {
        Err(DeclarationError::permission_denied()
            .with_reason("NOT_ENTITLED")
            .create())
    });
    let got = default_with(hub, true).await;
    assert!(matches!(got, Err(DomainError::Internal(_))), "got {got:?}");
}

#[tokio::test]
async fn a_value_that_is_not_a_boolean_is_an_internal_error() {
    let (hub, _) = hub_answering(|| Ok(serde_json::Value::String("yes".to_owned())));
    let got = default_with(hub, true).await;
    assert!(matches!(got, Err(DomainError::Internal(_))), "got {got:?}");
}

#[tokio::test]
async fn a_not_found_for_anything_but_the_declaration_is_an_internal_error() {
    let (hub, _) = hub_answering(|| {
        Err(CategoryError::not_found("no such category")
            .with_resource("personalization")
            .create())
    });
    let got = default_with(hub, true).await;
    assert!(matches!(got, Err(DomainError::Internal(_))), "got {got:?}");
}

#[tokio::test]
async fn a_retryable_failure_is_an_unavailable_error() {
    let slow: fn() -> Result<serde_json::Value, CanonicalError> =
        || Err(DeclarationError::deadline_exceeded("too slow").create());
    let throttled: fn() -> Result<serde_json::Value, CanonicalError> = || {
        Err(DeclarationError::resource_exhausted("too many requests")
            .with_quota_violation("settings", "rate limit")
            .create())
    };

    for answer in [slow, throttled] {
        let (hub, _) = hub_answering(answer);
        let got = default_with(hub, true).await;
        assert!(
            matches!(got, Err(DomainError::Unavailable(_))),
            "got {got:?}"
        );
    }
}

/// A settings client that never answers.
struct PendingReader;

#[async_trait]
impl SettingsReaderClient for PendingReader {
    async fn get_effective(
        &self,
        _ctx: &SecurityContext,
        _req: GetEffectiveRequest,
    ) -> Result<EffectiveValueResponse, CanonicalError> {
        std::future::pending().await
    }

    async fn get_effective_bulk(
        &self,
        _ctx: &SecurityContext,
        _selector: BulkSelector,
        _scope: String,
    ) -> Result<Vec<BulkOutcome>, CanonicalError> {
        std::future::pending().await
    }

    async fn resolve_secret(
        &self,
        _ctx: &SecurityContext,
        _handle: SecretHandle,
    ) -> Result<SecretString, CanonicalError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_read_without_an_answer_in_time_is_an_unavailable_error() {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn SettingsReaderClient>(Arc::new(PendingReader));

    let got = SettingsServiceDefaults::new(hub, true)
        .expect("the key parses")
        .with_read_timeout(Duration::from_millis(10))
        .personalization_default(&context_in(Uuid::new_v4()), Uuid::new_v4())
        .await;

    assert!(
        matches!(got, Err(DomainError::Unavailable(_))),
        "got {got:?}"
    );
}

#[tokio::test]
async fn the_fallback_keeps_answering_after_its_first_warning() {
    let defaults =
        SettingsServiceDefaults::new(Arc::new(ClientHub::new()), true).expect("the key parses");
    let ctx = context_in(Uuid::new_v4());

    for _ in 0..3 {
        let got = defaults
            .personalization_default(&ctx, Uuid::new_v4())
            .await
            .expect("default");
        assert!(got);
    }
}
