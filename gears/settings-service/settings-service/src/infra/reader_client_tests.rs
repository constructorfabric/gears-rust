//! The SDK contract as a consumer sees it, over the resolution harness.

use std::sync::Arc;

use secrecy::ExposeSecret;
use serde_json::json;
use settings_service_sdk::api::{BulkSelector, SettingsReaderClient};
use settings_service_sdk::models::GetEffectiveRequest;
use settings_service_sdk::{EffectiveSource, SettingsError};
use toolkit_security::SecurityContext;

use super::ReaderClient;
use crate::domain::resolution::scope_class;
use crate::domain::secrets::SecretResolver;
use crate::test_support::{
    AllowAllGate, RecordingAudit, RecordingReadMetrics, RecordingSecrets, ResolutionHarness, SECRET,
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
    let (reader, secrets, audit, _) = metered_with(h);
    (reader, secrets, audit)
}

/// The reader over the harness, with the recording `ReadMetrics` it reports
/// to.
fn metered(h: &ResolutionHarness) -> (Reader, Arc<RecordingReadMetrics>) {
    let (reader, _, _, metrics) = metered_with(h);
    (reader, metrics)
}

fn metered_with(
    h: &ResolutionHarness,
) -> (
    Reader,
    Arc<RecordingSecrets>,
    Arc<RecordingAudit>,
    Arc<RecordingReadMetrics>,
) {
    let metrics = Arc::new(RecordingReadMetrics::default());
    let secrets = Arc::new(RecordingSecrets::default());
    let audit = Arc::new(RecordingAudit::default());
    let resolver = Arc::new(SecretResolver::new(
        Arc::clone(&h.resolver),
        Arc::clone(&secrets) as Arc<dyn crate::domain::ports::SecretManager>,
        Arc::new(AllowAllGate),
        Arc::clone(&audit),
        Arc::new(crate::domain::ports::NoMetrics),
    ));
    (
        ReaderClient::new(
            Arc::clone(&h.db),
            Arc::clone(&h.resolver),
            resolver,
            Arc::clone(&metrics) as Arc<dyn crate::domain::ports::ReadMetrics>,
        ),
        secrets,
        audit,
        metrics,
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
async fn a_bulk_read_past_the_bound_is_refused_whole_never_answered_partially() {
    use settings_service_sdk::api::BULK_LIMIT;
    let h = ResolutionHarness::new().await;
    h.declare("one", scope_class::LOCAL, json!(1)).await;
    let keys = vec![h.key("one"); BULK_LIMIT + 1];

    let err = reader(&h)
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(keys),
            scope_of(h.tree.a),
        )
        .await
        .expect_err("the request as a whole is refused");
    assert!(
        matches!(
            err,
            toolkit_canonical_errors::CanonicalError::InvalidArgument { .. }
        ) && format!("{err:?}").contains(crate::field::BULK_TOO_LARGE),
        "{err:?}"
    );

    // At the bound, every key resolves.
    let keys = vec![h.key("one"); BULK_LIMIT];
    let outcomes = reader(&h)
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(keys),
            scope_of(h.tree.a),
        )
        .await
        .expect("within the bound");
    assert_eq!(outcomes.len(), BULK_LIMIT);
    assert!(outcomes.iter().all(|o| o.result.is_ok()));
}

