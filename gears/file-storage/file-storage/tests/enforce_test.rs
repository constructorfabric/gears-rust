//! End-to-end write-path enforcement tests (P2-M2) against a real temp-file
//! `SQLite` DB, the in-memory backend, the tenant-only authorizer, and (where
//! relevant) a mock `QuotaClient`. These prove the effective policy + quota
//! gates actually bite on the control-plane write path:
//!
//! - disallowed declared mime → reject
//! - oversized finalize → reject
//! - metadata over-limit → reject (create + update)
//! - quota exceeded → reject (create + version creation) when a client is wired
//! - permissive when no policy configured (P1 behaviour preserved)

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::policy::{MetadataLimits, PolicyBody, PolicyScope, SizeLimits};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{DataPlanePort, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::external_clients::{QuotaClient, QuotaDecision};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{CustomMetadataEntry, CustomMetadataPatch, NewFile, OwnerKind};

const GTS: &str = "gts.cf.fstorage.file.type.v1~x.test.v1~";

/// A mock quota client that denies once the cumulative requested bytes exceed a
/// cap. Each `check_storage_quota` call counts as a request of `additional_bytes`.
struct CappedQuota {
    cap: u64,
    seen: AtomicU64,
}

impl CappedQuota {
    fn new(cap: u64) -> Self {
        Self {
            cap,
            seen: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl QuotaClient for CappedQuota {
    async fn check_storage_quota(
        &self,
        _tenant_id: Uuid,
        _owner_id: Uuid,
        additional_bytes: u64,
        _metric_name: &str,
    ) -> Result<QuotaDecision, DomainError> {
        let total = self.seen.fetch_add(additional_bytes, Ordering::SeqCst) + additional_bytes;
        if total > self.cap {
            Ok(QuotaDecision::Denied {
                reason: format!("would use {total} > cap {}", self.cap),
            })
        } else {
            Ok(QuotaDecision::Allowed)
        }
    }
}

/// A quota client that always fails (to verify fail-closed behaviour).
struct ErroringQuota;

#[async_trait]
impl QuotaClient for ErroringQuota {
    async fn check_storage_quota(
        &self,
        _tenant_id: Uuid,
        _owner_id: Uuid,
        _additional_bytes: u64,
        _metric_name: &str,
    ) -> Result<QuotaDecision, DomainError> {
        Err(DomainError::InternalError)
    }
}

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-enforce-{}.db", Uuid::now_v7().simple()));
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let dsn = format!("sqlite://{file}?mode=rwc");
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    Arc::new(DBProvider::new(db))
}

async fn build_service(
    quota: Option<Arc<dyn QuotaClient>>,
) -> (Arc<FileService>, Arc<PolicyService>, DataPlaneService) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store,
        backends,
        issuer,
        Arc::clone(&authorizer),
        cfg,
        quota,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let psvc = Arc::new(PolicyService::new(policy_store, authorizer));
    (svc, psvc, dp)
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

fn new_file(owner: Uuid, mime: &str) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: owner,
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: mime.to_owned(),
        custom_metadata: vec![],
    }
}

// ── allowed-types-policy ────────────────────────────────────────────────────

#[tokio::test]
async fn create_file_with_disallowed_mime_is_rejected() {
    let (svc, psvc, _dp) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());

    // Tenant policy allows only image/*.
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            allowed_mime_types: vec!["image/*".to_owned()],
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    // text/plain is not allowed → reject.
    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMimeNotAllowed { .. }),
        "got {err:?}"
    );

    // image/png matches image/* → allowed.
    svc.create_file(&ctx, new_file(Uuid::now_v7(), "image/png"), None)
        .await
        .expect("image/png should be allowed");
}

// ── size-limits-policy ──────────────────────────────────────────────────────

#[tokio::test]
async fn finalize_oversized_upload_is_rejected() {
    let (svc, psvc, _dp) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();

    // Tenant policy: global 10-byte cap.
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(10),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let t = svc
        .create_file(&ctx, new_file(owner, "text/plain"), None)
        .await
        .unwrap();

    // Finalize a 100-byte upload → exceeds the 10-byte policy ceiling.
    let err = svc
        .finalize_upload(&ctx, t.file_id, t.version_id, 100, vec![0u8; 32])
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::PolicySizeExceeded {
                limit_bytes: 10,
                ..
            }
        ),
        "got {err:?}"
    );

    // A 5-byte finalize is within the ceiling.
    svc.finalize_upload(&ctx, t.file_id, t.version_id, 5, vec![0u8; 32])
        .await
        .expect("5 bytes within 10-byte cap");
}

