//! REST layer of the control plane through a real `axum::Router`: the query contract shared by the
//! four paginated listings, request validation, and the status each `DomainError` maps to.
//! `expected_status` has no wildcard arm, so a new variant fails to compile until covered.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::{get, patch, post};
use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::handlers;
use file_storage::domain::error::DomainError;
use file_storage::infra::backend::{InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Verifier;
use file_storage_sdk::NewFile;

mod common;
use common::{
    Stack, StackBuilder, body_json, ctx_for, get_req, insert_composite_version, problem_body,
    seed_file_with_versions, token_of, violations,
};

const BASE: &str = "/api/file-storage/v1";
const UNSUPPORTED: &str = "UNSUPPORTED_QUERY_PARAM";
const INVALID_PARAMS: &str = "INVALID_QUERY_PARAMS";

/// A stack with the REST handlers mounted for one caller, and one seeded file of that caller.
struct Rest {
    stack: Stack,
    router: Router,
    ctx: SecurityContext,
    owner: Uuid,
    file_id: Uuid,
}

impl Rest {
    async fn new() -> Self {
        Self::over(StackBuilder::new("api-rest").page_sizes(25, 200)).await
    }

    async fn over(builder: StackBuilder) -> Self {
        let stack = builder.build().await;
        let (tenant, owner) = (Uuid::now_v7(), Uuid::now_v7());
        let ctx = ctx_for(tenant, owner);
        let file_id = stack
            .svc
            .create_file(&ctx, new_file(owner), None, false)
            .await
            .expect("seed one file")
            .file_id;
        let router = Router::new()
            .route(&format!("{BASE}/files"), get(handlers::list_files))
            .route(
                &format!("{BASE}/files/{{id}}"),
                patch(handlers::update_metadata),
            )
            .route(
                &format!("{BASE}/files/{{id}}/versions"),
                get(handlers::list_versions),
            )
            .route(
                &format!("{BASE}/files/{{file_id}}/versions/{{version_id}}/finalize"),
                post(handlers::finalize_version),
            )
            .route(
                &format!("{BASE}/retention-rules"),
                get(handlers::list_retention_rules),
            )
            .route(&format!("{BASE}/storages"), get(handlers::list_storages))
            .layer(axum::Extension(ctx.clone()))
            .layer(axum::Extension(Arc::clone(&stack.svc)))
            .layer(axum::Extension(Arc::clone(&stack.psvc)))
            .layer(axum::Extension(
                Arc::new(stack.issuer.verifier()) as Arc<Verifier>
            ))
            .layer(axum::Extension(Arc::new(handlers::FinalizeAuth::new(
                "test-internal-secret".to_owned(),
                time::Duration::ZERO,
            ))));
        Self {
            stack,
            router,
            ctx,
            owner,
            file_id,
        }
    }

    async fn send(&self, req: Request<Body>) -> Response {
        self.router
            .clone()
            .oneshot(req)
            .await
            .expect("router dispatch")
    }

    async fn get(&self, uri: &str) -> Response {
        self.send(get_req(uri)).await
    }

    async fn get_ok(&self, uri: &str) -> Value {
        let resp = self.get(uri).await;
        assert_eq!(resp.status().as_u16(), 200, "{uri}");
        body_json(resp).await
    }

    /// `GET uri` answers a 400 problem whose field violations are exactly `expected`.
    async fn assert_rejected(&self, uri: &str, expected: &[(&str, &str)]) -> Value {
        let body = problem_body(self.get(uri).await, 400).await;
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(f, r)| ((*f).to_owned(), (*r).to_owned()))
            .collect();
        assert_eq!(violations(&body), expected, "{uri}: {body}");
        body
    }

    /// `GET uri` answers a 400 problem carrying `code` (e.g. `INVALID_CURSOR`).
    async fn assert_rejected_with(&self, uri: &str, code: &str) {
        let body = problem_body(self.get(uri).await, 400).await;
        assert!(
            body.to_string().contains(code),
            "{uri}: expected {code}: {body}"
        );
    }
}

fn new_file(owner_id: Uuid) -> NewFile {
    common::new_file_for(owner_id, "doc.bin", "text/plain")
}

