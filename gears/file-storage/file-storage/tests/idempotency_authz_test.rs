//! Idempotency-replay authorization and identity scoping.
//! `ScopedTestAuthorizer` can deny `WRITE` per subject (`create_file` passes `file_id: None`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::authz::{Authorizer, actions};
use file_storage::domain::error::DomainError;
use file_storage::domain::policy::{PolicyBody, PolicyScope};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::PolicyStore;
use file_storage::domain::service::{FileService, ServiceConfig, UploadTicket};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// Grants everything unless a `subject_id` has been marked write-denied.
#[derive(Default)]
struct ScopedTestAuthorizer {
    deny_write_for_subject: Mutex<Option<Uuid>>,
}

impl ScopedTestAuthorizer {
    fn new() -> Self {
        Self::default()
    }

    /// Mark `subject_id` as `WRITE`-denied.
    ///
    /// # Panics
    /// Panics if the mutex is poisoned.
    fn deny_write_for_subject(&self, subject_id: Uuid) {
        *self.deny_write_for_subject.lock().expect("lock poisoned") = Some(subject_id);
    }
}

#[async_trait]
impl Authorizer for ScopedTestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        _file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        if action == actions::WRITE
            && let Some(denied) = *self.deny_write_for_subject.lock().expect("lock poisoned")
            && denied == ctx.subject_id()
        {
            return Err(DomainError::Forbidden);
        }

        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-idempotency-authz-test-{}.db",
        Uuid::now_v7().simple()
    ));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
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

struct Harness {
    file_svc: Arc<FileService>,
    policy_svc: Arc<PolicyService>,
    authz: Arc<ScopedTestAuthorizer>,
    /// For direct row edits that simulate stored state drifting under a recorded key.
    db: Arc<DBProvider<DbError>>,
}

async fn build_harness() -> Harness {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authz = Arc::new(ScopedTestAuthorizer::new());
    let authorizer: Arc<dyn Authorizer> = Arc::clone(&authz) as Arc<dyn Authorizer>;
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let file_svc = Arc::new(FileService::new(
        store,
        backends,
        issuer,
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let policy_svc = Arc::new(PolicyService::new(policy_store, authorizer, 50, 1000));
    Harness {
        file_svc,
        policy_svc,
        authz,
        db,
    }
}

fn ctx(tenant: Uuid, subject: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

fn new_file(owner_id: Uuid) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id,
        name: "upload.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

/// A revoked caller must not replay a stored ticket (authorize runs before the lookup).
#[tokio::test]
async fn idempotency_replay_requires_authorization() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(tenant, subject);
    let key = "idem-authz-1".to_owned();

    let first = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key.clone()), false)
        .await
        .expect("initial create should succeed while authorized");

    h.authz.deny_write_for_subject(subject);
    let replay = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key), false)
        .await;

    assert!(
        matches!(replay, Err(DomainError::Forbidden)),
        "expected Forbidden on replay after WRITE was revoked, got {replay:?}"
    );
    assert_ne!(first.file_id, Uuid::nil());
}

/// The key is scoped to the caller: another caller reusing the same `(owner_kind, owner_id, key)`
/// must be denied, not handed the stored ticket.
#[tokio::test]
async fn idempotency_key_scoped_to_subject() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject_a = Uuid::now_v7();
    let subject_b = Uuid::now_v7();
    let ctx_a = ctx(tenant, subject_a);
    let ctx_b = ctx(tenant, subject_b);

    // Both requests share owner_id and key.
    let owner_id = subject_a;
    let key = "shared-key".to_owned();

    let ticket_a = h
        .file_svc
        .create_file(&ctx_a, new_file(owner_id), Some(key.clone()), false)
        .await
        .expect("caller A creates and stores the idempotency ticket");

    let result_b = h
        .file_svc
        .create_file(&ctx_b, new_file(owner_id), Some(key.clone()), false)
        .await;
    match &result_b {
        Ok(ticket_b) => assert_ne!(
            ticket_b.file_id, ticket_a.file_id,
            "caller B must never receive caller A's ticket"
        ),
        Err(DomainError::Forbidden) => {} // also an acceptable denial outcome
        Err(other) => panic!("unexpected error for caller B's replay: {other:?}"),
    }

    let replay_a = h
        .file_svc
        .create_file(&ctx_a, new_file(owner_id), Some(key), false)
        .await
        .expect("caller A's own replay must still succeed");
    assert_eq!(
        replay_a.file_id, ticket_a.file_id,
        "caller A's replay must return A's original ticket"
    );
}

