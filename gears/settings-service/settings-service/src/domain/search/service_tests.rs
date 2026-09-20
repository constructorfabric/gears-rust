// Created: 2026-09-17 by Virtuozzo International GmbH
//! Attribution: which field a hit is told about, and that a row the database
//! returned is never dropped on the way to the client.

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{SearchService, declaration_match};
use crate::domain::category::Category;
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::Declaration;
use crate::domain::error::DomainError;
use crate::domain::search::{Corpus, MatchedField, Needle, SearchRepository, SearchRequest};
use crate::domain::value::StoredValue;

fn at(seconds: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(seconds).expect("in range")
}

fn declaration(
    name: &str,
    description: Option<&str>,
    default: Value,
    classification: &str,
) -> Declaration {
    Declaration {
        id: Uuid::new_v4(),
        key: format!("gts.cf.core.settings.setting_type.v1~acme.settings.network.{name}.v1~"),
        leaf_slug: name.to_owned(),
        value_type_id: "gts.cf.core.settings.type_string.v1~".to_owned(),
        category_id: Uuid::from_u128(7),
        scope_class: "cascading".to_owned(),
        mode: "standard".to_owned(),
        status: "active".to_owned(),
        domain_affinity: None,
        licence_feature: None,
        owner_module: None,
        description: description.map(str::to_owned),
        default_value: default,
        has_secret_trait: classification == "secret",
        data_classification: classification.to_owned(),
        requires_step_up: false,
        anonymous_exposable: false,
        source: "admin_authored".to_owned(),
        last_change_at: at(0),
        updated_at: at(0),
    }
}

fn category(name: &str) -> Category {
    Category {
        id: Uuid::from_u128(7),
        key: crate::domain::category::key::CategoryKey::parse("network").expect("slug"),
        name: name.to_owned(),
        description: None,
        domain_affinity: None,
        sort_order: 0,
        icon: None,
        etag: crate::domain::precondition::ETag::new("1"),
    }
}

fn override_row(declaration_id: Uuid, tenant: Uuid, value: Value) -> StoredValue {
    StoredValue {
        id: Uuid::new_v4(),
        declaration_id,
        tenant_id: tenant,
        value: Some(value),
        secret_ref: None,
        data_classification: "public".to_owned(),
        needs_review: false,
        needs_review_detail: None,
        last_change_at: at(1),
        updated_at: at(1),
        set_by: "admin".to_owned(),
    }
}

/// A repository that answers with what the test staged, and remembers which
/// declarations it was asked the overrides of.
#[derive(Default)]
struct Staged {
    declarations: Vec<Declaration>,
    overrides: Vec<StoredValue>,
    categories: Vec<Category>,
    asked_for: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl SearchRepository for Staged {
    async fn declarations<C: DBRunner>(
        &self,
        _conn: &C,
        _request: &SearchRequest<'_>,
    ) -> Result<Page<Declaration>, DomainError> {
        Ok(Page {
            items: self.declarations.clone(),
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 25,
            },
        })
    }

    async fn overrides<C: DBRunner>(
        &self,
        _conn: &C,
        declaration_ids: &[Uuid],
        _tenant_ids: &[Uuid],
        _needle: &Needle,
        _corpus: Corpus,
    ) -> Result<Vec<StoredValue>, DomainError> {
        self.asked_for
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(declaration_ids);
        Ok(self.overrides.clone())
    }

    async fn categories<C: DBRunner>(
        &self,
        _conn: &C,
        _ids: &[Uuid],
    ) -> Result<Vec<Category>, DomainError> {
        Ok(self.categories.clone())
    }
}

type Outcome = (String, MatchedField, Option<Uuid>);

async fn run(service: &SearchService<Staged>, raw: &str, corpus: Corpus) -> Vec<Outcome> {
    let harness = crate::test_support::ResolutionHarness::new().await;
    let conn = harness.db.conn().expect("connection");
    let needle = Needle::parse(raw).expect("a needle");
    let query = ODataQuery::default();
    let page = service
        .search(
            &conn,
            &SearchRequest {
                scope: &AccessScope::allow_all(),
                visibility: &DomainVisibility::Unrestricted,
                needle: &needle,
                corpus,
                tenant_ids: &[harness.tree.root],
                query: &query,
            },
        )
        .await
        .expect("search runs");
    page.hits
        .into_iter()
        .map(|h| {
            (
                h.declaration.leaf_slug.clone(),
                h.matched,
                h.row.as_ref().map(|r| r.tenant_id),
            )
        })
        .collect()
}

