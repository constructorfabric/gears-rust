// Created: 2026-09-07 by Constructor Tech
//! The SDK contract as a consumer sees it, over the resolution harness.

use std::sync::Arc;

use serde_json::json;
use settings_service_sdk::api::{BulkSelector, SettingsReaderClient};
use settings_service_sdk::models::GetEffectiveRequest;
use settings_service_sdk::{EffectiveSource, SettingsError};
use toolkit_security::SecurityContext;

use super::ReaderClient;
use crate::domain::resolution::scope_class;
use crate::domain::secrets::SecretResolver;
use crate::test_support::{
    AllowAllGate, RecordingAudit, RecordingSecrets, ResolutionHarness, SECRET,
};

type Reader = ReaderClient<
    crate::infra::storage::declaration_repo::DeclarationRepo,
    crate::infra::storage::value_repo::ValueRepo,
    crate::infra::storage::access_repo::AccessRepo,
    Arc<RecordingAudit>,
>;

/// The reader over the harness, with a fake store and an open gate; the
/// store and the audit sink come back for inspection.
fn reader_with(h: &ResolutionHarness) -> (Reader, Arc<RecordingSecrets>, Arc<RecordingAudit>) {
    let secrets = Arc::new(RecordingSecrets::default());
    let audit = Arc::new(RecordingAudit::default());
    let resolver = Arc::new(SecretResolver::new(
        Arc::clone(&h.resolver),
        Arc::clone(&secrets) as Arc<dyn crate::domain::ports::SecretManager>,
        Arc::new(AllowAllGate),
        Arc::clone(&audit),
    ));
    (
        ReaderClient::new(Arc::clone(&h.db), Arc::clone(&h.resolver), resolver),
        secrets,
        audit,
    )
}

fn reader(h: &ResolutionHarness) -> Reader {
    reader_with(h).0
}

fn scope_of(tenant: uuid::Uuid) -> String {
    format!("/tenants/{tenant}")
}

#[tokio::test]
async fn a_consumer_gets_value_source_traits_and_a_trail_without_setter_identity() {
    let h = ResolutionHarness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    h.set(d, h.tree.a, json!(true)).await;

    let value = reader(&h)
        .get_effective(
            &SecurityContext::anonymous(),
            GetEffectiveRequest {
                key: h.key("strict"),
                scope: scope_of(h.tree.b),
            },
        )
        .await
        .expect("resolves");

    assert_eq!(value.value, json!(true));
    assert_eq!(value.source, EffectiveSource::Inherited);
    assert_eq!(value.source_scope, Some(scope_of(h.tree.a)));
    assert_eq!(value.scope, scope_of(h.tree.b));
    assert!(value.traits.is_object());
    let scopes: Vec<&str> = value
        .inheritance_trail
        .iter()
        .map(|e| e.scope.as_str())
        .collect();
    assert_eq!(
        scopes,
        vec![
            "/",
            scope_of(h.tree.a).as_str(),
            scope_of(h.tree.b).as_str()
        ]
    );
    assert!(value.inheritance_trail[1].provided_value);
    // The consumer trail carries no setter identity and no timestamp by type:
    // the SDK entry has neither field.
    let rendered = serde_json::to_value(&value).expect("serializes");
    assert!(rendered["inheritanceTrail"][1].get("setBy").is_none());
}