#[tokio::test]
async fn a_bulk_read_at_a_scope_that_is_not_a_path_is_refused_whole() {
    let h = ResolutionHarness::new().await;
    h.declare("one", scope_class::LOCAL, json!(1)).await;
    let err = reader(&h)
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![h.key("one")]),
            "tenant-a".to_owned(),
        )
        .await
        .expect_err("no key can be resolved at a scope that does not parse");
    assert!(
        matches!(
            err,
            toolkit_canonical_errors::CanonicalError::InvalidArgument { .. }
        ),
        "{err:?}"
    );
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
        .await
        .expect("the request is served; each key answers for itself");

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
async fn a_category_holding_a_row_whose_key_does_not_parse_is_refused_whole_not_shrunk() {
    use toolkit_security::AccessScope;

    use crate::domain::declaration::{DeclarationDraft, DeclarationRepository};
    use crate::infra::storage::declaration_repo::DeclarationRepo;
    use crate::test_support::BOOL;

    let h = ResolutionHarness::new().await;
    h.declare("good", scope_class::LOCAL, json!(1)).await;
    // A row no write path produces — the repository takes the key as text.
    {
        let conn = h.db.conn().expect("connection");
        DeclarationRepo
            .insert(
                &conn,
                &AccessScope::allow_all(),
                DeclarationDraft {
                    key: "not a key".to_owned(),
                    leaf_slug: "broken".to_owned(),
                    value_type_id: BOOL.to_owned(),
                    category_id: h.category_id(),
                    default_value: json!(false),
                    scope_class: scope_class::LOCAL.to_owned(),
                    mode: "standard".to_owned(),
                    requires_step_up: false,
                    anonymous_exposable: false,
                    domain_affinity: None,
                    has_secret_trait: false,
                    data_classification: "public".to_owned(),
                    source: "admin_authored".to_owned(),
                    owner_module: None,
                    licence_feature: None,
                    display_name: None,
                    description: None,
                    created_by: "test".to_owned(),
                },
            )
            .await
            .expect("the row is stored as given");
    }

    let err = reader(&h)
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Category(h.category_id().to_string()),
            scope_of(h.tree.a),
        )
        .await
        .expect_err("refused whole, not shortened by one");
    // Not one outcome short: a category that cannot be enumerated honestly is
    // refused whole, never answered as if the broken row were not filed
    // under it.
    assert!(
        matches!(
            err,
            toolkit_canonical_errors::CanonicalError::Internal { .. }
        ),
        "{err:?}"
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
        .await
        .expect("the category enumerates");

    let mut keys: Vec<String> = outcomes.iter().map(|o| o.key.to_string()).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![h.key("one").to_string(), h.key("two").to_string()]
    );
    assert!(outcomes.iter().all(|o| o.result.is_ok()));

    // A failure and a category that files nothing are now told apart: the
    // first is the request's error, the second an empty batch.
    let err = reader(&h)
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Category("not-a-uuid".to_owned()),
            scope_of(h.tree.a),
        )
        .await
        .expect_err("not a category id");
    assert!(
        matches!(
            err,
            toolkit_canonical_errors::CanonicalError::InvalidArgument { .. }
        ),
        "{err:?}"
    );
    let empty = reader(&h)
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Category(uuid::Uuid::now_v7().to_string()),
            scope_of(h.tree.a),
        )
        .await
        .expect("a category id that files nothing is not a failure");
    assert!(empty.is_empty());
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
    assert_eq!(plaintext.expose_secret(), "hunter2");
    // The SDK hands the plaintext over wrapped: a consumer's `{:?}` shows
    // nothing, and the bytes are read only where they are meant to be.
    assert!(
        !format!("{plaintext:?}").contains("hunter2"),
        "{plaintext:?}"
    );
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

#[tokio::test]
async fn a_handle_issued_before_the_secret_was_configured_resolves_to_whatever_is_current() {
    // A handle encodes the key and the scope, nothing about the credential:
    // the same handle, kept across configuration and a later change, always
    // resolves to what is current at that scope.
    let h = ResolutionHarness::new().await;
    let (reader, secrets, _audit) = reader_with(&h);
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
    let issued = reader
        .get_effective(
            &SecurityContext::anonymous(),
            GetEffectiveRequest {
                key: key.clone(),
                scope: scope_of(h.tree.b),
            },
        )
        .await
        .expect("resolves");
    let token = issued.value.as_str().expect("a handle string").to_owned();
    let same_handle = || settings_service_sdk::SecretHandle::new(&token);

    reader
        .resolve_secret(&SecurityContext::anonymous(), same_handle())
        .await
        .expect_err("unconfigured at issue time");

    // Configured at `a` after the handle was issued: the handle for `b`
    // resolves to the inherited credential.
    secrets.seed("first", "hunter2");
    h.set_secret(d, h.tree.a, "first").await;
    h.cache.invalidate_key(key.as_str());
    let plaintext = reader
        .resolve_secret(&SecurityContext::anonymous(), same_handle())
        .await
        .expect("resolved");
    assert_eq!(plaintext.expose_secret(), "hunter2");

    // A closer override appears at `b`: the same handle now resolves to it.
    secrets.seed("second", "s3cret");
    h.set_secret(d, h.tree.b, "second").await;
    h.cache.invalidate_key(key.as_str());
    let plaintext = reader
        .resolve_secret(&SecurityContext::anonymous(), same_handle())
        .await
        .expect("resolved");
    assert_eq!(plaintext.expose_secret(), "s3cret");
}

// ---- Read metrics: recorded here, at the reader boundary, from the
// resolver's report.

