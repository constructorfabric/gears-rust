//! Idempotency-replay authorization + identity-scoping tests (P2 remediation
//! 0.10).
//!
//! `TenantOnlyAuthorizer` (used by `tests/multipart_test.rs`'s idempotency
//! block) ignores `action` entirely, so it can never deny `WRITE` for a
//! specific caller — it can't exercise "a caller whose WRITE was revoked
//! mid-window must not be able to replay a stored ticket". This file
//! duplicates a minimal `ScopedTestAuthorizer` test double (the pattern
//! established in `tests/policy_authz_test.rs` / `tests/list_authz_test.rs`;
//! each `tests/*.rs` file compiles as its own integration-test crate, so
//! cross-file reuse would require a shared harness restructuring that's more
//! invasive than the few lines duplicated below) that can deny `WRITE` for a
//! specific *subject* — `create_file`'s authorize call always passes
//! `file_id: None`, so the existing `deny_write_for_file` variant (keyed on
//! `file_id`) can't be reused as-is.

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
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

// ── ScopedTestAuthorizer (minimal duplicate: WRITE deny keyed on subject,
//    not file_id — see module docs) ─────────────────────────────────────────

/// Grants `READ`/`WRITE`/`DELETE` unconditionally, *unless* a specific
/// `subject_id` has been marked write-denied via `deny_write_for_subject`.
#[derive(Default)]
struct ScopedTestAuthorizer {
    deny_write_for_subject: Mutex<Option<Uuid>>,
}

impl ScopedTestAuthorizer {
    fn new() -> Self {
        Self::default()
    }

    /// Mark a specific `subject_id` as `WRITE`-denied (all other subjects and
    /// actions stay allowed). Used to simulate "the caller's WRITE grant was
    /// revoked mid-window".
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned (a prior panic while held) —
    /// not expected in single-threaded test bodies.
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

// ── test harness ─────────────────────────────────────────────────────────────

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
    let policy_svc = Arc::new(PolicyService::new(policy_store, authorizer));
    Harness {
        file_svc,
        policy_svc,
        authz,
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

// ── idempotency_replay_requires_authorization ───────────────────────────────

/// A stored idempotency ticket must never be replayed to a caller whose
/// `WRITE` grant has since been revoked. Before the 0.10 fix, the idempotency
/// lookup + early return ran *before* `authorize(...)`, so a revoked caller
/// could still retrieve a live signed upload URL.
#[tokio::test]
async fn idempotency_replay_requires_authorization() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(tenant, subject);
    let key = "idem-authz-1".to_owned();

    // Seed a ticket while WRITE is still granted.
    let first = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key.clone()), false)
        .await
        .expect("initial create should succeed while authorized");

    // Revoke WRITE for this subject, then replay the same key.
    h.authz.deny_write_for_subject(subject);
    let replay = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key), false)
        .await;

    assert!(
        matches!(replay, Err(DomainError::Forbidden)),
        "expected Forbidden on replay after WRITE was revoked, got {replay:?}"
    );
    // Sanity: the seeded ticket is real and distinct from any leaked value.
    assert_ne!(first.file_id, Uuid::nil());
}

// ── idempotency_key_scoped_to_subject ───────────────────────────────────────

/// One caller's idempotency key must never surface another caller's ticket,
/// even when both request bodies share the same `(owner_kind, owner_id, key)`
/// tuple. The key is scoped by the request-body `owner_id`, not the caller,
/// so a caller who guesses/reuses the tuple must be denied — not handed the
/// original caller's stored ticket.
#[tokio::test]
async fn idempotency_key_scoped_to_subject() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject_a = Uuid::now_v7();
    let subject_b = Uuid::now_v7();
    let ctx_a = ctx(tenant, subject_a);
    let ctx_b = ctx(tenant, subject_b);

    // Both requests target the same owner_id (e.g. a shared resource owner)
    // and the same key.
    let owner_id = subject_a;
    let key = "shared-key".to_owned();

    let ticket_a = h
        .file_svc
        .create_file(&ctx_a, new_file(owner_id), Some(key.clone()), false)
        .await
        .expect("caller A creates and stores the idempotency ticket");

    // Caller B replays the same (owner_id, key) tuple. B must not receive A's
    // ticket.
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

    // Caller A's own replay must still work unchanged.
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

// ── idempotency_replay_rejected_after_policy_tightened ──────────────────────

/// A policy tightened *after* the original `create_file` must also reject a
/// replay of a stored idempotency ticket — not just a fresh request. Before
/// the fix, the replay path (`create.rs`, the `idempotency_key` branch)
/// returned the stored ticket unconditionally once the request-hash and
/// subject checks passed, entirely before the policy-resolution block that
/// runs for a fresh create — bypassing a tightened policy for the whole
/// idempotency TTL (24h default).
#[tokio::test]
async fn idempotency_replay_rejected_after_policy_tightened() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(tenant, subject);
    let key = "idem-policy-tighten-1".to_owned();

    // No policy configured yet: the original create succeeds (mime type is
    // "application/octet-stream", see `new_file`).
    let first = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key.clone()), false)
        .await
        .expect("initial create should succeed with no policy configured");

    // Tighten the tenant policy to allow only a different mime type.
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

    // Replaying the same key must now be rejected: the stored request's mime
    // type is no longer permitted by the CURRENT effective policy.
    let replay = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key), false)
        .await;
    assert!(
        matches!(
            replay,
            Err(DomainError::PolicyMimeNotAllowed { ref mime_type }) if mime_type == "application/octet-stream"
        ),
        "expected PolicyMimeNotAllowed on replay after policy tightened, got {replay:?}"
    );
    assert_ne!(first.file_id, Uuid::nil());
}

// ── helpers for the `auto_bind`/ownership replay tests below ───────────────