/// `(query, expected (field, reason) violations, text the first description holds)`.
type QueryRow<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a [&'a str]);

/// Query-string contract of `/files`: every rejected option and the exact violations reported.
/// The extractor looks at which keys are present, never at their values, so one value per
/// distinct rule is enough.
#[tokio::test]
async fn files_query_contract() {
    let rest = Rest::new().await;
    let prefix = format!("{BASE}/files?owner_kind=user&owner_id={}", rest.owner);
    let rows: Vec<QueryRow> = vec![
        // The old offset param is rejected, not silently ignored.
        ("offset=0", vec![("offset", UNSUPPORTED)], &[]),
        ("limit=0", vec![("$top", "INVALID_LIMIT")], &[]),
        // Both spellings of one parameter: the violation names the `OData` spelling.
        ("limit=1&$top=1", vec![("$top", INVALID_PARAMS)], &[]),
        ("limit=1&limit=2", vec![("$top", INVALID_PARAMS)], &[]),
        // A repeated page parameter is reported together with the other violations, in order.
        (
            "$top=1&$top=2&$filter=x",
            vec![("$top", INVALID_PARAMS), ("$filter", UNSUPPORTED)],
            &[],
        ),
        ("$skip=1", vec![("$skip", UNSUPPORTED)], &["$skiptoken"]),
        (
            "$filter=name%20eq%20'a'",
            vec![("$filter", UNSUPPORTED)],
            &[],
        ),
        (
            "$orderby=created_at%20desc",
            vec![("$orderby", UNSUPPORTED)],
            &[],
        ),
        ("$select=file_id", vec![("$select", UNSUPPORTED)], &[]),
        // Still an unsupported key, not a cursor error.
        (
            "$orderby=created_at%20desc&cursor=x",
            vec![("$orderby", UNSUPPORTED)],
            &[],
        ),
        ("$Top=1", vec![("$Top", UNSUPPORTED)], &[]),
        (
            "owner_id=00000000-0000-0000-0000-000000000000",
            vec![("owner_id", INVALID_PARAMS)],
            &[],
        ),
        (
            "offset=0&foo=1&offset=2",
            vec![("offset", UNSUPPORTED), ("foo", UNSUPPORTED)],
            &["owner_kind", "$top"],
        ),
    ];
    for (query, expected, description_holds) in rows {
        let body = rest
            .assert_rejected(&format!("{prefix}&{query}"), &expected)
            .await;
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

/// `$top` / `$skiptoken` are synonyms of `limit` / `cursor`; the limit is clamped to the maximum
/// page size and an undecodable cursor is `INVALID_CURSOR`.
#[tokio::test]
async fn files_page_parameters_accept_odata_synonyms_and_clamp_the_limit() {
    let rest = Rest::new().await;
    rest.stack
        .svc
        .create_file(&rest.ctx, new_file(rest.owner), None, false)
        .await
        .expect("seed a second file");
    let files = format!("{BASE}/files?owner_kind=user&owner_id={}", rest.owner);

    let first = rest.get_ok(&format!("{files}&$top=1")).await;
    assert_eq!(first["page_info"]["limit"], 1, "$top is a synonym of limit");
    let next = first["page_info"]["next_cursor"]
        .as_str()
        .expect("a second file exists");
    let second = rest.get_ok(&format!("{files}&$skiptoken={next}")).await;
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_ne!(
        second["items"][0]["file_id"], first["items"][0]["file_id"],
        "the continuation page must hold the other file"
    );

    let clamped = rest.get_ok(&format!("{files}&limit=9999")).await;
    assert_eq!(clamped["page_info"]["limit"], 200);

    rest.assert_rejected_with(
        &format!("{files}&cursor=not-a-real-cursor"),
        "INVALID_CURSOR",
    )
    .await;
}

/// `/versions`, `/retention-rules` and `/storages` share the `/files` extractor: a rejected option,
/// `$top` and a garbage cursor behave alike on each.
#[tokio::test]
async fn other_listings_share_the_query_contract() {
    let rest = Rest::new().await;
    for path in [
        format!("/files/{}/versions", rest.file_id),
        "/retention-rules".to_owned(),
        "/storages".to_owned(),
    ] {
        let uri = |query: &str| format!("{BASE}{path}?{query}");
        let body = rest.get_ok(&uri("$top=1")).await;
        assert_eq!(body["page_info"]["limit"], 1, "{path}");
        rest.assert_rejected(
            &uri("$skip=1&foo=1"),
            &[("$skip", UNSUPPORTED), ("foo", UNSUPPORTED)],
        )
        .await;
        rest.assert_rejected_with(&uri("cursor=not-a-real-cursor"), "INVALID_CURSOR")
            .await;
    }
}

fn storage_ids(body: &Value) -> Vec<&str> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|s| s["id"].as_str().expect("id"))
        .collect()
}

/// Three backends registered out of id order are listed by id and paged in both directions; a
/// cursor of the `/files` listing is an order mismatch here.
#[tokio::test]
async fn storages_are_paged_by_id_in_both_directions() {
    let backends: Vec<Arc<dyn StorageBackend>> = ["mem-b", "mem-c", "mem-a"]
        .into_iter()
        .map(|id| Arc::new(InMemoryBackend::new(id)) as Arc<dyn StorageBackend>)
        .collect();
    let rest = Rest::over(
        StackBuilder::new("api-rest")
            .page_sizes(25, 200)
            .backends(backends, "mem-a"),
    )
    .await;
    let storages = format!("{BASE}/storages");

    let all = rest.get_ok(&storages).await;
    assert_eq!(storage_ids(&all), ["mem-a", "mem-b", "mem-c"]);
    assert!(all["page_info"]["next_cursor"].is_null());
    assert!(all["page_info"]["prev_cursor"].is_null());

    let first = rest.get_ok(&format!("{storages}?limit=1")).await;
    assert_eq!(storage_ids(&first), ["mem-a"]);
    assert!(first["page_info"]["prev_cursor"].is_null());
    let next = first["page_info"]["next_cursor"]
        .as_str()
        .expect("more storages follow");

    let second = rest
        .get_ok(&format!("{storages}?limit=1&$skiptoken={next}"))
        .await;
    assert_eq!(storage_ids(&second), ["mem-b"]);
    let prev = second["page_info"]["prev_cursor"]
        .as_str()
        .expect("a page precedes");
    assert!(second["page_info"]["next_cursor"].is_string());

    let back = rest
        .get_ok(&format!("{storages}?limit=1&cursor={prev}"))
        .await;
    assert_eq!(storage_ids(&back), ["mem-a"]);
    assert!(back["page_info"]["prev_cursor"].is_null());
    assert!(back["page_info"]["next_cursor"].is_string());

    rest.stack
        .svc
        .create_file(&rest.ctx, new_file(rest.owner), None, false)
        .await
        .expect("seed a second file");
    let files = rest
        .get_ok(&format!(
            "{BASE}/files?owner_kind=user&owner_id={}&limit=1",
            rest.owner
        ))
        .await;
    let files_cursor = files["page_info"]["next_cursor"]
        .as_str()
        .expect("a second file exists");
    rest.assert_rejected_with(
        &format!("{storages}?cursor={files_cursor}"),
        "ORDER_MISMATCH",
    )
    .await;
}

#[tokio::test]
async fn invalid_path_uuid_is_problem_json() {
    let rest = Rest::new().await;
    problem_body(
        rest.get(&format!("{BASE}/files/not-a-uuid/versions")).await,
        400,
    )
    .await;
}

/// `GET /files/{id}/versions` serializes whole-sha256 versions without a manifest and never caps
/// a page below `?limit` (the manifest budget applies only to composite versions).
#[tokio::test]
async fn list_versions_whole_versions_are_not_capped_and_carry_no_manifest() {
    let rest = Rest::new().await;
    let now = OffsetDateTime::now_utc();
    let (file_id, _) = seed_file_with_versions(
        &rest.stack.db,
        rest.ctx.subject_tenant_id(),
        rest.owner,
        &[now; 8],
    )
    .await;

    let body = rest
        .get_ok(&format!("{BASE}/files/{file_id}/versions?limit=8"))
        .await;
    let items = body["items"].as_array().expect("array body");
    assert_eq!(items.len(), 8);
    assert!(
        items
            .iter()
            .all(|i| i["hash_mode"] == "whole-sha256" && i.get("manifest").is_none()),
        "{body}"
    );
}

/// A composite version is serialized with its non-empty manifest.
#[tokio::test]
async fn list_versions_composite_version_carries_its_manifest() {
    let rest = Rest::new().await;
    let (file_id, _) = seed_file_with_versions(
        &rest.stack.db,
        rest.ctx.subject_tenant_id(),
        rest.owner,
        &[],
    )
    .await;
    insert_composite_version(
        &rest.stack.db,
        file_id,
        OffsetDateTime::now_utc(),
        "v1,0:aa",
    )
    .await;

    let body = rest
        .get_ok(&format!("{BASE}/files/{file_id}/versions"))
        .await;
    let items = body["items"].as_array().expect("array body");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["hash_mode"], "multipart-composite-sha256");
    assert_eq!(items[0]["manifest"], "v1,0:aa");
}

