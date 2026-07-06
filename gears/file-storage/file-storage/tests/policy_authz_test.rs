//! Cross-user policy/retention authorization tests (P2 remediation 0.7).
//!
//! `TenantOnlyAuthorizer` is not sufficient here — it ignores `action`/
//! `file_id` entirely, so it can't distinguish an `ADMIN_POLICY` grant from an
//! ordinary `WRITE`/`READ`, nor deny `WRITE` for a specific file. This file
//! defines a local `ScopedTestAuthorizer` test double that:
//!   - grants `READ`/`WRITE`/`DELETE` always, *unless* a specific `file_id` has
//!     been marked as write-denied (`deny_write_for_file`);
//!   - denies `ADMIN_POLICY` unless `set_admin(true)` has been called.
//!
//! `ScopedTestAuthorizer` is intentionally self-contained (no dependency on
//! other test modules) so later steps (0.9/0.10/0.11) can reuse it verbatim.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use sea_orm_migration::MigratorTrait;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::authz::{Authorizer, actions};
use file_storage::domain::error::DomainError;
use file_storage::domain::policy::{
    AgeRetention, MimeSizeOverride, PolicyBody, PolicyScope, RetentionRuleBody, RetentionScope,
    SizeLimits,
};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::PolicyStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

const GTS: &str = "gts.cf.fstorage.file.type.v1~x.test.v1~";

// ── ScopedTestAuthorizer ─────────────────────────────────────────────────────

/// Grants `READ`/`WRITE`/`DELETE` unconditionally (subject to `deny_write_for`),
/// but only grants `ADMIN_POLICY` while `is_admin` is set. Reused by later P2
/// remediation steps (0.9/0.10/0.11) that also need to exercise both the admin
/// and non-admin authorization paths.
#[derive(Default)]
pub struct ScopedTestAuthorizer {
    is_admin: AtomicBool,
    deny_write_for: Mutex<Option<Uuid>>,
}

impl ScopedTestAuthorizer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggle whether `ADMIN_POLICY` is granted.
    pub fn set_admin(&self, admin: bool) {
        self.is_admin.store(admin, Ordering::SeqCst);
    }

    /// Mark a specific `file_id` as `WRITE`-denied (all other files/actions
    /// stay allowed). Used to simulate "caller cannot write the target file".
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned (a prior panic while held) —
    /// not expected in single-threaded test bodies.
    pub fn deny_write_for_file(&self, file_id: Uuid) {
        *self.deny_write_for.lock().expect("lock poisoned") = Some(file_id);
    }
}

#[async_trait]
impl Authorizer for ScopedTestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        if action == actions::ADMIN_POLICY {
            return if self.is_admin.load(Ordering::SeqCst) {
                Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
            } else {
                Err(DomainError::Forbidden)
            };
        }

        if action == actions::WRITE
            && let Some(denied) = *self.deny_write_for.lock().expect("lock poisoned")
            && Some(denied) == file_id
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
        "cf-fs-policy-authz-test-{}.db",
        Uuid::now_v7().simple()
    ));
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

struct Harness {
    file_svc: Arc<FileService>,
    policy_svc: Arc<PolicyService>,
    policy_store: Arc<dyn PolicyStore>,
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
    let policy_svc = Arc::new(PolicyService::new(
        Arc::clone(&policy_store),
        Arc::clone(&authorizer),
    ));
    Harness {
        file_svc,
        policy_svc,
        policy_store,
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

/// A semantically valid retention-rule body (P2 remediation 0.11 rejects
/// `RetentionRuleBody::default()` — all-criteria-`None` — at create time), for
/// tests whose focus is authorization rather than validation.
fn valid_rule_body() -> RetentionRuleBody {
    RetentionRuleBody {
        age: Some(AgeRetention { max_age_days: 30 }),
        inactivity: None,
        metadata: None,
    }
}

fn new_file(owner_id: Uuid) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id,
        name: "victim.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

// ── set_policy ───────────────────────────────────────────────────────────────

/// `PUT /policy?scope=user&scope_owner_id=<victim>` from a non-owner,
/// non-admin caller must be denied and must not write a row.
#[tokio::test]
async fn set_policy_foreign_owner_without_admin_scope_is_denied() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let user_b = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);