fn read_request(h: &ResolutionHarness, name: &str, tenant: uuid::Uuid) -> GetEffectiveRequest {
    GetEffectiveRequest {
        key: h.key(name),
        scope: scope_of(tenant),
    }
}

#[tokio::test]
async fn a_cache_hit_records_the_cache_source_and_one_hit_lookup_under_the_declarations_class() {
    let h = ResolutionHarness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    h.set(d, h.tree.a, json!(true)).await;
    // The first resolve fills the cache.
    h.resolve(
        "strict",
        crate::domain::resolution::ScopeTarget::Tenant(h.tree.b),
    )
    .await
    .expect("resolves");
    let (reader, metrics) = metered(&h);

    reader
        .get_effective(
            &SecurityContext::anonymous(),
            read_request(&h, "strict", h.tree.b),
        )
        .await
        .expect("resolves");

    assert_eq!(metrics.lookups(), vec![("hit", scope_class::CASCADING)]);
    let reads = metrics.reads();
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert_eq!((reads[0].0, reads[0].1), ("cache", scope_class::CASCADING));
    assert!(metrics.failures().is_empty());
}

#[tokio::test]
async fn a_miss_that_resolves_records_the_resolve_source_and_one_miss_lookup() {
    let h = ResolutionHarness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    h.set(d, h.tree.a, json!(true)).await;
    let (reader, metrics) = metered(&h);

    reader
        .get_effective(
            &SecurityContext::anonymous(),
            read_request(&h, "strict", h.tree.b),
        )
        .await
        .expect("resolves");

    assert_eq!(metrics.lookups(), vec![("miss", scope_class::CASCADING)]);
    let reads = metrics.reads();
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert_eq!(
        (reads[0].0, reads[0].1),
        ("resolve", scope_class::CASCADING)
    );
    assert!(
        reads[0].2 > 0.0,
        "the span of a resolve is not zero: {reads:?}"
    );
}

#[tokio::test]
async fn the_class_label_follows_the_declaration_for_every_scope_class() {
    let h = ResolutionHarness::new().await;
    for (name, class) in [
        ("g", scope_class::GLOBAL),
        ("c", scope_class::CASCADING),
        ("l", scope_class::LOCAL),
    ] {
        h.declare(name, class, json!(false)).await;
    }
    let (reader, metrics) = metered(&h);
    for name in ["g", "c", "l"] {
        reader
            .get_effective(
                &SecurityContext::anonymous(),
                read_request(&h, name, h.tree.b),
            )
            .await
            .expect("resolves");
    }
    assert_eq!(
        metrics.lookups(),
        vec![
            ("miss", scope_class::GLOBAL),
            ("miss", scope_class::CASCADING),
            ("miss", scope_class::LOCAL),
        ]
    );
}

#[tokio::test]
async fn an_unavailable_dependency_counts_one_unavailable_failure_and_no_lookup_or_duration() {
    let h = ResolutionHarness::new().await;
    h.declare("strict", scope_class::CASCADING, json!(false))
        .await;
    h.hierarchy.set_unavailable(true);
    let (reader, metrics) = metered(&h);

    let err = reader
        .get_effective(
            &SecurityContext::anonymous(),
            read_request(&h, "strict", h.tree.b),
        )
        .await
        .expect_err("the walk cannot run");

    assert!(matches!(
        SettingsError::from(err),
        SettingsError::Unavailable { .. }
    ));
    assert_eq!(metrics.failures(), vec!["unavailable"]);
    assert!(metrics.lookups().is_empty());
    assert!(metrics.reads().is_empty());
}

#[tokio::test]
async fn an_internal_failure_counts_one_internal_failure_and_no_lookup_or_duration() {
    let h = ResolutionHarness::new().await;
    h.declare("odd", scope_class::CASCADING, json!(false)).await;
    // The tenant resolver answers with a fault of its own, not an outage.
    h.hierarchy.set_broken(true);
    let (reader, metrics) = metered(&h);

    let err = reader
        .get_effective(
            &SecurityContext::anonymous(),
            read_request(&h, "odd", h.tree.b),
        )
        .await
        .expect_err("the walk cannot run");

    assert!(
        matches!(
            err,
            toolkit_canonical_errors::CanonicalError::Internal { .. }
        ),
        "{err:?}"
    );
    assert_eq!(metrics.failures(), vec!["internal"]);
    assert!(metrics.lookups().is_empty());
    assert!(metrics.reads().is_empty());
}