#[test]
fn a_declaration_level_hit_names_the_first_field_that_matched_in_the_order_told() {
    let needle = Needle::parse("proxy").expect("a needle");
    let cat = category("Proxy things");
    let by_key = declaration(
        "proxy_enabled",
        Some("Use a proxy"),
        json!("proxy"),
        "public",
    );
    assert_eq!(
        declaration_match(&by_key, Some(&cat), &needle, Corpus::Public),
        Some(MatchedField::Key),
        "the key outranks every other field that also matched"
    );
    let by_description = declaration("relay", Some("Use a proxy"), json!("proxy"), "public");
    assert_eq!(
        declaration_match(&by_description, Some(&cat), &needle, Corpus::Public),
        Some(MatchedField::Description)
    );
    let by_category = declaration("relay", None, json!("proxy"), "public");
    assert_eq!(
        declaration_match(&by_category, Some(&cat), &needle, Corpus::Public),
        Some(MatchedField::CategoryName)
    );
    let by_default = declaration("relay", None, json!("proxy"), "public");
    assert_eq!(
        declaration_match(
            &by_default,
            Some(&category("Network")),
            &needle,
            Corpus::Public
        ),
        Some(MatchedField::DefaultValue)
    );
    let by_nothing = declaration("relay", None, json!("direct"), "public");
    assert_eq!(
        declaration_match(
            &by_nothing,
            Some(&category("Network")),
            &needle,
            Corpus::Public
        ),
        None
    );
}

#[test]
fn a_default_outside_the_corpus_is_not_a_field_that_matched() {
    let needle = Needle::parse("ops@").expect("a needle");
    let pii = declaration("contact", None, json!("ops@example.test"), "pii");
    assert_eq!(declaration_match(&pii, None, &needle, Corpus::Public), None);
    assert_eq!(
        declaration_match(&pii, None, &needle, Corpus::PublicAndPii),
        Some(MatchedField::DefaultValue)
    );
    let secret = declaration("token", None, json!("ops@"), "secret");
    assert_eq!(
        declaration_match(&secret, None, &needle, Corpus::PublicAndPii),
        None,
        "a secret default is never a match, whatever the corpus"
    );
    let null_default = declaration("unset", None, json!(null), "public");
    assert_eq!(
        declaration_match(
            &null_default,
            None,
            &Needle::parse("null").expect("a needle"),
            Corpus::Public
        ),
        None
    );
}

#[tokio::test]
async fn an_override_hit_names_its_tenant_and_a_declaration_may_yield_several_hits() {
    let d = declaration("motto", None, json!("nothing"), "public");
    let id = d.id;
    let tenant_a = Uuid::from_u128(10);
    let tenant_b = Uuid::from_u128(11);
    let service = SearchService::new(Staged {
        declarations: vec![d],
        overrides: vec![
            override_row(id, tenant_a, json!("alpha")),
            override_row(id, tenant_b, json!("alphabet")),
        ],
        categories: vec![category("Network")],
        ..Staged::default()
    });

    let hits = run(&service, "alpha", Corpus::Public).await;
    assert_eq!(
        hits,
        vec![
            ("motto".to_owned(), MatchedField::Value, Some(tenant_a)),
            ("motto".to_owned(), MatchedField::Value, Some(tenant_b)),
        ],
        "no declaration-level hit: the declaration is on the page only because overrides matched"
    );
}

#[tokio::test]
async fn a_declaration_and_its_overrides_are_each_a_hit_when_both_matched() {
    let d = declaration("proxy_host", None, json!("none"), "public");
    let id = d.id;
    let tenant_a = Uuid::from_u128(10);
    let service = SearchService::new(Staged {
        declarations: vec![d],
        overrides: vec![override_row(id, tenant_a, json!("proxy.internal"))],
        categories: vec![category("Network")],
        ..Staged::default()
    });
    let hits = run(&service, "proxy", Corpus::Public).await;
    assert_eq!(
        hits,
        vec![
            ("proxy_host".to_owned(), MatchedField::Key, None),
            ("proxy_host".to_owned(), MatchedField::Value, Some(tenant_a)),
        ]
    );
}

#[tokio::test]
async fn a_row_the_database_matched_that_rust_cannot_name_is_attributed_not_dropped() {
    // The database compared `{"a": 1}` as PostgreSQL spells it, with a space;
    // Rust sees serde's compact text. The declaration came back, so it is a
    // hit — on the field whose projection can differ, the default.
    let d = declaration("shape", None, json!({"a": 1}), "public");
    let service = SearchService::new(Staged {
        declarations: vec![d],
        categories: vec![category("Network")],
        ..Staged::default()
    });
    let hits = run(&service, "\"a\": 1", Corpus::Public).await;
    assert_eq!(
        hits,
        vec![("shape".to_owned(), MatchedField::DefaultValue, None)]
    );

    // With no default in the corpus there is nothing else to point at but
    // the key; better a hit with an approximate label than a hit that
    // vanished between the database and the client.
    let d = declaration("pii_shape", None, json!({"a": 1}), "pii");
    let service = SearchService::new(Staged {
        declarations: vec![d],
        categories: vec![category("Network")],
        ..Staged::default()
    });
    let hits = run(&service, "\"a\": 1", Corpus::Public).await;
    assert_eq!(
        hits,
        vec![("pii_shape".to_owned(), MatchedField::Key, None)]
    );
}

#[tokio::test]
async fn the_overrides_are_asked_for_exactly_the_pages_declarations() {
    let d1 = declaration("one", None, json!("x"), "public");
    let d2 = declaration("two", None, json!("x"), "public");
    let ids = vec![d1.id, d2.id];
    let service = SearchService::new(Staged {
        declarations: vec![d1, d2],
        categories: vec![category("Network")],
        ..Staged::default()
    });

    run(&service, "one", Corpus::Public).await;

    assert_eq!(
        *service
            .repository()
            .asked_for
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        ids
    );
}