    let result = h
        .policy_svc
        .set_policy(
            &ctx_a,
            PolicyScope::User,
            Some(user_b),
            PolicyBody::default(),
        )
        .await;

    assert!(
        matches!(result, Err(DomainError::Forbidden)),
        "expected Forbidden, got {result:?}"
    );

    let row = h
        .policy_store
        .get_policy(
            &AccessScope::allow_all(),
            tenant,
            &PolicyScope::User,
            Some(user_b),
        )
        .await
        .expect("get_policy");
    assert!(row.is_none(), "no policy row should exist for user_b");
}

/// Positive control: setting one's own user-scope policy is always allowed.
#[tokio::test]
async fn set_policy_self_owner_is_allowed() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);

    let stored = h
        .policy_svc
        .set_policy(
            &ctx_a,
            PolicyScope::User,
            Some(user_a),
            PolicyBody::default(),
        )
        .await
        .expect("set_policy should succeed for self");
    assert_eq!(stored.scope_owner_id, Some(user_a));

    let row = h
        .policy_store
        .get_policy(
            &AccessScope::allow_all(),
            tenant,
            &PolicyScope::User,
            Some(user_a),
        )
        .await
        .expect("get_policy")
        .expect("row must exist");
    assert_eq!(row.scope_owner_id, Some(user_a));
}

/// An `ADMIN_POLICY`-authorized caller may set another user's policy.
#[tokio::test]
async fn set_policy_tenant_admin_scope_allows_foreign_owner() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let admin = Uuid::now_v7();
    let user_b = Uuid::now_v7();
    let ctx_admin = ctx(tenant, admin);
    h.authz.set_admin(true);

    let stored = h
        .policy_svc
        .set_policy(
            &ctx_admin,
            PolicyScope::User,
            Some(user_b),
            PolicyBody::default(),
        )
        .await
        .expect("admin should be able to set foreign owner's policy");
    assert_eq!(stored.scope_owner_id, Some(user_b));

    let row = h
        .policy_store
        .get_policy(
            &AccessScope::allow_all(),
            tenant,
            &PolicyScope::User,
            Some(user_b),
        )
        .await
        .expect("get_policy")
        .expect("row must exist for user_b");
    assert_eq!(row.scope_owner_id, Some(user_b));
}

// ── create_retention_rule (file scope) ──────────────────────────────────────

/// A `scope=file` retention rule staged against a file the caller cannot
/// `WRITE` must be denied, and no row may be written.
#[tokio::test]
async fn create_retention_rule_file_scope_target_not_writable_is_denied() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let ticket = h
        .file_svc
        .create_file(&ctx_a, new_file(owner), None)
        .await
        .expect("create victim file");
    h.authz.deny_write_for_file(ticket.file_id);

    let result = h
        .policy_svc
        .create_retention_rule(
            &ctx_a,
            RetentionScope::File,
            Some(ticket.file_id),
            RetentionRuleBody::default(),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::Forbidden)),
        "expected Forbidden, got {result:?}"
    );

    let rules = h
        .policy_store
        .list_retention_rules(&AccessScope::allow_all(), tenant)
        .await
        .expect("list_retention_rules");
    assert_eq!(rules.len(), 0, "no retention rule row should be written");
}

/// Positive control: a `scope=file` rule against a real, writable file
/// succeeds. Also covers verifier finding B4: a rule staged against a
/// nonexistent `scope_target_id` surfaces `DomainError::FileNotFound` and
/// writes zero rows.
#[tokio::test]
async fn create_retention_rule_file_scope_target_writable_is_allowed() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let ticket = h
        .file_svc
        .create_file(&ctx_a, new_file(owner), None)
        .await
        .expect("create file");

    let rule = h
        .policy_svc
        .create_retention_rule(
            &ctx_a,
            RetentionScope::File,
            Some(ticket.file_id),
            valid_rule_body(),
        )
        .await
        .expect("create_retention_rule should succeed for a writable file");
    assert_eq!(rule.scope_target_id, Some(ticket.file_id));

    // B4: a nonexistent scope_target_id must 404, not silently pre-stage.
    let nonexistent = Uuid::now_v7();
    let result = h
        .policy_svc
        .create_retention_rule(
            &ctx_a,
            RetentionScope::File,
            Some(nonexistent),
            RetentionRuleBody::default(),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::FileNotFound { id }) if id == nonexistent),
        "expected FileNotFound, got {result:?}"
    );

    let rules = h
        .policy_store
        .list_retention_rules(&AccessScope::allow_all(), tenant)
        .await
        .expect("list_retention_rules");
    assert_eq!(rules.len(), 1, "only the writable-file rule should exist");
}