fn token_from_url(url: &str) -> &str {
    let start = url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len();
    &url[start..]
}

/// Creates (or replays) the keyed file for `owner` as `ctx`.
async fn create_keyed(
    h: &Harness,
    ctx: &SecurityContext,
    owner: Uuid,
    key: &str,
    auto_bind: bool,
) -> Result<UploadTicket, DomainError> {
    h.file_svc
        .create_file(ctx, new_file(owner), Some(key.to_owned()), auto_bind)
        .await
}

/// State that drifts after the key was recorded; the replay must then be refused (never mint a
/// fresh upload token) with the row's own error.
enum Drift {
    PolicyTightened,
    OwnershipTransferred,
    StoredBodyCorrupted,
    VersionRowRemoved,
    VersionNoLongerPending,
}

#[tokio::test]
async fn idempotency_replay_is_rejected_after_stored_state_drifts() {
    for (row, drift) in [
        // The stored request's mime type is no longer permitted by the CURRENT effective policy.
        ("policy tightened", Drift::PolicyTightened),
        // A former owner must not retain write capability past a transfer.
        ("ownership transferred", Drift::OwnershipTransferred),
        ("stored body corrupted", Drift::StoredBodyCorrupted),
        ("version row removed", Drift::VersionRowRemoved),
        // E.g. a finalize whose response was lost: no re-minting write capability.
        ("version no longer pending", Drift::VersionNoLongerPending),
    ] {
        let h = build_harness().await;
        let tenant = Uuid::now_v7();
        let subject = Uuid::now_v7();
        let ctx_caller = ctx(tenant, subject);
        let key = "idem-drift-1";
        let auto_bind = matches!(drift, Drift::VersionNoLongerPending);

        let ticket = create_keyed(&h, &ctx_caller, subject, key, auto_bind)
            .await
            .unwrap_or_else(|e| panic!("{row}: initial create must succeed: {e:?}"));
        let conn = h.db.conn().expect("conn");

        match drift {
            Drift::PolicyTightened => {
                h.policy_svc
                    .set_policy(
                        &ctx_caller,
                        PolicyScope::Tenant,
                        None,
                        PolicyBody {
                            allowed_mime_types: vec!["image/png".to_owned()],
                            ..PolicyBody::default()
                        },
                    )
                    .await
                    .expect("tighten tenant policy");
            }
            Drift::OwnershipTransferred => {
                h.file_svc
                    .transfer_ownership(
                        &ctx_caller,
                        ticket.file_id,
                        OwnerKind::User,
                        Uuid::now_v7(),
                    )
                    .await
                    .expect("transfer_ownership should succeed");
            }
            Drift::StoredBodyCorrupted => {
                use sea_orm::sea_query::Expr;
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
                use toolkit_db::secure::SecureUpdateExt;

                use file_storage::infra::storage::entity::idempotency_key::{
                    Column as IdemCol, Entity as IdemEntity,
                };

                IdemEntity::update_many()
                    .col_expr(IdemCol::ResponseBody, Expr::value("not-json"))
                    .filter(IdemCol::TenantId.eq(tenant))
                    .filter(IdemCol::OwnerId.eq(subject))
                    .filter(IdemCol::IdempotencyKey.eq(key))
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .exec(&conn)
                    .await
                    .expect("corrupt the stored idempotency response body directly");
            }
            Drift::VersionRowRemoved => {
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
                use toolkit_db::secure::SecureDeleteExt;

                use file_storage::infra::storage::entity::file_version::{
                    Column as VerCol, Entity as VerEntity,
                };

                VerEntity::delete_many()
                    .filter(VerCol::VersionId.eq(ticket.version_id))
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .exec(&conn)
                    .await
                    .expect("delete the pending version row directly");
            }
            Drift::VersionNoLongerPending => {
                let finalized = file_storage::infra::storage::repo::VersionRepo::new()
                    .finalize(
                        &conn,
                        &AccessScope::allow_all(),
                        ticket.file_id,
                        ticket.version_id,
                        1,
                        vec![0u8; 32],
                        "whole-sha256",
                        None,
                        None,
                    )
                    .await
                    .expect("finalize the pending version directly");
                assert!(finalized);
            }
        }

        let replay = create_keyed(&h, &ctx_caller, subject, key, auto_bind).await;
        match (drift, replay) {
            (Drift::PolicyTightened, Err(DomainError::PolicyMimeNotAllowed { mime_type })) => {
                assert_eq!(mime_type, "application/octet-stream", "{row}");
            }
            (
                Drift::OwnershipTransferred | Drift::VersionNoLongerPending,
                Err(DomainError::Conflict { .. }),
            )
            | (Drift::StoredBodyCorrupted, Err(DomainError::Database { .. })) => {}
            (
                Drift::VersionRowRemoved,
                Err(DomainError::VersionNotFound {
                    file_id,
                    version_id,
                }),
            ) => {
                assert_eq!(file_id, ticket.file_id, "{row}");
                assert_eq!(version_id, ticket.version_id, "{row}");
            }
            (_, other) => panic!("{row}: unexpected replay outcome {other:?}"),
        }
    }
}