/// A digest that is not 32 bytes (truncated or oversized SHA-256) is rejected by the handler with
/// a `hash_hex` violation, not a downstream 500.
#[tokio::test]
async fn finalize_wrong_length_hash_hex_is_rejected() {
    let rest = Rest::new().await;
    let ticket = rest
        .stack
        .svc
        .create_file(&rest.ctx, new_file(rest.owner), None, false)
        .await
        .expect("create file");
    for hash_byte_len in [16, 48] {
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "{BASE}/files/{}/versions/{}/finalize",
                ticket.file_id, ticket.version_id
            ))
            .header("content-type", "application/json")
            .header("x-fs-token", token_of(&ticket.upload_url))
            .header("x-fs-internal-token", "test-internal-secret")
            .body(Body::from(
                json!({ "size": 5, "hash_hex": hex::encode(vec![0u8; hash_byte_len]) }).to_string(),
            ))
            .expect("build request");
        let body = problem_body(rest.send(req).await, 400).await;
        assert!(
            violations(&body)
                .iter()
                .any(|(field, _)| field == "hash_hex"),
            "{hash_byte_len} bytes: expected a hash_hex violation: {body}"
        );
    }
}

/// `If-Match-Metadata` through the router: a present-but-unparseable header is a validation error
/// (not silently an unconditional patch), a stale well-formed one a precondition failure (also
/// HTTP 400, but a different violation), an absent one applies unconditionally.
#[tokio::test]
async fn patch_metadata_if_match_header_handling() {
    let rest = Rest::new().await;
    let patch_with = |header: Option<&str>| {
        let mut req = Request::builder()
            .method("PATCH")
            .uri(format!("{BASE}/files/{}", rest.file_id))
            .header("content-type", "application/json");
        if let Some(value) = header {
            req = req.header("if-match-metadata", value);
        }
        rest.send(
            req.body(Body::from(
                json!({ "custom_metadata": { "k": "v" } }).to_string(),
            ))
            .expect("build request"),
        )
    };

    let malformed = problem_body(patch_with(Some("not-a-number")).await, 400).await;
    assert_eq!(
        violations(&malformed)
            .first()
            .map(|(field, _)| field.as_str()),
        Some("if-match-metadata"),
        "{malformed}"
    );

    let stale = problem_body(patch_with(Some("999999")).await, 400).await;
    assert!(stale.to_string().contains("IF_MATCH"), "{stale}");
    assert_ne!(stale, malformed, "a stale header is a different violation");

    let applied = patch_with(None).await;
    assert_eq!(
        applied.status().as_u16(),
        200,
        "an absent header applies unconditionally"
    );
}