#[tokio::test]
async fn a_not_found_and_a_retired_outcome_record_nothing() {
    let h = ResolutionHarness::new().await;
    let gone = h.declare("gone", scope_class::LOCAL, json!(1)).await;
    h.retire(gone).await;
    let live = h.declare("live", scope_class::LOCAL, json!(1)).await;
    let _ = live;
    let (reader, metrics) = metered(&h);

    reader
        .get_effective(
            &SecurityContext::anonymous(),
            read_request(&h, "ghost", h.tree.b),
        )
        .await
        .expect_err("not found");
    reader
        .get_effective(
            &SecurityContext::anonymous(),
            read_request(&h, "gone", h.tree.b),
        )
        .await
        .expect_err("retired");
    assert!(
        metrics.is_silent(),
        "an answer is not a failure: {:?} {:?} {:?}",
        metrics.lookups(),
        metrics.reads(),
        metrics.failures()
    );

    // The recorder is wired: a read that resolves is counted.
    reader
        .get_effective(
            &SecurityContext::anonymous(),
            read_request(&h, "live", h.tree.b),
        )
        .await
        .expect("resolves");
    assert_eq!(metrics.lookups().len(), 1);
}

#[tokio::test]
async fn a_failure_of_the_readers_own_connection_is_unavailable_and_counts_one_failure() {
    let h = ResolutionHarness::new().await;
    h.declare("flag", scope_class::GLOBAL, json!(false)).await;
    let (reader, metrics) = metered(&h);
    let reader = Arc::new(reader);
    let request = read_request(&h, "flag", h.tree.b);

    // A connection asked for from inside a transaction is refused: the one
    // way the reader's own `conn()` fails without a contrived database.
    let outcome =
        h.db.transaction(move |_tx| {
            Box::pin(async move {
                Ok::<_, toolkit_db::DbError>(
                    reader
                        .get_effective(&SecurityContext::anonymous(), request)
                        .await,
                )
            })
        })
        .await
        .expect("the transaction itself runs");

    let err = outcome.expect_err("no connection");
    assert!(
        matches!(SettingsError::from(err), SettingsError::Unavailable { .. }),
        "a dependency that cannot answer is unavailable, not the service's own fault"
    );
    assert_eq!(metrics.failures(), vec!["unavailable"]);
    assert!(metrics.lookups().is_empty() && metrics.reads().is_empty());
}

#[tokio::test]
async fn a_bulk_read_of_n_keys_that_resolve_counts_n_lookups() {
    let h = ResolutionHarness::new().await;
    for name in ["one", "two", "three"] {
        h.declare(name, scope_class::CASCADING, json!(1)).await;
    }
    let (reader, metrics) = metered(&h);

    let outcomes = reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![h.key("one"), h.key("two"), h.key("three")]),
            scope_of(h.tree.b),
        )
        .await
        .expect("served");

    assert!(outcomes.iter().all(|o| o.result.is_ok()));
    assert_eq!(metrics.lookups(), vec![("miss", scope_class::CASCADING); 3]);
    assert_eq!(metrics.reads().len(), 3);
    assert!(metrics.failures().is_empty());
}

#[tokio::test]
async fn a_bulk_read_whose_platform_scope_cannot_be_read_counts_one_failure_for_the_request() {
    let h = ResolutionHarness::new().await;
    for name in ["one", "two", "three"] {
        h.declare(name, scope_class::CASCADING, json!(1)).await;
    }
    h.platform.go_down();
    let (reader, metrics) = metered(&h);

    reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![h.key("one"), h.key("two"), h.key("three")]),
            scope_of(h.tree.b),
        )
        .await
        .expect_err("no key can be resolved without the root");

    assert_eq!(
        metrics.failures(),
        vec!["unavailable"],
        "one for the request, not one per key"
    );
    assert!(metrics.lookups().is_empty() && metrics.reads().is_empty());
}