/// Pull the `fs-token` query value out of a signed sidecar URL, matching the
/// extraction pattern used in `api_handlers_test.rs`/`enforce_test.rs`.
fn token_from_url(url: &str) -> &str {
    let start = url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len();
    &url[start..]
}

// ── idempotency_replay_rejects_changed_bind_mode (t19) ──────────────────────

/// A replay that supplies a different `bind` than the original request must
/// be rejected with the *same* conflict as a `request_hash` mismatch — not
/// silently re-mint a token under the new mode. `bind` is deliberately
/// excluded from `request_hash` (so a stored ticket predating the flag still
/// hashes the same), which is exactly why this needs its own check.
#[tokio::test]
async fn idempotency_replay_rejects_changed_bind_mode() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(tenant, subject);

    // Capture the wording of an actual `request_hash` mismatch, so this test
    // doesn't hardcode a message string that could drift independently of
    // the production code it's supposed to mirror.
    let hash_mismatch_key = "idem-hash-mismatch-1".to_owned();
    h.file_svc
        .create_file(
            &ctx_caller,
            new_file(subject),
            Some(hash_mismatch_key.clone()),
            false,
        )
        .await
        .expect("initial create for hash-mismatch reference");
    let mut renamed = new_file(subject);
    renamed.name = "different-name.bin".to_owned();
    let hash_mismatch_err = h
        .file_svc
        .create_file(&ctx_caller, renamed, Some(hash_mismatch_key), false)
        .await
        .expect_err("a different request body must conflict");
    let DomainError::Conflict {
        message: hash_mismatch_message,
    } = hash_mismatch_err
    else {
        panic!("expected Conflict, got {hash_mismatch_err:?}");
    };

    // Now the actual case under test: same request body, only `bind` flips.
    let bind_key = "idem-bind-mismatch-1".to_owned();
    h.file_svc
        .create_file(
            &ctx_caller,
            new_file(subject),
            Some(bind_key.clone()),
            false,
        )
        .await
        .expect("initial create with bind=false");
    let bind_replay_err = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(bind_key), true)
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

// ── idempotency_replay_same_bind_reissues_original_mode (t19) ───────────────

/// A replay with the SAME `bind` as the original request must succeed, and
/// the re-minted token must carry the originally-recorded `bind_on_finalize`
/// — not whatever the (matching) retry happened to pass in.
#[tokio::test]
async fn idempotency_replay_same_bind_reissues_original_mode() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(tenant, subject);
    let key = "idem-bind-same-1".to_owned();

    let first = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key.clone()), true)
        .await
        .expect("initial create with bind=true");

    let replay = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key), true)
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

// ── ownership-transfer / deletion invalidate an in-flight replay (t20) ─────

/// After the file is transferred to a new owner, a replay of the ORIGINAL
/// owner's idempotency key must be rejected rather than mint a fresh upload
/// token — a former owner must not retain write capability past a transfer.
#[tokio::test]
async fn idempotency_replay_rejected_after_ownership_transfer() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let new_owner = Uuid::now_v7();
    let ctx_caller = ctx(tenant, subject);
    let key = "idem-transfer-1".to_owned();

    let first = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key.clone()), false)
        .await
        .expect("initial create should succeed");

    h.file_svc
        .transfer_ownership(&ctx_caller, first.file_id, OwnerKind::User, new_owner)
        .await
        .expect("transfer_ownership should succeed");

    let replay = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key), false)
        .await;
    assert!(
        matches!(replay, Err(DomainError::Conflict { .. })),
        "expected Conflict after the file changed owner, got {replay:?}"
    );
}

/// If the file was deleted before the replay, the replay must never mint a
/// token against the dead file.
///
/// `idempotency_keys.file_id REFERENCES files (file_id) ON DELETE CASCADE`
/// (`m20260701_000001_p2_initial`) means `delete_file` already removes the
/// stored ticket in the very same transaction as the file — so by the time a
/// replay runs, `get_idempotency_key` finds no record at all, and
/// `create_file` correctly falls through to its fresh-create path instead of
/// its replay branch. That is what's asserted here: the "replay" mints an
/// entirely independent file, never a token pointing at `first.file_id`.
/// `create_file`'s own live-file recheck in its replay branch (added
/// alongside the ownership-transfer guard) is a defense-in-depth backstop
/// for the narrower race where a record is read just before a concurrent
/// delete removes both rows — not exercisable through this service-level
/// API, since the cascade already closes the ordinary case.
#[tokio::test]
async fn idempotency_replay_after_delete_mints_an_independent_file() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(tenant, subject);
    let key = "idem-delete-1".to_owned();

    let first = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key.clone()), false)
        .await
        .expect("initial create should succeed");

    h.file_svc
        .delete_file(&ctx_caller, first.file_id, Some("*"))
        .await
        .expect("delete_file should succeed");

    let replay = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key), false)
        .await
        .expect("replay after delete must still succeed as a fresh create");
    assert_ne!(
        replay.file_id, first.file_id,
        "must never mint a token against the deleted file"
    );
}

/// Regression: an ordinary replay with nothing changed must still succeed
/// and return the original file/version — the ownership/version-status
/// re-checks above must not affect the unmodified happy path.
#[tokio::test]
async fn idempotency_replay_unchanged_still_succeeds() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(tenant, subject);
    let key = "idem-regress-1".to_owned();

    let first = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key.clone()), false)
        .await
        .expect("initial create should succeed");

    let replay = h
        .file_svc
        .create_file(&ctx_caller, new_file(subject), Some(key), false)
        .await
        .expect("unchanged replay must still succeed");

    assert_eq!(replay.file_id, first.file_id);
    assert_eq!(replay.version_id, first.version_id);
}
