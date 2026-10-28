//! Cursor-pagination contract for `/files`, `/files/{id}/versions`, `/retention-rules` and
//! `/storages`:
//! full walks in both directions (including `created_at` ties) and cursor errors.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::handlers;
use file_storage::domain::error::DomainError;
use file_storage::domain::pagination;
use file_storage::domain::policy::{AgeRetention, RetentionRuleBody, RetentionScope};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{InMemoryBackend, StorageBackend};
use file_storage::infra::storage::repo::{FileRepo, VersionRepo};
use file_storage_sdk::{File, FileVersion, NewFile, OwnerFilter, OwnerKind, VersionStatus};

mod common;
use common::{ScopedTestAuthorizer, Stack, StackBuilder, body_json, ctx_for};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.cursor_pagination_test.file.type.v1~");
const BASE: &str = "/api/file-storage/v1";

/// Real default page sizes, so `?limit` clamping tests observe the real ceiling.
fn base_config() -> ServiceConfig {
    ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 25,
        max_page_size: 200,
        idempotency_ttl_secs: 86400,
    }
}

/// Parse a wire token the way the `OData` extractor does before the services see it.
fn cur(token: &str) -> toolkit_odata::CursorV1 {
    toolkit_odata::CursorV1::decode(token).expect("decode cursor")
}

fn new_file(owner_id: Uuid) -> NewFile {
    NewFile {
        gts_file_type: GTS.to_owned(),
        ..common::new_file_for(owner_id, "doc.bin", "application/octet-stream")
    }
}

fn valid_rule_body() -> RetentionRuleBody {
    RetentionRuleBody {
        age: Some(AgeRetention { max_age_days: 30 }),
        inactivity: None,
        metadata: None,
    }
}

fn get_req(uri: String) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build request")
}

/// Seeds `n` files for one owner via `FileRepo` with pinned `created_at` (`instants[i]`) to
/// exercise the tie-break at page boundaries.
async fn seed_files(
    db: &Arc<DBProvider<DbError>>,
    tenant: Uuid,
    owner_id: Uuid,
    instants: &[OffsetDateTime],
) -> Vec<Uuid> {
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let scope = AccessScope::allow_all();
    let mut ids = Vec::with_capacity(instants.len());
    for &created_at in instants {
        let file_id = Uuid::now_v7();
        let file = File {
            file_id,
            tenant_id: tenant,
            owner_kind: OwnerKind::User,
            owner_id,
            name: "doc.bin".to_owned(),
            gts_file_type: GTS.to_owned(),
            content_id: None,
            meta_version: 0,
            created_at,
            last_modified_at: created_at,
        };
        files.create(&conn, &scope, &file).await.expect("create");
        ids.push(file_id);
    }
    ids
}

/// Canonical order for every listing: `created_at DESC`, id column `DESC`.
fn expected_order(mut items: Vec<(OffsetDateTime, Uuid)>) -> Vec<Uuid> {
    items.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    items.into_iter().map(|(_, id)| id).collect()
}

#[tokio::test]
async fn files_cursor_walk_covers_all_items_exactly_once_including_created_at_ties() {
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .build()
        .await;
    let Stack { db, svc, .. } = s;

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let base = OffsetDateTime::now_utc();
    // 7 files over 3 instants, so a small `limit` walk crosses a `created_at` tie.
    let instants = [
        base,
        base,
        base,
        base + time::Duration::seconds(1),
        base + time::Duration::seconds(1),
        base + time::Duration::seconds(2),
        base + time::Duration::seconds(2),
    ];
    let ids = seed_files(&db, tenant, owner, &instants).await;
    let expected = expected_order(instants.iter().copied().zip(ids.iter().copied()).collect());

    let ctx = ctx_for(tenant, owner);
    let owner_filter = OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id: owner,
    };

    let mut seen = Vec::new();
    let mut pages: Vec<Vec<Uuid>> = Vec::new();
    let mut prev_cursors: Vec<Option<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = svc
            .list_files(&ctx, owner_filter, Some(2), cursor.as_deref().map(cur))
            .await
            .expect("list_files page");
        prev_cursors.push(page.page_info.prev_cursor.clone());
        let page_ids: Vec<Uuid> = page.items.iter().map(|f| f.file_id).collect();
        seen.extend(page_ids.iter().copied());
        pages.push(page_ids);
        match page.page_info.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(seen.len() <= expected.len(), "walk must terminate");
    }

    assert_eq!(
        seen, expected,
        "cursor walk must cover every file exactly once, in canonical order"
    );
    assert_eq!(
        prev_cursors[0], None,
        "the very first page (no cursor supplied) must have prev_cursor = null"
    );
    assert!(pages.len() > 1, "test must exercise more than one page");

    // Walking back from the last page's `prev_cursor` reproduces the same pages.
    let mut back_pages: Vec<Vec<Uuid>> = Vec::new();
    let mut back_cursor = prev_cursors
        .last()
        .cloned()
        .flatten()
        .expect("the last page must have a predecessor");
    loop {
        let page = svc
            .list_files(&ctx, owner_filter, Some(2), Some(cur(&back_cursor)))
            .await
            .expect("list_files backward page");
        back_pages.push(page.items.iter().map(|f| f.file_id).collect());
        match page.page_info.prev_cursor {
            Some(prev) => back_cursor = prev,
            None => break,
        }
    }
    back_pages.reverse();
    let expected_back = &pages[..pages.len() - 1];
    assert_eq!(
        back_pages, expected_back,
        "walking prev_cursor backward from the last page must reproduce every \
         earlier page, in the same canonical order"
    );
}