/// Guards `expected_status` and `all_variant_instances` against a new variant being forgotten.
const EXPECTED_VARIANT_COUNT: usize = 26;

/// Expected HTTP status per variant, per the canonical-error taxonomy (`status_code()`).
/// No wildcard arm on purpose.
fn expected_status(err: &DomainError) -> u16 {
    match err {
        DomainError::Validation { .. }
        | DomainError::PreconditionFailed { .. }
        | DomainError::MimeMismatch { .. }
        | DomainError::HashMismatch { .. }
        | DomainError::InvalidGtsType { .. }
        | DomainError::UnknownBackend { .. }
        | DomainError::PolicyMimeNotAllowed { .. }
        | DomainError::PolicySizeExceeded { .. }
        | DomainError::PolicyMetadataExceeded { .. }
        | DomainError::MultipartNotSupported { .. }
        // Delegated to `toolkit_odata`: every cursor failure is `InvalidArgument` -> 400.
        | DomainError::Pagination(_) => 400,
        DomainError::TokenInvalid { .. } | DomainError::Forbidden => 403,
        DomainError::FileNotFound { .. }
        | DomainError::VersionNotFound { .. }
        | DomainError::RetentionRuleNotFound { .. }
        | DomainError::MultipartUploadNotFound { .. } => 404,
        DomainError::Conflict { .. }
        | DomainError::MultipartUploadNotInProgress { .. }
        | DomainError::MultipartPartsMissing { .. }
        | DomainError::VersionedFileMigrationNotSupported { .. } => 409,
        DomainError::QuotaExceeded { .. } => 429,
        DomainError::BackendUnavailable { .. } => 503,
        DomainError::Database { .. } | DomainError::Backend { .. } | DomainError::InternalError => {
            500
        }
    }
}