// ── delete_retention_rule ────────────────────────────────────────────────────

/// A `User`-scope retention rule created by user A must not be deletable by
/// user B (same tenant, no `ADMIN_POLICY`).
#[tokio::test]
async fn delete_retention_rule_foreign_owner_is_denied() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let user_b = Uuid::now_v7();
    let ctx_a = ctx(tenant, user_a);
    let ctx_b = ctx(tenant, user_b);

    let rule = h
        .policy_svc
        .create_retention_rule(
            &ctx_a,
            RetentionScope::User,
            Some(user_a),
            valid_rule_body(),
        )
        .await
        .expect("user A creates own rule");

    let result = h
        .policy_svc
        .delete_retention_rule(&ctx_b, rule.rule_id)
        .await;
    assert!(
        matches!(result, Err(DomainError::Forbidden)),
        "expected Forbidden, got {result:?}"
    );

    let still_there = h
        .policy_store
        .get_retention_rule(&AccessScope::allow_all(), rule.rule_id)
        .await
        .expect("get_retention_rule")
        .expect("rule must still exist");
    assert_eq!(still_there.rule_id, rule.rule_id);
}

/// `DELETE /retention-rules/{id}` for a `rule_id` that does not exist must
/// surface `DomainError::RetentionRuleNotFound`, not the file-shaped
/// `FileNotFound` it used to return (P2 remediation 3.10) — the RFC-9457
/// payload's resource type/detail must name a retention rule, not a file.
#[tokio::test]
async fn delete_missing_retention_rule_returns_retention_not_found() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let user = Uuid::now_v7();
    let missing_rule_id = Uuid::now_v7();

    let result = h
        .policy_svc
        .delete_retention_rule(&ctx(tenant, user), missing_rule_id)
        .await;
    assert!(
        matches!(
            result,
            Err(DomainError::RetentionRuleNotFound { rule_id }) if rule_id == missing_rule_id
        ),
        "expected RetentionRuleNotFound({missing_rule_id}), got {result:?}"
    );

    let err = result.expect_err("must be an error");
    let canonical: CanonicalError = err.into();
    assert_eq!(canonical.status_code(), 404);
    assert!(
        canonical
            .resource_type()
            .is_some_and(|t| t.contains("retention_rule")),
        "resource type must name a retention rule, got {:?}",
        canonical.resource_type()
    );
    assert!(
        canonical.detail().contains("Retention rule"),
        "detail must name a retention rule, not a file, got {:?}",
        canonical.detail()
    );
    assert!(
        !canonical.detail().to_lowercase().starts_with("file "),
        "detail must not mislabel the resource as a file, got {:?}",
        canonical.detail()
    );
}

// ── semantic validation (P2 remediation 0.11) ───────────────────────────────

/// `max_age_days = 0` would match every file in the tenant on the very next
/// sweep tick (`now - created_at > Duration::days(0)` is always true) —
/// `create_retention_rule` must reject it and write zero rows.
#[tokio::test]
async fn create_retention_rule_zero_max_age_is_rejected() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let result = h
        .policy_svc
        .create_retention_rule(
            &ctx_a,
            RetentionScope::User,
            Some(owner),
            RetentionRuleBody {
                age: Some(AgeRetention { max_age_days: 0 }),
                inactivity: None,
                metadata: None,
            },
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::Validation { .. })),
        "expected Validation, got {result:?}"
    );

    let rules = h
        .policy_store
        .list_retention_rules(&AccessScope::allow_all(), tenant)
        .await
        .expect("list_retention_rules");
    assert_eq!(rules.len(), 0, "no retention rule row should be written");
}