#[tokio::test]
async fn files_cursor_rejects_a_different_owner() {
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .build()
        .await;
    let Stack { db, svc, .. } = s;

    let tenant = Uuid::now_v7();
    let owner_a = Uuid::now_v7();
    let owner_b = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    seed_files(&db, tenant, owner_a, &[now, now]).await;
    seed_files(&db, tenant, owner_b, &[now]).await;

    let ctx_a = ctx_for(tenant, owner_a);
    let filter_for_a = OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id: owner_a,
    };
    let page = svc
        .list_files(&ctx_a, filter_for_a, Some(1), None)
        .await
        .expect("first page");
    let cursor = page.page_info.next_cursor.expect("more pages remain");

    let filter_for_b = OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id: owner_b,
    };
    let ctx_b = ctx_for(tenant, owner_b);
    let err = svc
        .list_files(&ctx_b, filter_for_b, Some(1), Some(cur(&cursor)))
        .await
        .expect_err("a cursor issued for a different owner must be rejected");
    assert!(
        matches!(err, DomainError::Pagination(_)),
        "expected a cursor error, got {err:?}"
    );
}

fn files_router(ctx: SecurityContext, svc: Arc<FileService>) -> Router {
    Router::new()
        .route(&format!("{BASE}/files"), get(handlers::list_files))
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(svc))
}

async fn build_files_harness() -> (Arc<FileService>, SecurityContext, Uuid) {
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .build()
        .await;
    let Stack { svc, .. } = s;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    svc.create_file(&ctx_for(tenant, owner), new_file(owner), None, false)
        .await
        .expect("seed one file");
    (svc, ctx_for(tenant, owner), owner)
}

#[tokio::test]
async fn files_garbage_cursor_is_rejected_as_400() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let resp = router
        .oneshot(get_req(format!(
            "{BASE}/files?owner_kind=user&owner_id={owner}&cursor=not-a-real-cursor"
        )))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(
        body.to_string().contains("INVALID_CURSOR"),
        "an undecodable cursor must be INVALID_CURSOR: {body}"
    );
}

#[tokio::test]
async fn files_limit_above_max_is_clamped_to_200() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let resp = router
        .oneshot(get_req(format!(
            "{BASE}/files?owner_kind=user&owner_id={owner}&limit=9999"
        )))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["page_info"]["limit"], 200);
    assert!(body["items"].as_array().unwrap().len() <= 200);
}

fn new_version(file_id: Uuid, version_id: Uuid, created_at: OffsetDateTime) -> FileVersion {
    FileVersion {
        file_id,
        version_id,
        mime_type: "text/plain".to_owned(),
        size: 0,
        hash_algorithm: "SHA-256".to_owned(),
        hash_value: vec![0u8; 32],
        hash_mode: "whole-sha256".to_owned(),
        part_count: None,
        status: VersionStatus::Available,
        is_current: false,
        backend_id: "mem".to_owned(),
        backend_path: format!("/{file_id}/{version_id}"),
        created_at,
        bound_on_finalize: false,
    }
}

async fn seed_file_and_versions(
    db: &Arc<DBProvider<DbError>>,
    tenant: Uuid,
    owner: Uuid,
    instants: &[OffsetDateTime],
) -> (Uuid, Vec<Uuid>) {
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();
    let file_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    files
        .create(
            &conn,
            &scope,
            &File {
                file_id,
                tenant_id: tenant,
                owner_kind: OwnerKind::User,
                owner_id: owner,
                name: "doc.bin".to_owned(),
                gts_file_type: GTS.to_owned(),
                content_id: None,
                meta_version: 0,
                created_at: now,
                last_modified_at: now,
            },
        )
        .await
        .expect("create file");
    let mut ids = Vec::with_capacity(instants.len());
    for &created_at in instants {
        let version_id = Uuid::now_v7();
        versions
            .insert(&conn, &scope, &new_version(file_id, version_id, created_at))
            .await
            .expect("insert version");
        ids.push(version_id);
    }
    (file_id, ids)
}