#[tokio::test]
async fn idempotency_replay_rejects_changed_bind_mode() {
    let h = build_harness().await;
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(Uuid::now_v7(), subject);

    create_keyed(&h, &ctx_caller, subject, "idem-hash-mismatch-1", false)
        .await
        .expect("initial create for hash-mismatch reference");
    let mut renamed = new_file(subject);
    renamed.name = "different-name.bin".to_owned();
    let hash_mismatch_err = h
        .file_svc
        .create_file(
            &ctx_caller,
            renamed,
            Some("idem-hash-mismatch-1".to_owned()),
            false,
        )
        .await
        .expect_err("a different request body must conflict");
    let DomainError::Conflict {
        message: hash_mismatch_message,
    } = hash_mismatch_err
    else {
        panic!("expected Conflict, got {hash_mismatch_err:?}");
    };

    create_keyed(&h, &ctx_caller, subject, "idem-bind-mismatch-1", false)
        .await
        .expect("initial create with bind=false");
    let bind_replay_err = create_keyed(&h, &ctx_caller, subject, "idem-bind-mismatch-1", true)
        .await
        .expect_err("a replay with a different bind mode must conflict");
    let DomainError::Conflict {
        message: bind_mismatch_message,
    } = bind_replay_err
    else {
        panic!("expected Conflict, got {bind_replay_err:?}");
    };

    assert_eq!(
        bind_mismatch_message, hash_mismatch_message,
        "a bind-mode mismatch must produce the exact same conflict as a request_hash mismatch"
    );
}

#[tokio::test]
async fn idempotency_replay_same_bind_reissues_original_mode() {
    let h = build_harness().await;
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(Uuid::now_v7(), subject);

    let first = create_keyed(&h, &ctx_caller, subject, "idem-bind-same-1", true)
        .await
        .expect("initial create with bind=true");
    let replay = create_keyed(&h, &ctx_caller, subject, "idem-bind-same-1", true)
        .await
        .expect("replay with the same bind mode must succeed");

    assert_eq!(replay.file_id, first.file_id);
    assert_eq!(replay.version_id, first.version_id);

    let now = time::OffsetDateTime::now_utc();
    let claims = h
        .file_svc
        .verifier()
        .verify(token_from_url(&replay.upload_url), now)
        .expect("replayed token must verify");
    assert!(
        claims.bind_on_finalize,
        "replayed token must keep the originally-recorded auto_bind=true"
    );
}

#[tokio::test]
async fn idempotency_replay_after_delete_mints_an_independent_file() {
    let h = build_harness().await;
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(Uuid::now_v7(), subject);

    let first = create_keyed(&h, &ctx_caller, subject, "idem-delete-1", false)
        .await
        .expect("initial create should succeed");
    h.file_svc
        .delete_file(&ctx_caller, first.file_id, Some("*"))
        .await
        .expect("delete_file should succeed");

    let replay = create_keyed(&h, &ctx_caller, subject, "idem-delete-1", false)
        .await
        .expect("replay after delete must still succeed as a fresh create");
    assert_ne!(
        replay.file_id, first.file_id,
        "must never mint a token against the deleted file"
    );
}

#[tokio::test]
async fn idempotency_replay_unchanged_still_succeeds() {
    let h = build_harness().await;
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(Uuid::now_v7(), subject);

    let first = create_keyed(&h, &ctx_caller, subject, "idem-regress-1", false)
        .await
        .expect("initial create should succeed");
    let replay = create_keyed(&h, &ctx_caller, subject, "idem-regress-1", false)
        .await
        .expect("unchanged replay must still succeed");

    assert_eq!(replay.file_id, first.file_id);
    assert_eq!(replay.version_id, first.version_id);
}