/// A retention rule with all of `age`/`inactivity`/`metadata` set to `None`
/// can never match any file — almost certainly a mistake — and must be
/// rejected rather than silently stored as a dead rule.
#[tokio::test]
async fn create_retention_rule_all_criteria_none_is_rejected() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let result = h
        .policy_svc
        .create_retention_rule(
            &ctx_a,
            RetentionScope::User,
            Some(owner),
            RetentionRuleBody::default(),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::Validation { .. })),
        "expected Validation, got {result:?}"
    );

    let rules = h
        .policy_store
        .list_retention_rules(&AccessScope::allow_all(), tenant)
        .await
        .expect("list_retention_rules");
    assert_eq!(rules.len(), 0, "no retention rule row should be written");
}

/// A `User`-scope retention rule with `scope_target_id = None` is a dead rule
/// (it can never resolve to a target user). `authorize_retention_scope`
/// already rejects a missing target for a non-admin caller as a `Forbidden`
/// mismatch, so this test drives the gap through an `ADMIN_POLICY` caller —
/// for whom the authz check alone would let it through — to prove the
/// `validate_retention_rule` guard closes it independently of authorization.
#[tokio::test]
async fn create_retention_rule_user_scope_without_target_is_rejected() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let admin = Uuid::now_v7();
    let ctx_admin = ctx(tenant, admin);
    h.authz.set_admin(true);

    let result = h
        .policy_svc
        .create_retention_rule(&ctx_admin, RetentionScope::User, None, valid_rule_body())
        .await;
    assert!(
        matches!(result, Err(DomainError::Validation { .. })),
        "expected Validation, got {result:?}"
    );

    let rules = h
        .policy_store
        .list_retention_rules(&AccessScope::allow_all(), tenant)
        .await
        .expect("list_retention_rules");
    assert_eq!(rules.len(), 0, "no retention rule row should be written");
}

/// A `scope = User` policy with `scope_owner_id = None` is a dead row: the
/// effective-policy reader (`FileService::get_effective_policy_internal`)
/// always queries the user-scope row with `Some(owner_id)`, so a `None`-owner
/// row can never be read back. `set_policy` must reject it at write time.
#[tokio::test]
async fn set_policy_user_scope_without_owner_is_rejected() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let result = h
        .policy_svc
        .set_policy(&ctx_a, PolicyScope::User, None, PolicyBody::default())
        .await;
    assert!(
        matches!(result, Err(DomainError::Validation { .. })),
        "expected Validation, got {result:?}"
    );
}

/// A `*/*` mime pattern is not a usable "allow everything" wildcard: the
/// matcher (`PolicyResolver::mime_allowed`) only special-cases the *subtype*
/// half of a pattern, so `*/*` never matches any real mime type and silently
/// acts as deny-all. `set_policy` rejects it outright (both in
/// `allowed_mime_types` and in a per-mime size override) rather than letting
/// it masquerade as "no restriction".
#[tokio::test]
async fn set_policy_star_slash_star_mime_is_rejected_or_defined() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let allowed_result = h
        .policy_svc
        .set_policy(
            &ctx_a,
            PolicyScope::User,
            Some(owner),
            PolicyBody {
                allowed_mime_types: vec!["*/*".to_owned()],
                ..PolicyBody::default()
            },
        )
        .await;
    assert!(
        matches!(allowed_result, Err(DomainError::Validation { .. })),
        "expected '*/*' in allowed_mime_types to be rejected, got {allowed_result:?}"
    );

    let per_mime_result = h
        .policy_svc
        .set_policy(
            &ctx_a,
            PolicyScope::User,
            Some(owner),
            PolicyBody {
                size_limits: SizeLimits {
                    max_bytes: None,
                    per_mime: vec![MimeSizeOverride {
                        mime: "*/*".to_owned(),
                        max_bytes: 1024,
                    }],
                },
                ..PolicyBody::default()
            },
        )
        .await;
    assert!(
        matches!(per_mime_result, Err(DomainError::Validation { .. })),
        "expected '*/*' in size_limits.per_mime to be rejected, got {per_mime_result:?}"
    );

    let row = h
        .policy_store
        .get_policy(
            &AccessScope::allow_all(),
            tenant,
            &PolicyScope::User,
            Some(owner),
        )
        .await
        .expect("get_policy");
    assert!(row.is_none(), "no policy row should be written");
}