#[tokio::test]
async fn versions_cursor_walk_covers_all_items_exactly_once_including_created_at_ties() {
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .build()
        .await;
    let Stack { db, svc, .. } = s;

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let base = OffsetDateTime::now_utc();
    let instants = [
        base,
        base,
        base,
        base + time::Duration::seconds(1),
        base + time::Duration::seconds(1),
        base + time::Duration::seconds(2),
    ];
    let (file_id, ids) = seed_file_and_versions(&db, tenant, owner, &instants).await;
    let expected = expected_order(instants.iter().copied().zip(ids.iter().copied()).collect());

    let ctx = ctx_for(tenant, owner);
    let mut seen = Vec::new();
    let mut pages: Vec<Vec<Uuid>> = Vec::new();
    let mut prev_cursors: Vec<Option<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = svc
            .list_versions(&ctx, file_id, Some(2), cursor.as_deref().map(cur))
            .await
            .expect("list_versions page");
        prev_cursors.push(page.page_info.prev_cursor.clone());
        let page_ids: Vec<Uuid> = page.items.iter().map(|v| v.version_id).collect();
        seen.extend(page_ids.iter().copied());
        pages.push(page_ids);
        match page.page_info.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(seen.len() <= expected.len(), "walk must terminate");
    }

    assert_eq!(
        seen, expected,
        "cursor walk must cover every version exactly once, in canonical order"
    );
    assert_eq!(
        prev_cursors[0], None,
        "the very first page (no cursor supplied) must have prev_cursor = null"
    );
    assert!(pages.len() > 1, "test must exercise more than one page");

    let mut back_pages: Vec<Vec<Uuid>> = Vec::new();
    let mut back_cursor = prev_cursors
        .last()
        .cloned()
        .flatten()
        .expect("the last page must have a predecessor");
    loop {
        let page = svc
            .list_versions(&ctx, file_id, Some(2), Some(cur(&back_cursor)))
            .await
            .expect("list_versions backward page");
        back_pages.push(page.items.iter().map(|v| v.version_id).collect());
        match page.page_info.prev_cursor {
            Some(prev) => back_cursor = prev,
            None => break,
        }
    }
    back_pages.reverse();
    let expected_back = &pages[..pages.len() - 1];
    assert_eq!(
        back_pages, expected_back,
        "walking prev_cursor backward from the last page must reproduce every \
         earlier page, in the same canonical order"
    );
}

#[tokio::test]
async fn versions_cursor_rejects_a_different_file() {
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .build()
        .await;
    let Stack { db, svc, .. } = s;

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let (file_a, _) = seed_file_and_versions(&db, tenant, owner, &[now, now]).await;
    let (file_b, _) = seed_file_and_versions(&db, tenant, owner, &[now]).await;

    let ctx = ctx_for(tenant, owner);
    let page = svc
        .list_versions(&ctx, file_a, Some(1), None)
        .await
        .expect("first page");
    let cursor = page.page_info.next_cursor.expect("more pages remain");

    let err = svc
        .list_versions(&ctx, file_b, Some(1), Some(cur(&cursor)))
        .await
        .expect_err("a cursor issued for a different file must be rejected");
    assert!(
        matches!(err, DomainError::Pagination(_)),
        "expected a cursor error, got {err:?}"
    );
}

/// A caller who cannot see the file gets the same answer as without a cursor, even when the cursor
/// was issued for another file: access is checked before the cursor is parsed.
#[tokio::test]
async fn versions_with_manifests_checks_access_before_the_cursor() {
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .build()
        .await;
    let Stack { db, svc, .. } = s;

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let (file_a, _) = seed_file_and_versions(&db, tenant, owner, &[now, now]).await;
    let (file_b, _) = seed_file_and_versions(&db, tenant, owner, &[now]).await;

    let page = svc
        .list_versions(&ctx_for(tenant, owner), file_a, Some(1), None)
        .await
        .expect("first page");
    let foreign_cursor = page.page_info.next_cursor.expect("more pages remain");

    let outsider = ctx_for(Uuid::now_v7(), Uuid::now_v7());
    let without_cursor = svc
        .list_versions_with_manifests(&outsider, file_b, Some(1), None)
        .await
        .expect_err("an outsider must not see the file");
    let with_cursor = svc
        .list_versions_with_manifests(&outsider, file_b, Some(1), Some(cur(&foreign_cursor)))
        .await
        .expect_err("an outsider must not see the file");
    assert!(
        !matches!(with_cursor, DomainError::Pagination(_)),
        "access must be checked before the cursor, got {with_cursor:?}"
    );
    assert_eq!(
        std::mem::discriminant(&with_cursor),
        std::mem::discriminant(&without_cursor),
        "with a cursor: {with_cursor:?}, without: {without_cursor:?}"
    );
}

