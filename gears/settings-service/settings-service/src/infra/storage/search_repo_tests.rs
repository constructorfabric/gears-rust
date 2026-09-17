// Created: 2026-09-17 by Constructor Tech
//! The corpus, matched on a real database: what a needle may reach and what it
//! may never reach, on the `SQLite` dialect the tests run on.

use sea_orm::DbBackend;
use serde_json::{Value, json};
use toolkit_odata::ODataQuery;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::SearchRepo;
use crate::domain::category::key::CategoryKey;
use crate::domain::category::repo::{CategoryDraft, CategoryRepository};
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::repo::{DeclarationDraft, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::search::{Corpus, Needle, SearchRepository, SearchRequest, cursor_binding};
use crate::domain::value::ValueDraft;
use crate::domain::value::repo::ValueRepository;
use crate::infra::storage::category_repo::CategoryRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::test_support::{BOOL, ResolutionHarness};

fn repo() -> SearchRepo {
    SearchRepo::new(DbBackend::Sqlite)
}

fn needle(raw: &str) -> Needle {
    Needle::parse(raw).expect("a valid needle")
}

fn query(limit: u64) -> ODataQuery {
    ODataQuery {
        limit: Some(limit),
        ..ODataQuery::default()
    }
}

/// A declaration with the fields the harness's own helper does not set: a
/// description, and a category of the test's choosing.
async fn declare_described(
    h: &ResolutionHarness,
    name: &str,
    description: Option<&str>,
    default: Value,
    classification: &str,
    category_id: Uuid,
) -> Uuid {
    let conn = h.db.conn().expect("connection");
    DeclarationRepo
        .insert(
            &conn,
            &AccessScope::allow_all(),
            DeclarationDraft {
                key: h.key(name).to_string(),
                leaf_slug: name.to_owned(),
                value_type_id: BOOL.to_owned(),
                category_id,
                default_value: default,
                scope_class: "cascading".to_owned(),
                mode: "standard".to_owned(),
                requires_step_up: true,
                anonymous_exposable: false,
                domain_affinity: None,
                has_secret_trait: classification == "secret",
                data_classification: classification.to_owned(),
                source: "module_contributed".to_owned(),
                owner_module: Some("test".to_owned()),
                licence_feature: None,
                description: description.map(str::to_owned),
                created_by: "test".to_owned(),
            },
        )
        .await
        .expect("declaration")
        .id
}

/// A stored override with an explicit classification: the harness's `set`
/// always writes `public`.
async fn set_classified(
    h: &ResolutionHarness,
    declaration_id: Uuid,
    tenant: Uuid,
    value: Value,
    classification: &str,
) {
    let conn = h.db.conn().expect("connection");
    ValueRepo
        .insert(
            &conn,
            &AccessScope::allow_all(),
            ValueDraft {
                declaration_id,
                tenant_id: tenant,
                value: Some(value),
                secret_ref: None,
                data_classification: classification.to_owned(),
                needs_review: false,
                needs_review_detail: None,
                set_by: "test".to_owned(),
            },
        )
        .await
        .expect("override");
}

async fn page_of(
    h: &ResolutionHarness,
    raw: &str,
    corpus: Corpus,
    tenants: &[Uuid],
    query: &ODataQuery,
) -> Result<toolkit_odata::Page<crate::domain::declaration::Declaration>, DomainError> {
    let conn = h.db.conn().expect("connection");
    let n = needle(raw);
    let mut q = query.clone();
    if q.filter_hash.is_none() {
        q.filter_hash = Some(cursor_binding(&n, h.tree.root, corpus));
    }
    repo()
        .declarations(
            &conn,
            &SearchRequest {
                scope: &AccessScope::allow_all(),
                visibility: &DomainVisibility::Unrestricted,
                needle: &n,
                corpus,
                tenant_ids: tenants,
                query: &q,
            },
        )
        .await
}

async fn declaration_keys(
    h: &ResolutionHarness,
    raw: &str,
    corpus: Corpus,
    tenants: &[Uuid],
) -> Vec<String> {
    page_of(h, raw, corpus, tenants, &query(50))
        .await
        .expect("search runs")
        .items
        .into_iter()
        .map(|d| d.leaf_slug)
        .collect()
}

#[tokio::test]
async fn a_key_a_description_and_a_category_name_are_each_a_way_in() {
    let h = ResolutionHarness::new().await;
    let conn = h.db.conn().expect("connection");
    let billing = CategoryRepo
        .insert(
            &conn,
            &AccessScope::allow_all(),
            CategoryDraft {
                key: CategoryKey::parse("billing").expect("slug"),
                name: "Invoices & Payments".to_owned(),
                description: None,
                domain_affinity: None,
                sort_order: 0,
                icon: None,
            },
        )
        .await
        .expect("category")
        .id;
    declare_described(
        &h,
        "proxy_enabled",
        None,
        json!(true),
        "public",
        h.category_id(),
    )
    .await;
    declare_described(
        &h,
        "retention",
        Some("How long audit records are kept"),
        json!(true),
        "public",
        h.category_id(),
    )
    .await;
    declare_described(&h, "tax_rate", None, json!(true), "public", billing).await;

    let all = &[h.tree.root];
    assert_eq!(
        declaration_keys(&h, "proxy", Corpus::Public, all).await,
        vec!["proxy_enabled"]
    );
    assert_eq!(
        declaration_keys(&h, "audit records", Corpus::Public, all).await,
        vec!["retention"]
    );
    assert_eq!(
        declaration_keys(&h, "payments", Corpus::Public, all).await,
        vec!["tax_rate"],
        "the category name reaches the settings filed under it"
    );
    assert!(
        declaration_keys(&h, "nowhere", Corpus::Public, all)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_schema_default_matches_by_its_text_and_a_json_null_default_never_does() {
    let h = ResolutionHarness::new().await;
    declare_described(
        &h,
        "greeting",
        None,
        json!("hunter two"),
        "public",
        h.category_id(),
    )
    .await;
    declare_described(&h, "empty", None, json!(null), "public", h.category_id()).await;
    declare_described(&h, "port", None, json!(8087), "public", h.category_id()).await;

    let all = &[h.tree.root];
    assert_eq!(
        declaration_keys(&h, "hunter", Corpus::Public, all).await,
        vec!["greeting"]
    );
    assert_eq!(
        declaration_keys(&h, "8087", Corpus::Public, all).await,
        vec!["port"],
        "a number matches by its text"
    );
    assert!(
        declaration_keys(&h, "null", Corpus::Public, all)
            .await
            .is_empty(),
        "a JSON null default is the absence of a default, not the word"
    );
}

#[tokio::test]
async fn an_override_is_matched_where_it_is_set_and_only_inside_the_bounded_tenants() {
    let h = ResolutionHarness::new().await;
    let id = declare_described(&h, "motto", None, json!(true), "public", h.category_id()).await;
    h.set(id, h.tree.a, json!("alpha and omega")).await;
    h.set(id, h.tree.c, json!("gamma")).await;
    h.set(id, h.tree.s, json!("omega on the standalone")).await;

    let conn = h.db.conn().expect("connection");
    let subtree_of_a = &[h.tree.a, h.tree.b];
    assert_eq!(
        declaration_keys(&h, "omega", Corpus::Public, subtree_of_a).await,
        vec!["motto"]
    );
    let rows = repo()
        .overrides(&conn, &[id], subtree_of_a, &needle("omega"), Corpus::Public)
        .await
        .expect("overrides");
    assert_eq!(
        rows.iter().map(|r| r.tenant_id).collect::<Vec<_>>(),
        vec![h.tree.a],
        "the standalone descendant's row is outside the corpus"
    );

    assert!(
        declaration_keys(&h, "gamma", Corpus::Public, subtree_of_a)
            .await
            .is_empty(),
        "an override outside the subtree is not a way in"
    );
    assert!(
        declaration_keys(&h, "standalone", Corpus::Public, subtree_of_a)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_secret_is_never_matched_by_its_reference_or_by_anything() {
    let h = ResolutionHarness::new().await;
    let secret = h
        .declare_typed("api_token", "cascading", json!(""), BOOL, "secret")
        .await;
    h.set_secret(secret, h.tree.a, "settings/abc/def-ref").await;
    let plain = declare_described(
        &h,
        "visible",
        None,
        json!("def-ref"),
        "public",
        h.category_id(),
    )
    .await;

    let all = &[h.tree.root, h.tree.a, h.tree.b, h.tree.c];
    assert_eq!(
        declaration_keys(&h, "def-ref", Corpus::PublicAndPii, all).await,
        vec!["visible"],
        "the secret's stored reference is not searchable content"
    );
    let conn = h.db.conn().expect("connection");
    let rows = repo()
        .overrides(
            &conn,
            &[secret, plain],
            all,
            &needle("def-ref"),
            Corpus::PublicAndPii,
        )
        .await
        .expect("overrides");
    assert!(rows.is_empty(), "{rows:?}");
}

#[tokio::test]
async fn pii_content_enters_the_corpus_only_with_the_entitlement() {
    let h = ResolutionHarness::new().await;
    let contact = declare_described(
        &h,
        "contact",
        None,
        json!("ops@example.test"),
        "pii",
        h.category_id(),
    )
    .await;
    set_classified(&h, contact, h.tree.a, json!("oncall@example.test"), "pii").await;

    let all = &[h.tree.root, h.tree.a];
    assert!(
        declaration_keys(&h, "example.test", Corpus::Public, all)
            .await
            .is_empty(),
        "neither the default nor the override is reachable without read_unmasked"
    );
    assert_eq!(
        declaration_keys(&h, "example.test", Corpus::PublicAndPii, all).await,
        vec!["contact"]
    );
    let conn = h.db.conn().expect("connection");
    assert!(
        repo()
            .overrides(&conn, &[contact], all, &needle("oncall"), Corpus::Public)
            .await
            .expect("overrides")
            .is_empty()
    );
    assert_eq!(
        repo()
            .overrides(
                &conn,
                &[contact],
                all,
                &needle("oncall"),
                Corpus::PublicAndPii
            )
            .await
            .expect("overrides")
            .len(),
        1
    );
}

#[tokio::test]
async fn wildcards_in_the_needle_match_literally() {
    let h = ResolutionHarness::new().await;
    declare_described(
        &h,
        "discount",
        None,
        json!("100% off"),
        "public",
        h.category_id(),
    )
    .await;
    declare_described(
        &h,
        "plain",
        None,
        json!("100 percent"),
        "public",
        h.category_id(),
    )
    .await;
    declare_described(&h, "snake", None, json!("a_b"), "public", h.category_id()).await;
    declare_described(&h, "letters", None, json!("axb"), "public", h.category_id()).await;

    let all = &[h.tree.root];
    assert_eq!(
        declaration_keys(&h, "0% ", Corpus::Public, all).await,
        vec!["discount"],
        "`%` is a character, not a wildcard"
    );
    assert_eq!(
        declaration_keys(&h, "a_b", Corpus::Public, all).await,
        vec!["snake"],
        "`_` is a character, not a wildcard"
    );
}

#[tokio::test]
async fn a_retired_declaration_is_not_matched() {
    let h = ResolutionHarness::new().await;
    let id = declare_described(
        &h,
        "old_proxy",
        None,
        json!(true),
        "public",
        h.category_id(),
    )
    .await;
    h.retire(id).await;
    assert!(
        declaration_keys(&h, "old_proxy", Corpus::Public, &[h.tree.root])
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn pages_are_ordered_by_key_and_a_cursor_continues_only_its_own_search() {
    let h = ResolutionHarness::new().await;
    for name in ["page_c", "page_a", "page_b"] {
        declare_described(&h, name, None, json!(true), "public", h.category_id()).await;
    }
    let root = &[h.tree.root];

    let page = page_of(&h, "page_", Corpus::Public, root, &query(2))
        .await
        .expect("first page");
    assert_eq!(
        page.items
            .iter()
            .map(|d| d.leaf_slug.as_str())
            .collect::<Vec<_>>(),
        ["page_a", "page_b"]
    );
    let cursor = page.page_info.next_cursor.expect("a second page");

    let mut second = query(2);
    second.cursor =
        Some(toolkit_odata::CursorV1::decode(&cursor).expect("the cursor we were given decodes"));
    let page = page_of(&h, "page_", Corpus::Public, root, &second)
        .await
        .expect("second page");
    assert_eq!(
        page.items
            .iter()
            .map(|d| d.leaf_slug.as_str())
            .collect::<Vec<_>>(),
        ["page_c"]
    );
    assert!(page.page_info.next_cursor.is_none());

    // The same cursor, presented for another needle: its binding differs.
    let refused = page_of(&h, "page_a", Corpus::Public, root, &second).await;
    assert!(
        matches!(refused, Err(DomainError::Validation { .. })),
        "a cursor from another search is refused, got {refused:?}"
    );
}

#[tokio::test]
async fn the_page_carries_its_categories_for_breadcrumbs() {
    let h = ResolutionHarness::new().await;
    let conn = h.db.conn().expect("connection");
    let categories = repo()
        .categories(&conn, &[h.category_id(), Uuid::new_v4()])
        .await
        .expect("categories");
    assert_eq!(categories.len(), 1);
    assert_eq!(categories[0].name, "network");
}