#[tokio::test]
async fn create_file_bakes_max_size_into_upload_url() {
    // When a policy caps size, the signed URL carries the constraint so the
    // sidecar enforces mid-stream. We can't decode the opaque token here, but
    // the URL must still be issued (the gate did not reject create).
    let (svc, psvc, _dp) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(1_000_000),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();
    let t = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None)
        .await
        .unwrap();
    assert!(t.upload_url.contains("fs-token="));
}

// ── metadata-limits ─────────────────────────────────────────────────────────

#[tokio::test]
async fn create_file_with_too_many_metadata_pairs_is_rejected() {
    let (svc, psvc, _dp) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            metadata_limits: MetadataLimits {
                max_pairs: Some(1),
                ..MetadataLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let mut nf = new_file(Uuid::now_v7(), "text/plain");
    nf.custom_metadata = vec![
        CustomMetadataEntry {
            key: "a".to_owned(),
            value: "1".to_owned(),
        },
        CustomMetadataEntry {
            key: "b".to_owned(),
            value: "2".to_owned(),
        },
    ];
    let err = svc.create_file(&ctx, nf, None).await.unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMetadataExceeded { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn update_metadata_over_limit_is_rejected_on_resulting_total() {
    let (svc, psvc, _dp) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            metadata_limits: MetadataLimits {
                max_pairs: Some(2),
                ..MetadataLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    // Create with one entry (within the limit).
    let mut nf = new_file(Uuid::now_v7(), "text/plain");
    nf.custom_metadata = vec![CustomMetadataEntry {
        key: "a".to_owned(),
        value: "1".to_owned(),
    }];
    let t = svc.create_file(&ctx, nf, None).await.unwrap();

    // Patch adds two more keys → resulting total of 3 pairs > 2 → reject.
    let patch = CustomMetadataPatch {
        entries: vec![
            ("b".to_owned(), Some("2".to_owned())),
            ("c".to_owned(), Some("3".to_owned())),
        ],
    };
    let err = svc
        .update_metadata(&ctx, t.file_id, patch, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMetadataExceeded { .. }),
        "got {err:?}"
    );

    // Patch that replaces the existing key keeps the total at 1 → allowed.
    let patch_ok = CustomMetadataPatch {
        entries: vec![("a".to_owned(), Some("9".to_owned()))],
    };
    svc.update_metadata(&ctx, t.file_id, patch_ok, None)
        .await
        .expect("replacing an existing key stays within the limit");
}

// ── storage-quota ───────────────────────────────────────────────────────────

#[tokio::test]
async fn quota_exceeded_rejects_create_when_client_present() {
    // Cap of 10 bytes; the policy caps size at 100, so each create preflights
    // 100 bytes → the first create busts the 10-byte quota.
    let quota: Arc<dyn QuotaClient> = Arc::new(CappedQuota::new(10));
    let (svc, psvc, _dp) = build_service(Some(quota)).await;
    let ctx = ctx(Uuid::now_v7());
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(100),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::QuotaExceeded { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn quota_gates_version_creation_not_just_first_upload() {
    // Cap of 100 bytes; policy caps size at 60. First create preflights 60
    // (allowed, total 60). presign_version preflights another 60 (total 120 >
    // 100) → version creation is denied, proving quota covers overwrites too.
    let quota: Arc<dyn QuotaClient> = Arc::new(CappedQuota::new(100));
    let (svc, psvc, _dp) = build_service(Some(quota)).await;
    let ctx = ctx(Uuid::now_v7());
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(60),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let t = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None)
        .await
        .expect("first create within quota");

    let err = svc.presign_version(&ctx, t.file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::QuotaExceeded { .. }),
        "version creation must also be quota-gated, got {err:?}"
    );
}

#[tokio::test]
async fn quota_client_error_fails_closed() {
    let quota: Arc<dyn QuotaClient> = Arc::new(ErroringQuota);
    let (svc, _psvc, _dp) = build_service(Some(quota)).await;
    let ctx = ctx(Uuid::now_v7());

    // No policy configured, but the quota client errors → fail closed (deny).
    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::InternalError),
        "quota client error must fail closed, got {err:?}"
    );
}

// ── permissive when no policy ───────────────────────────────────────────────

#[tokio::test]
async fn no_policy_and_no_quota_is_fully_permissive() {
    let (svc, _psvc, _dp) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());

    // Any mime, any size finalize, any metadata — all accepted.
    let mut nf = new_file(Uuid::now_v7(), "application/x-anything");
    nf.custom_metadata = (0..50)
        .map(|i| CustomMetadataEntry {
            key: format!("k{i}"),
            value: "v".repeat(1000),
        })
        .collect();
    let t = svc
        .create_file(&ctx, nf, None)
        .await
        .expect("permissive create");

    svc.finalize_upload(&ctx, t.file_id, t.version_id, 10_000_000, vec![0u8; 32])
        .await
        .expect("no size limit without policy");
}