fn versions_router(ctx: SecurityContext, svc: Arc<FileService>) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/files/{{id}}/versions"),
            get(handlers::list_versions),
        )
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(svc))
}

async fn build_retention_harness() -> (Arc<FileService>, Arc<PolicyService>, Uuid, Uuid) {
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .authorizer(Arc::new(ScopedTestAuthorizer::new()))
        .build()
        .await;
    let Stack { psvc, svc, .. } = s;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    (svc, psvc, tenant, subject)
}

#[tokio::test]
async fn retention_rules_cursor_walk_covers_every_rule_in_tenant() {
    let (svc, policy_svc, tenant, subject) = build_retention_harness().await;
    let ctx_subject = ctx_for(tenant, subject);

    let mut created = Vec::new();
    for _ in 0..5 {
        let r = policy_svc
            .create_retention_rule(
                &ctx_subject,
                RetentionScope::Tenant,
                None,
                valid_rule_body(),
            )
            .await
            .expect("create rule");
        created.push(r.rule_id);
    }
    let _ = &svc;

    let mut seen = Vec::new();
    let mut pages: Vec<Vec<Uuid>> = Vec::new();
    let mut prev_cursors: Vec<Option<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = policy_svc
            .list_retention_rules(&ctx_subject, Some(2), cursor.as_deref().map(cur))
            .await
            .expect("list_retention_rules page");
        prev_cursors.push(page.page_info.prev_cursor.clone());
        let page_ids: Vec<Uuid> = page.items.iter().map(|r| r.rule_id).collect();
        seen.extend(page_ids.iter().copied());
        pages.push(page_ids);
        match page.page_info.next_cursor.clone() {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(seen.len() <= created.len(), "walk must terminate");
    }

    let seen_set: std::collections::HashSet<Uuid> = seen.iter().copied().collect();
    let expected_set: std::collections::HashSet<Uuid> = created.into_iter().collect();
    assert_eq!(
        seen_set, expected_set,
        "the walk must return every rule in the tenant"
    );
    assert_eq!(
        prev_cursors[0], None,
        "the very first page (no cursor supplied) must have prev_cursor = null"
    );
    assert!(pages.len() > 1, "test must exercise more than one page");

    let mut back_pages: Vec<Vec<Uuid>> = Vec::new();
    let mut back_cursor = prev_cursors
        .last()
        .cloned()
        .flatten()
        .expect("the last page must have a predecessor");
    loop {
        let page = policy_svc
            .list_retention_rules(&ctx_subject, Some(2), Some(cur(&back_cursor)))
            .await
            .expect("list_retention_rules backward page");
        back_pages.push(page.items.iter().map(|r| r.rule_id).collect());
        match page.page_info.prev_cursor {
            Some(prev) => back_cursor = prev,
            None => break,
        }
    }
    back_pages.reverse();
    let expected_back = &pages[..pages.len() - 1];
    assert_eq!(
        back_pages, expected_back,
        "walking prev_cursor backward from the last page must reproduce every \
         earlier page, in the same canonical order"
    );
}

fn retention_rules_router(ctx: SecurityContext, policy_svc: Arc<PolicyService>) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/retention-rules"),
            get(handlers::list_retention_rules),
        )
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(policy_svc))
}

/// A cursor built under a different order must be rejected (`ORDER_MISMATCH`), in either direction.
#[tokio::test]
async fn retention_rules_cursor_with_wrong_order_field_is_rejected() {
    let (_svc, policy_svc, tenant, subject) = build_retention_harness().await;
    for direction in [
        pagination::Direction::Forward,
        pagination::Direction::Backward,
    ] {
        let bogus = pagination::encode(
            OffsetDateTime::now_utc(),
            Uuid::now_v7(),
            pagination::FILES_ID_FIELD, // wrong id field for /retention-rules
            None,
            direction,
        )
        .expect("encode");
        let err = policy_svc
            .list_retention_rules(&ctx_for(tenant, subject), Some(2), Some(cur(&bogus)))
            .await
            .expect_err("a cursor built for a different listing's order must be rejected");
        assert!(
            matches!(err, DomainError::Pagination(_)),
            "{direction:?}: got {err:?}"
        );
    }
}

// --- REST query-string contract: `$top`/`$skiptoken` synonyms and rejected options ---

/// `(field, reason)` of every violation of a canonical 400 body, in order.
fn violations(body: &Value) -> Vec<(String, String)> {
    body["context"]["field_violations"]
        .as_array()
        .unwrap_or_else(|| panic!("no field_violations in {body}"))
        .iter()
        .map(|v| {
            (
                v["field"].as_str().expect("field").to_owned(),
                v["reason"].as_str().expect("reason").to_owned(),
            )
        })
        .collect()
}