fn all_variant_instances() -> Vec<DomainError> {
    vec![
        DomainError::FileNotFound { id: Uuid::nil() },
        DomainError::VersionNotFound {
            file_id: Uuid::nil(),
            version_id: Uuid::nil(),
        },
        DomainError::RetentionRuleNotFound {
            rule_id: Uuid::nil(),
        },
        DomainError::Database {
            message: "db down".into(),
            source: None,
        },
        DomainError::Validation {
            field: "name".into(),
            message: "required".into(),
        },
        DomainError::Conflict {
            message: "already exists".into(),
        },
        DomainError::PreconditionFailed {
            message: "If-Match mismatch".into(),
        },
        DomainError::MimeMismatch {
            declared: "image/png".into(),
            detected: "image/jpeg".into(),
        },
        DomainError::HashMismatch {
            expected: "aaaa".into(),
            got: "bbbb".into(),
        },
        DomainError::InvalidGtsType {
            value: "not-a-gts-type".into(),
        },
        DomainError::Backend {
            backend_id: "s3".into(),
            message: "put failed".into(),
        },
        DomainError::BackendUnavailable {
            backend_id: "s3".into(),
            message: "timeout".into(),
        },
        DomainError::UnknownBackend {
            backend_id: "nope".into(),
        },
        DomainError::TokenInvalid {
            reason: "bad signature".into(),
        },
        DomainError::Forbidden,
        DomainError::InternalError,
        DomainError::PolicyMimeNotAllowed {
            mime_type: "application/x-evil".into(),
        },
        DomainError::PolicySizeExceeded {
            limit_bytes: 1024,
            limit_source: "tenant-policy".into(),
        },
        DomainError::PolicyMetadataExceeded {
            reason: "too many keys".into(),
        },
        DomainError::QuotaExceeded {
            reason: "storage_bytes".into(),
        },
        DomainError::MultipartNotSupported {
            backend_id: "local".into(),
        },
        DomainError::MultipartUploadNotFound {
            upload_id: Uuid::nil(),
        },
        DomainError::MultipartUploadNotInProgress {
            upload_id: Uuid::nil(),
            state: "aborted".into(),
        },
        DomainError::MultipartPartsMissing {
            upload_id: Uuid::nil(),
            missing: vec![2, 5],
        },
        DomainError::VersionedFileMigrationNotSupported {
            file_id: Uuid::nil(),
        },
        DomainError::Pagination(toolkit_odata::Error::InvalidCursor),
    ]
}

#[test]
fn error_domain_error_maps_to_expected_http_status() {
    let cases = all_variant_instances();
    // Best-effort backstop: a variant added to `expected_status` but not to
    // `all_variant_instances` trips this.
    assert_eq!(
        cases.len(),
        EXPECTED_VARIANT_COUNT,
        "all_variant_instances must enumerate every DomainError variant \
         exactly once; update EXPECTED_VARIANT_COUNT and add/remove a case \
         when DomainError gains or loses a variant"
    );

    for err in cases {
        let expected = expected_status(&err);
        let debug = format!("{err:?}");
        let canonical: CanonicalError = err.into();
        let actual = canonical.status_code();
        assert_eq!(
            actual, expected,
            "DomainError variant {debug} mapped to {actual} but expected {expected}"
        );
    }
}

/// A transient backend failure is a `503` carrying `Retry-After: 5` on the wire.
#[test]
fn backend_unavailable_maps_to_503_with_retry_after() {
    use axum::response::IntoResponse;

    let canonical: CanonicalError = DomainError::BackendUnavailable {
        backend_id: "s3".into(),
        message: "timeout".into(),
    }
    .into();
    let response = toolkit::api::canonical_prelude::Problem::from(canonical).into_response();
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .expect("Retry-After header"),
        "5"
    );
}