#[tokio::test]
async fn the_platform_scope_is_the_root_and_a_bad_scope_is_refused() {
    let h = ResolutionHarness::new().await;
    let d = h.declare("flag", scope_class::GLOBAL, json!(false)).await;
    h.set(d, h.tree.root, json!(true)).await;
    let r = reader(&h);

    let at_root = r
        .get_effective(
            &SecurityContext::anonymous(),
            GetEffectiveRequest {
                key: h.key("flag"),
                scope: "/".to_owned(),
            },
        )
        .await
        .expect("resolves");
    assert_eq!(
        (at_root.value.clone(), at_root.source),
        (json!(true), EffectiveSource::OwnOverride)
    );

    let err = r
        .get_effective(
            &SecurityContext::anonymous(),
            GetEffectiveRequest {
                key: h.key("flag"),
                scope: "tenant-a".to_owned(),
            },
        )
        .await
        .expect_err("a scope that is not a path is refused");
    assert!(
        matches!(
            err,
            toolkit_canonical_errors::CanonicalError::InvalidArgument { .. }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn outcomes_project_to_the_typed_errors_and_never_to_a_default() {
    let h = ResolutionHarness::new().await;
    let retired = h
        .declare("retired", scope_class::CASCADING, json!(false))
        .await;
    h.retire(retired).await;
    let cascading = h
        .declare("down", scope_class::CASCADING, json!(false))
        .await;
    h.set(cascading, h.tree.root, json!(true)).await;
    let r = reader(&h);
    let ctx = SecurityContext::anonymous();

    let err = r
        .get_effective(
            &ctx,
            GetEffectiveRequest {
                key: h.key("ghost"),
                scope: scope_of(h.tree.b),
            },
        )
        .await
        .expect_err("no declaration");
    assert!(matches!(
        SettingsError::from(err),
        SettingsError::NotFound { .. }
    ));

    let err = r
        .get_effective(
            &ctx,
            GetEffectiveRequest {
                key: h.key("retired"),
                scope: scope_of(h.tree.b),
            },
        )
        .await
        .expect_err("retired");
    assert!(matches!(
        SettingsError::from(err),
        SettingsError::Retired { .. }
    ));

    h.hierarchy.set_unavailable(true);
    let err = r
        .get_effective(
            &ctx,
            GetEffectiveRequest {
                key: h.key("down"),
                scope: scope_of(h.tree.b),
            },
        )
        .await
        .expect_err("the walk's dependency is down");
    assert!(matches!(
        SettingsError::from(err),
        SettingsError::Unavailable { .. }
    ));
}

#[tokio::test]
async fn a_bulk_read_answers_every_key_independently() {
    let h = ResolutionHarness::new().await;
    let good = h.declare("good", scope_class::LOCAL, json!(1)).await;
    h.set(good, h.tree.b, json!(2)).await;
    let retired = h.declare("gone", scope_class::LOCAL, json!(1)).await;
    h.retire(retired).await;

    let outcomes = reader(&h)
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![h.key("good"), h.key("gone"), h.key("ghost")]),
            scope_of(h.tree.b),
        )
        .await;

    assert_eq!(outcomes.len(), 3);
    assert!(matches!(&outcomes[0].result, Ok(v) if v.value == json!(2)));
    assert!(matches!(
        outcomes[1]
            .result
            .as_ref()
            .map_err(|e| SettingsError::from(e.clone())),
        Err(SettingsError::Retired { .. })
    ));
    assert!(matches!(
        outcomes[2]
            .result
            .as_ref()
            .map_err(|e| SettingsError::from(e.clone())),
        Err(SettingsError::NotFound { .. })
    ));
    assert_eq!(
        h.hierarchy.chain_calls(),
        0,
        "local settings never ask for ancestry"
    );
}

#[tokio::test]
async fn a_category_selector_resolves_every_declaration_filed_under_it() {
    let h = ResolutionHarness::new().await;
    h.declare("one", scope_class::LOCAL, json!(1)).await;
    h.declare("two", scope_class::LOCAL, json!(2)).await;

    let outcomes = reader(&h)
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Category(h.category_id().to_string()),
            scope_of(h.tree.a),
        )
        .await;

    let mut keys: Vec<String> = outcomes.iter().map(|o| o.key.to_string()).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![h.key("one").to_string(), h.key("two").to_string()]
    );
    assert!(outcomes.iter().all(|o| o.result.is_ok()));

    let none = reader(&h)
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Category("not-a-uuid".to_owned()),
            scope_of(h.tree.a),
        )
        .await;
    assert!(
        none.is_empty(),
        "no key is known, so no per-key outcome can carry the failure"
    );
}

#[tokio::test]
async fn a_secret_setting_reads_as_an_opaque_handle_that_resolves_only_through_the_reader() {
    let h = ResolutionHarness::new().await;
    let (reader, secrets, audit) = reader_with(&h);
    let d = h
        .declare_typed(
            "api_token",
            crate::domain::resolution::scope_class::CASCADING,
            json!(""),
            SECRET,
            "secret",
        )
        .await;
    let key = h.key("api_token");
    let scope = scope_of(h.tree.b);

    // Unconfigured: still a handle, `source=schema_default`, and resolving it
    // is `SecretNotConfigured`, never the placeholder.
    let unconfigured = reader
        .get_effective(
            &SecurityContext::anonymous(),
            GetEffectiveRequest {
                key: key.clone(),
                scope: scope.clone(),
            },
        )
        .await
        .expect("resolves");
    assert_eq!(unconfigured.source, EffectiveSource::SchemaDefault);
    let token = unconfigured.value.as_str().expect("a handle string");
    assert!(token.starts_with("sh1."));
    let err = reader
        .resolve_secret(
            &SecurityContext::anonymous(),
            settings_service_sdk::SecretHandle::new(token),
        )
        .await
        .expect_err("unconfigured");
    assert!(matches!(
        SettingsError::from(err),
        SettingsError::SecretNotConfigured { .. }
    ));

    // Configured at `a`: the handle for `b` is the same shape, carries neither
    // the reference nor `a`, and resolves to the plaintext with one record.
    let reference = "seeded-at-a".to_owned();
    secrets.seed(&reference, "hunter2");
    h.set_secret(d, h.tree.a, &reference).await;
    // The harness writes the row directly, so it evicts by hand what a
    // committed write would have evicted.
    h.cache.invalidate_key(key.as_str());
    let configured = reader
        .get_effective(
            &SecurityContext::anonymous(),
            GetEffectiveRequest {
                key: key.clone(),
                scope: scope.clone(),
            },
        )
        .await
        .expect("resolves");
    assert_eq!(configured.source, EffectiveSource::Inherited);
    let token = configured.value.as_str().expect("a handle string");
    assert!(!token.contains(&reference) && !token.contains(&h.tree.a.to_string()));
    let plaintext = reader
        .resolve_secret(
            &SecurityContext::anonymous(),
            settings_service_sdk::SecretHandle::new(token),
        )
        .await
        .expect("resolved");
    assert_eq!(plaintext, "hunter2");
    assert_eq!(audit.operations(), vec!["secret_use"]);
}

#[tokio::test]
async fn a_malformed_handle_projects_to_an_invalid_argument() {
    let h = ResolutionHarness::new().await;
    let err = reader(&h)
        .resolve_secret(
            &SecurityContext::anonymous(),
            settings_service_sdk::SecretHandle::new("x"),
        )
        .await
        .expect_err("malformed");
    assert!(matches!(
        SettingsError::from(err),
        SettingsError::Other { .. }
    ));
}