async fn assert_rejected(router: &Router, uri: String, expected: &[(&str, &str)]) -> Value {
    let resp = router
        .clone()
        .oneshot(get_req(uri.clone()))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
    let body = body_json(resp).await;
    let got = violations(&body);
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(f, r)| ((*f).to_owned(), (*r).to_owned()))
        .collect();
    assert_eq!(got, expected, "{uri}: {body}");
    body
}

async fn get_ok(router: &Router, uri: String) -> Value {
    let resp = router
        .clone()
        .oneshot(get_req(uri.clone()))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    body_json(resp).await
}

/// Query-string contract of `/files`: every rejected option and the exact violations reported.
/// Each row is `(query, expected (field, reason) violations, text the first description holds)`.
#[tokio::test]
async fn files_query_contract() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let prefix = format!("{BASE}/files?owner_kind=user&owner_id={owner}");
    let unsupported = "UNSUPPORTED_QUERY_PARAM";
    let invalid_params = "INVALID_QUERY_PARAMS";
    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, Vec<(&str, &str)>, &[&str])> = vec![
        // The old offset param is rejected, not silently ignored.
        ("offset=0".into(), vec![("offset", unsupported)], &[]),
        ("limit=0".into(), vec![("$top", "INVALID_LIMIT")], &[]),
        ("$top=0".into(), vec![("$top", "INVALID_LIMIT")], &[]),
        // Both spellings of one parameter: the violation names the `OData` spelling.
        ("limit=1&$top=1".into(), vec![("$top", invalid_params)], &[]),
        (
            "cursor=X&$skiptoken=X".into(),
            vec![("$skiptoken", invalid_params)],
            &[],
        ),
        // A repeated page parameter is reported together with the other violations.
        (
            "$top=1&$top=2&$filter=x".into(),
            vec![("$top", invalid_params), ("$filter", unsupported)],
            &[],
        ),
        (
            "limit=1&limit=2".into(),
            vec![("$top", invalid_params)],
            &[],
        ),
        (
            "$skip=1".into(),
            vec![("$skip", unsupported)],
            &["$skiptoken"],
        ),
        // Unsupported options are rejected at any value, including empty and malformed ones.
        (
            "$filter=name%20eq%20'a'".into(),
            vec![("$filter", unsupported)],
            &[],
        ),
        (
            "$orderby=created_at%20desc".into(),
            vec![("$orderby", unsupported)],
            &[],
        ),
        (
            "$select=file_id".into(),
            vec![("$select", unsupported)],
            &[],
        ),
        ("$filter=".into(), vec![("$filter", unsupported)], &[]),
        ("$orderby=".into(), vec![("$orderby", unsupported)], &[]),
        ("$select=".into(), vec![("$select", unsupported)], &[]),
        (
            "$filter=name%20eq".into(),
            vec![("$filter", unsupported)],
            &[],
        ),
        // Still an unsupported key, not a cursor error.
        (
            "$orderby=created_at%20desc&cursor=x".into(),
            vec![("$orderby", unsupported)],
            &[],
        ),
        ("$Top=1".into(), vec![("$Top", unsupported)], &[]),
        (
            "owner_id=00000000-0000-0000-0000-000000000000".into(),
            vec![("owner_id", invalid_params)],
            &[],
        ),
        // All offences of one request come back together, in order.
        (
            "$skip=1&$filter=a%20eq%201&offset=0".into(),
            vec![
                ("$skip", unsupported),
                ("$filter", unsupported),
                ("offset", unsupported),
            ],
            &[],
        ),
        (
            "offset=0&foo=1&offset=2".into(),
            vec![("offset", unsupported), ("foo", unsupported)],
            &["owner_kind", "$top"],
        ),
    ];
    for (query, expected, description_holds) in rows {
        let body = assert_rejected(&router, format!("{prefix}&{query}"), &expected).await;
        let description = body["context"]["field_violations"][0]["description"]
            .as_str()
            .expect("description");
        for text in description_holds {
            assert!(
                description.contains(text),
                "{query}: the description must mention {text}: {description}"
            );
        }
    }
}

#[tokio::test]
async fn files_skiptoken_is_a_synonym_of_cursor() {
    let (svc, subject, owner) = build_files_harness().await;
    svc.create_file(&subject, new_file(owner), None, false)
        .await
        .expect("seed a second file");
    let router = files_router(subject, svc);
    let first = get_ok(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&$top=1"),
    )
    .await;
    assert_eq!(first["page_info"]["limit"], 1, "$top is a synonym of limit");
    let next = first["page_info"]["next_cursor"]
        .as_str()
        .expect("a second file exists");
    let second = get_ok(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&$skiptoken={next}"),
    )
    .await;
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_ne!(
        second["items"][0]["file_id"], first["items"][0]["file_id"],
        "the continuation page must hold the other file"
    );
}