#[tokio::test]
async fn a_bulk_whose_category_cannot_be_enumerated_counts_one_internal_failure() {
    use toolkit_security::AccessScope;

    use crate::domain::declaration::{DeclarationDraft, DeclarationRepository};
    use crate::infra::storage::declaration_repo::DeclarationRepo;
    use crate::test_support::BOOL;

    let h = ResolutionHarness::new().await;
    h.declare("good", scope_class::LOCAL, json!(1)).await;
    {
        let conn = h.db.conn().expect("connection");
        DeclarationRepo
            .insert(
                &conn,
                &AccessScope::allow_all(),
                DeclarationDraft {
                    key: "not a key".to_owned(),
                    leaf_slug: "broken".to_owned(),
                    value_type_id: BOOL.to_owned(),
                    category_id: h.category_id(),
                    default_value: json!(false),
                    scope_class: scope_class::LOCAL.to_owned(),
                    mode: "standard".to_owned(),
                    requires_step_up: false,
                    anonymous_exposable: false,
                    domain_affinity: None,
                    has_secret_trait: false,
                    data_classification: "public".to_owned(),
                    source: "admin_authored".to_owned(),
                    owner_module: None,
                    licence_feature: None,
                    display_name: None,
                    description: None,
                    created_by: "test".to_owned(),
                },
            )
            .await
            .expect("the row is stored as given");
    }
    let (reader, metrics) = metered(&h);

    reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Category(h.category_id().to_string()),
            scope_of(h.tree.a),
        )
        .await
        .expect_err("refused whole");

    assert_eq!(metrics.failures(), vec!["internal"]);
    assert!(metrics.lookups().is_empty() && metrics.reads().is_empty());
}

#[tokio::test]
async fn a_bulk_whose_chain_walk_is_unavailable_counts_one_failure_per_cascading_key() {
    let h = ResolutionHarness::new().await;
    h.declare("first", scope_class::CASCADING, json!(1)).await;
    h.declare("second", scope_class::CASCADING, json!(2)).await;
    h.declare("flat", scope_class::GLOBAL, json!(3)).await;
    h.hierarchy.set_unavailable(true);
    let (reader, metrics) = metered(&h);

    let outcomes = reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![h.key("first"), h.key("flat"), h.key("second")]),
            scope_of(h.tree.b),
        )
        .await
        .expect("the request is served; each key answers for itself");

    assert!(outcomes[0].result.is_err());
    assert!(
        outcomes[1].result.is_ok(),
        "a global setting never asks for ancestry"
    );
    assert!(outcomes[2].result.is_err());
    assert_eq!(
        metrics.failures(),
        vec!["unavailable", "unavailable"],
        "the walk is lazy and per key"
    );
    assert_eq!(metrics.lookups(), vec![("miss", scope_class::GLOBAL)]);
    assert_eq!(metrics.reads().len(), 1);
}

#[tokio::test]
async fn a_bulk_read_refused_for_its_size_or_scope_counts_nothing() {
    use settings_service_sdk::api::BULK_LIMIT;
    let h = ResolutionHarness::new().await;
    h.declare("one", scope_class::LOCAL, json!(1)).await;
    let (reader, metrics) = metered(&h);

    reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![h.key("one"); BULK_LIMIT + 1]),
            scope_of(h.tree.a),
        )
        .await
        .expect_err("past the bound");
    reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![h.key("one")]),
            "tenant-a".to_owned(),
        )
        .await
        .expect_err("not a path");
    assert!(metrics.is_silent());

    // The recorder is wired: a served request is counted.
    reader
        .get_effective_bulk(
            &SecurityContext::anonymous(),
            BulkSelector::Keys(vec![h.key("one")]),
            scope_of(h.tree.a),
        )
        .await
        .expect("served");
    assert_eq!(metrics.lookups().len(), 1);
}

#[tokio::test]
async fn the_administrative_read_and_the_secret_path_record_nothing_on_the_read_metrics() {
    let h = ResolutionHarness::new().await;
    let (reader, secrets, _, metrics) = metered_with(&h);
    h.declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let d = h
        .declare_typed(
            "api_token",
            scope_class::CASCADING,
            json!(""),
            SECRET,
            "secret",
        )
        .await;
    secrets.seed("seeded", "hunter2");
    h.set_secret(d, h.tree.a, "seeded").await;

    // The administrative read is the resolver's own `resolve`.
    h.resolve(
        "flag",
        crate::domain::resolution::ScopeTarget::Tenant(h.tree.b),
    )
    .await
    .expect("resolves");
    // The machine path resolves again inside the secret resolver.
    let handle =
        crate::domain::secrets::issue_handle(h.key("api_token").as_str(), &scope_of(h.tree.b));
    let plaintext = reader
        .resolve_secret(&SecurityContext::anonymous(), handle)
        .await
        .expect("resolved");
    assert_eq!(plaintext.expose_secret(), "hunter2");
    assert!(metrics.is_silent());

    // The recorder is wired: the reader's own read is counted.
    reader
        .get_effective(
            &SecurityContext::anonymous(),
            read_request(&h, "flag", h.tree.b),
        )
        .await
        .expect("resolves");
    assert_eq!(metrics.lookups().len(), 1);
}