#[tokio::test]
async fn invalid_path_uuid_is_problem_json() {
    let (svc, subject, _owner) = build_files_harness().await;
    let router = versions_router(subject, svc);
    let resp = router
        .oneshot(get_req(format!("{BASE}/files/not-a-uuid/versions")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        content_type.starts_with("application/problem+json"),
        "{content_type}"
    );
}

/// Expected `(field, reason)` violations of a rejected request.
type Violations<'a> = &'a [(&'a str, &'a str)];

/// The three listings that take no owner pair share one query contract.
async fn assert_plain_listing_contract(router: &Router, path: &str) {
    let body = get_ok(router, format!("{BASE}{path}?$top=1")).await;
    assert_eq!(body["page_info"]["limit"], 1, "{path}");

    let unsupported = "UNSUPPORTED_QUERY_PARAM";
    let rows: [(&str, Violations); 5] = [
        ("$skip=1", &[("$skip", unsupported)]),
        ("foo=1", &[("foo", unsupported)]),
        ("offset=0", &[("offset", unsupported)]),
        ("limit=0", &[("$top", "INVALID_LIMIT")]),
        (
            "$filter=x%20eq%201&$orderby=id",
            &[("$filter", unsupported), ("$orderby", unsupported)],
        ),
    ];
    for (query, expected) in rows {
        assert_rejected(router, format!("{BASE}{path}?{query}"), expected).await;
    }

    let resp = router
        .clone()
        .oneshot(get_req(format!("{BASE}{path}?cursor=not-a-real-cursor")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{path}");
    let body = body_json(resp).await;
    assert!(
        body.to_string().contains("INVALID_CURSOR"),
        "{path}: an undecodable cursor must be INVALID_CURSOR: {body}"
    );
}

#[tokio::test]
async fn versions_query_contract() {
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .build()
        .await;
    let Stack { db, svc, .. } = s;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let (file_id, _) = seed_file_and_versions(
        &db,
        tenant,
        owner,
        &[OffsetDateTime::now_utc(), OffsetDateTime::now_utc()],
    )
    .await;
    let router = versions_router(ctx_for(tenant, owner), svc);
    assert_plain_listing_contract(&router, &format!("/files/{file_id}/versions")).await;
}

#[tokio::test]
async fn retention_rules_query_contract() {
    let (_svc, policy_svc, tenant, subject) = build_retention_harness().await;
    let router = retention_rules_router(ctx_for(tenant, subject), policy_svc);
    assert_plain_listing_contract(&router, "/retention-rules").await;
}

fn storages_router(ctx: SecurityContext, svc: Arc<FileService>) -> Router {
    Router::new()
        .route(&format!("{BASE}/files"), get(handlers::list_files))
        .route(&format!("{BASE}/storages"), get(handlers::list_storages))
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(svc))
}

/// Three backends registered out of id order, plus two files for cursors of another listing.
async fn build_storages_harness() -> (Router, Uuid) {
    let backends: Vec<Arc<dyn StorageBackend>> = ["mem-b", "mem-c", "mem-a"]
        .into_iter()
        .map(|id| Arc::new(InMemoryBackend::new(id)) as Arc<dyn StorageBackend>)
        .collect();
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .backends(backends, "mem-a")
        .build()
        .await;
    let Stack { svc, .. } = s;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    for _ in 0..2 {
        svc.create_file(&ctx_for(tenant, owner), new_file(owner), None, false)
            .await
            .expect("seed file");
    }
    (storages_router(ctx_for(tenant, owner), svc), owner)
}

fn storage_ids(body: &Value) -> Vec<&str> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|s| s["id"].as_str().expect("id"))
        .collect()
}

#[tokio::test]
async fn storages_are_paged_by_id_in_both_directions() {
    let (router, _owner) = build_storages_harness().await;

    let all = get_ok(&router, format!("{BASE}/storages")).await;
    assert_eq!(storage_ids(&all), ["mem-a", "mem-b", "mem-c"]);
    assert!(all["page_info"]["next_cursor"].is_null());
    assert!(all["page_info"]["prev_cursor"].is_null());

    let first = get_ok(&router, format!("{BASE}/storages?limit=1")).await;
    assert_eq!(storage_ids(&first), ["mem-a"]);
    assert_eq!(first["page_info"]["limit"], 1);
    assert!(first["page_info"]["prev_cursor"].is_null());
    let next = first["page_info"]["next_cursor"]
        .as_str()
        .expect("more storages follow");

    let second = get_ok(
        &router,
        format!("{BASE}/storages?limit=1&$skiptoken={next}"),
    )
    .await;
    assert_eq!(storage_ids(&second), ["mem-b"]);
    let prev = second["page_info"]["prev_cursor"]
        .as_str()
        .expect("a page precedes");
    assert!(second["page_info"]["next_cursor"].is_string());

    let back = get_ok(&router, format!("{BASE}/storages?limit=1&cursor={prev}")).await;
    assert_eq!(storage_ids(&back), ["mem-a"]);
    assert!(back["page_info"]["prev_cursor"].is_null());
    assert!(back["page_info"]["next_cursor"].is_string());
}

#[tokio::test]
async fn storages_reject_unsupported_options_and_foreign_cursors() {
    let (router, owner) = build_storages_harness().await;
    assert_plain_listing_contract(&router, "/storages").await;

    let files = get_ok(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&limit=1"),
    )
    .await;
    let files_cursor = files["page_info"]["next_cursor"]
        .as_str()
        .expect("a second file exists");
    let resp = router
        .clone()
        .oneshot(get_req(format!("{BASE}/storages?cursor={files_cursor}")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(
        body.to_string().contains("ORDER_MISMATCH"),
        "a /files cursor must be an order mismatch on /storages: {body}"
    );
}

/// Inserts a composite version with a manifest, returning its id.
async fn insert_composite_version(
    db: &Arc<DBProvider<DbError>>,
    file_id: Uuid,
    created_at: OffsetDateTime,
    manifest_text: &str,
) -> Uuid {
    let conn = db.conn().expect("conn");
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();
    let version_id = Uuid::now_v7();
    let mut v = new_version(file_id, version_id, created_at);
    "multipart-composite-sha256".clone_into(&mut v.hash_mode);
    v.part_count = Some(2);
    versions
        .insert(&conn, &scope, &v)
        .await
        .expect("insert composite version");
    versions
        .insert_manifest(&conn, &scope, version_id, manifest_text, created_at)
        .await
        .expect("insert manifest");
    version_id
}

/// Three composite versions whose 3 MiB manifests each exceed half of the 4 MiB page budget, so
/// every `list_versions_with_manifests` page holds exactly one row. Returns the service, caller
/// context, file id and the version ids newest first: `[v2, v1, v0]`.
async fn seed_budget_busting_versions() -> (Arc<FileService>, SecurityContext, Uuid, [Uuid; 3]) {
    let s = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .build()
        .await;
    let Stack { db, svc, .. } = s;

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let base = OffsetDateTime::now_utc();
    let (file_id, _) = seed_file_and_versions(&db, tenant, owner, &[]).await;

    let manifest_text = "x".repeat(3 * 1024 * 1024);
    let mut version_ids = Vec::new();
    for i in 0..3i64 {
        let created_at = base + time::Duration::seconds(i);
        version_ids.push(insert_composite_version(&db, file_id, created_at, &manifest_text).await);
    }
    // Newest-first order: v2 (base+2), v1 (base+1), v0 (base).
    (
        svc,
        ctx_for(tenant, owner),
        file_id,
        [version_ids[2], version_ids[1], version_ids[0]],
    )
}

fn version_ids_of(page: &toolkit_odata::Page<(FileVersion, Option<String>)>) -> Vec<Uuid> {
    page.items.iter().map(|(v, _)| v.version_id).collect()
}

/// A forward page cut short by the manifest budget keeps the leading rows (adjacent to the
/// cursor) and rebuilds `next_cursor` from the last kept row.
#[tokio::test]
async fn versions_manifest_budget_truncation_forces_next_cursor_with_no_gap_or_duplicate() {
    let (svc, ctx, file_id, [v2, v1, v0]) = seed_budget_busting_versions().await;

    let page1 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), None)
        .await
        .expect("page 1");
    assert_eq!(
        version_ids_of(&page1),
        vec![v2],
        "the manifest budget must cut the page short after the first version"
    );
    assert!(
        page1.page_info.prev_cursor.is_none(),
        "page 1 has no earlier page"
    );
    let cursor = page1
        .page_info
        .next_cursor
        .clone()
        .expect("a budget-truncated page must still carry a next_cursor");

    let page2 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&cursor)))
        .await
        .expect("page 2");
    assert_eq!(
        version_ids_of(&page2),
        vec![v1],
        "page 2 must resume exactly at v1 -- no gap, no repeat of v2"
    );
    let cursor2 = page2
        .page_info
        .next_cursor
        .clone()
        .expect("page 2 is budget-truncated too, so it must also carry a next_cursor");

    let page3 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&cursor2)))
        .await
        .expect("page 3");
    assert_eq!(
        version_ids_of(&page3),
        vec![v0],
        "page 3 must resume exactly at v0 -- no gap, no repeat of v1"
    );
    assert!(
        page3.page_info.next_cursor.is_none(),
        "must be the last page"
    );
}

/// A backward page cut short by the manifest budget keeps the TRAILING rows (the ones adjacent to
/// its cursor in canonical newest-first order) and rebuilds `prev_cursor` from the first kept row;
/// `next_cursor` stays valid. Walking back from the last page then loses and repeats nothing.
#[tokio::test]
async fn versions_manifest_budget_truncation_on_a_backward_page_keeps_rows_adjacent_to_the_cursor()
{
    let (svc, ctx, file_id, [v2, v1, v0]) = seed_budget_busting_versions().await;

    // Walk forward to the last page to obtain a genuine `prev_cursor` positioned at v0.
    let mut cursor = None;
    let mut last_page = None;
    for _ in 0..3 {
        let page = svc
            .list_versions_with_manifests(&ctx, file_id, Some(10), cursor.as_deref().map(cur))
            .await
            .expect("forward walk");
        cursor = page.page_info.next_cursor.clone();
        last_page = Some(page);
    }
    let last_page = last_page.unwrap();
    assert_eq!(version_ids_of(&last_page), vec![v0]);
    let prev_from_v0 = last_page
        .page_info
        .prev_cursor
        .clone()
        .expect("the last page has an earlier page");

    // Backward from v0: v2 and v1 are both newer, but the budget fits only one manifest. The row
    // adjacent to the cursor (v1) must be kept, not the far one (v2).
    let back1 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&prev_from_v0)))
        .await
        .expect("backward page 1");
    assert_eq!(
        version_ids_of(&back1),
        vec![v1],
        "a truncated backward page must keep the row adjacent to its cursor"
    );
    assert!(
        back1.items[0].1.is_some(),
        "the kept composite version still carries its manifest"
    );
    let prev_from_v1 = back1
        .page_info
        .prev_cursor
        .clone()
        .expect("truncation must leave a prev_cursor for the dropped newer row");
    let next_from_v1 = back1
        .page_info
        .next_cursor
        .clone()
        .expect("the opposite-direction cursor must stay valid");

    // The rebuilt `prev_cursor` re-serves exactly the dropped row, nothing more or less.
    let back2 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&prev_from_v1)))
        .await
        .expect("backward page 2");
    assert_eq!(
        version_ids_of(&back2),
        vec![v2],
        "backward page 2 must resume exactly at v2 -- no gap, no repeat of v1"
    );
    assert!(
        back2.page_info.prev_cursor.is_none(),
        "v2 is the newest version: nothing earlier"
    );

    // The untouched `next_cursor` of the truncated backward page leads forward to v0 again.
    let forward = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(cur(&next_from_v1)))
        .await
        .expect("forward from the backward page");
    assert_eq!(version_ids_of(&forward), vec![v0]);
}

/// `GET /files/{id}/versions` serializes versions (manifest lookup batched for composite ones):
/// a whole-sha256 version carries no manifest, a page of whole-sha256 versions is never capped
/// below `?limit`, and a composite version carries its non-empty manifest.
#[tokio::test]
async fn list_versions_endpoint_serializes_whole_and_composite_versions() {
    let Stack { db, svc, .. } = StackBuilder::new("cursor-pagination")
        .config(base_config())
        .build()
        .await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let router = versions_router(ctx_for(tenant, owner), svc);
    let now = OffsetDateTime::now_utc();

    // One whole-sha256 version: no manifest.
    let (file_id, ids) = seed_file_and_versions(&db, tenant, owner, &[now]).await;
    let body = get_ok(&router, format!("{BASE}/files/{file_id}/versions")).await;
    let items = body["items"].as_array().expect("array body");
    assert_eq!(items.len(), 1, "expected exactly the one version");
    assert_eq!(items[0]["version_id"], ids[0].to_string().as_str());
    assert!(
        items[0].get("manifest").is_none() || items[0]["manifest"].is_null(),
        "a whole-sha256 version must carry no manifest"
    );

    // Eight whole-sha256 versions (no manifests) are never capped below `?limit`.
    let (file_id, _) = seed_file_and_versions(&db, tenant, owner, &[now; 8]).await;
    let body = get_ok(&router, format!("{BASE}/files/{file_id}/versions?limit=8")).await;
    assert_eq!(body["items"].as_array().expect("array body").len(), 8);

    // A composite version carries its manifest.
    let (file_id, _) = seed_file_and_versions(&db, tenant, owner, &[]).await;
    insert_composite_version(&db, file_id, now, "v1,0:aa").await;
    let body = get_ok(&router, format!("{BASE}/files/{file_id}/versions")).await;
    let items = body["items"].as_array().expect("array body");
    assert_eq!(items.len(), 1, "expected exactly the one version");
    assert_eq!(items[0]["hash_mode"], "multipart-composite-sha256");
    let manifest = items[0]["manifest"]
        .as_str()
        .expect("composite version must carry a non-null manifest string");
    assert!(!manifest.is_empty(), "manifest must not be empty");
}
