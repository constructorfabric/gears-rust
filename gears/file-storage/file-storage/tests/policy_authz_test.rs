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
use toolkit_gts::gts_id;
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

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

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

// ── get_own_policy ───────────────────────────────────────────────────────────

/// `GET /policy?scope=user` with no `scope_owner_id` must be a `400
/// Validation` error, not a silently-empty (`204 No Content`) read: the
/// `(policy_scope, scope_owner_id)` pair `get_own_policy` is asked to look up
/// can never resolve a row (a `None`-owner user-scope row can never be
/// written either, per `validate_policy_body`'s identical guard on the write
/// path), so treating it as "no policy configured" instead of "malformed
/// request" would hide the caller's mistake.
#[tokio::test]
async fn get_own_policy_user_scope_without_owner_is_rejected() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let result = h
        .policy_svc
        .get_own_policy(&ctx_a, PolicyScope::User, None)
        .await;
    assert!(
        matches!(result, Err(DomainError::Validation { .. })),
        "expected Validation, got {result:?}"
    );
}

/// Positive control: `scope=tenant` with no `scope_owner_id` is the normal
/// (and only valid) shape for a tenant-scope read, and must not be rejected.
#[tokio::test]
async fn get_own_policy_tenant_scope_without_owner_is_allowed() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let result = h
        .policy_svc
        .get_own_policy(&ctx_a, PolicyScope::Tenant, None)
        .await;
    assert!(
        result.is_ok(),
        "tenant-scope read with no owner must be allowed, got {result:?}"
    );
}

/// `scope=tenant` with a `scope_owner_id` is the mirror-image malformed
/// shape: tenant policy rows never have an owner (`get_policy`/`upsert_policy`
/// key tenant rows on `(tenant_id, scope)` alone), so this pair can never
/// resolve a real row either. Must be rejected with `Validation`, the same
/// class of error as the `User`-scope-without-owner case above, not silently
/// queried as if the owner were ignored.
#[tokio::test]
async fn get_own_policy_tenant_scope_with_owner_is_rejected() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let result = h
        .policy_svc
        .get_own_policy(&ctx_a, PolicyScope::Tenant, Some(owner))
        .await;
    assert!(
        matches!(result, Err(DomainError::Validation { .. })),
        "expected Validation, got {result:?}"
    );
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
        .create_file(&ctx_a, new_file(owner), None, false)
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
        .create_file(&ctx_a, new_file(owner), None, false)
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

/// Cross-tenant existence oracle (verifier finding, medium): a `scope=file`
/// retention rule targeting a file that genuinely exists, but in a
/// *different* tenant, must surface the exact same `FileNotFound` a truly
/// nonexistent `scope_target_id` would — not resolve the file first (leaking
/// "this UUID exists somewhere") and only then fail authorization. Before the
/// fix, `authorize_retention_scope`'s `File` arm resolved the target via
/// `require_file(&AccessScope::allow_all(), ...)`, which finds a foreign
/// tenant's file unconditionally; `ScopedTestAuthorizer` grants `WRITE`
/// unconditionally too (unless a specific `file_id` is explicitly denied), so
/// with the old code this call would have *succeeded* — silently creating a
/// tenant-B-owned rule pointing at a tenant-A file. The fix scopes the
/// prefetch to the caller's own tenant, so the foreign file is
/// indistinguishable from a missing one.
#[tokio::test]
async fn create_retention_rule_file_scope_target_foreign_tenant_is_not_found() {
    let h = build_harness().await;
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let owner_a = Uuid::now_v7();
    let owner_b = Uuid::now_v7();
    let ctx_a = ctx(tenant_a, owner_a);
    let ctx_b = ctx(tenant_b, owner_b);

    let ticket = h
        .file_svc
        .create_file(&ctx_a, new_file(owner_a), None, false)
        .await
        .expect("tenant A creates a file");

    let result = h
        .policy_svc
        .create_retention_rule(
            &ctx_b,
            RetentionScope::File,
            Some(ticket.file_id),
            valid_rule_body(),
        )
        .await;
    assert!(
        matches!(result, Err(DomainError::FileNotFound { id }) if id == ticket.file_id),
        "expected FileNotFound (foreign-tenant file must not resolve), got {result:?}"
    );

    let rules = h
        .policy_store
        .list_retention_rules(&AccessScope::allow_all(), tenant_b)
        .await
        .expect("list_retention_rules");
    assert_eq!(
        rules.len(),
        0,
        "no rule should be created under tenant B pointing at tenant A's file"
    );
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

/// A `scope=file` rule whose target file has since been deleted (no FK/
/// cascade ties `retention_rules.scope_target_id` to `files.file_id`) must
/// remain deletable by the rule owner. Before the fix, `authorize_retention_scope`'s
/// `File` arm always re-resolved the (now-gone) target via `require_file`,
/// which 404s — permanently stuck rule, re-scanned by every sweep.
#[tokio::test]
async fn delete_retention_rule_file_scope_target_deleted_still_deletable() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let ticket = h
        .file_svc
        .create_file(&ctx_a, new_file(owner), None, false)
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
        .expect("create file-scope rule");

    // Delete the target file out from under the rule (unconditional delete).
    h.file_svc
        .delete_file(&ctx_a, ticket.file_id, Some("*"))
        .await
        .expect("delete target file");

    // The rule must still be deletable: authorization falls back to a plain
    // tenant-wide WRITE gate instead of re-resolving the gone file.
    let removed = h
        .policy_svc
        .delete_retention_rule(&ctx_a, rule.rule_id)
        .await
        .expect("delete_retention_rule must not fail once the file is gone");
    assert!(removed, "the orphaned rule must actually be removed");

    let gone = h
        .policy_store
        .get_retention_rule(&AccessScope::allow_all(), rule.rule_id)
        .await
        .expect("get_retention_rule");
    assert!(gone.is_none(), "rule row must be gone after delete");
}

/// The tenant-wide fallback used for an orphaned `scope=file` rule must not
/// widen who can delete it: a caller in a *different* tenant than the rule
/// must still fail — and, per the cross-tenant rule-ID oracle fix (P2
/// remediation), as the exact same `RetentionRuleNotFound` a nonexistent
/// `rule_id` produces (`delete_missing_retention_rule_returns_retention_not_found`
/// below), never a `Forbidden` (which would leak "yes, a rule with this id
/// exists") and never reaching the tenant-wide `WRITE` fallback at all: the
/// rule is now fetched under the caller's *own* tenant scope, so a foreign
/// tenant's rule_id simply does not resolve.
#[tokio::test]
async fn delete_retention_rule_file_scope_deleted_target_foreign_tenant_cannot_delete() {
    let h = build_harness().await;
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let owner_a = Uuid::now_v7();
    let owner_b = Uuid::now_v7();
    let ctx_a = ctx(tenant_a, owner_a);
    let ctx_b = ctx(tenant_b, owner_b);

    let ticket = h
        .file_svc
        .create_file(&ctx_a, new_file(owner_a), None, false)
        .await
        .expect("tenant A creates a file");

    let rule = h
        .policy_svc
        .create_retention_rule(
            &ctx_a,
            RetentionScope::File,
            Some(ticket.file_id),
            valid_rule_body(),
        )
        .await
        .expect("tenant A creates a file-scope rule");

    h.file_svc
        .delete_file(&ctx_a, ticket.file_id, Some("*"))
        .await
        .expect("tenant A deletes the target file");

    let result = h
        .policy_svc
        .delete_retention_rule(&ctx_b, rule.rule_id)
        .await;
    assert!(
        matches!(
            result,
            Err(DomainError::RetentionRuleNotFound { rule_id }) if rule_id == rule.rule_id
        ),
        "tenant B must not be able to delete tenant A's orphaned rule, and must see \
         RetentionRuleNotFound rather than Forbidden or a silent no-op; got {result:?}"
    );

    let still_there = h
        .policy_store
        .get_retention_rule(&AccessScope::allow_all(), rule.rule_id)
        .await
        .expect("get_retention_rule")
        .expect("rule must still exist, owned by tenant A");
    assert_eq!(still_there.tenant_id, tenant_a);
}

/// The cross-tenant rule-ID oracle, closed directly: deleting an existing
/// rule that belongs to a *different* tenant (`Tenant`-scope this time, not
/// the orphaned-file-scope special case above) must produce the exact same
/// `RetentionRuleNotFound` error — same variant, same `rule_id` field — as
/// deleting a `rule_id` that was never created at all. Before the fix, the
/// rule row was prefetched with `AccessScope::allow_all()` (crossing tenant
/// boundaries), so a foreign tenant's real rule_id could reach the
/// authorization decision and be distinguished (via `Forbidden` vs `404`)
/// from a merely-nonexistent id -- a cross-tenant rule-ID existence oracle.
#[tokio::test]
async fn delete_retention_rule_foreign_tenant_gets_same_error_as_nonexistent_id() {
    let h = build_harness().await;
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let owner_a = Uuid::now_v7();
    let owner_b = Uuid::now_v7();
    let ctx_a = ctx(tenant_a, owner_a);
    let ctx_b = ctx(tenant_b, owner_b);

    let rule = h
        .policy_svc
        .create_retention_rule(&ctx_a, RetentionScope::Tenant, None, valid_rule_body())
        .await
        .expect("tenant A creates a tenant-scope rule");

    let foreign_result = h
        .policy_svc
        .delete_retention_rule(&ctx_b, rule.rule_id)
        .await;

    let missing_rule_id = Uuid::now_v7();
    let missing_result = h
        .policy_svc
        .delete_retention_rule(&ctx_b, missing_rule_id)
        .await;

    match (&foreign_result, &missing_result) {
        (
            Err(DomainError::RetentionRuleNotFound {
                rule_id: foreign_id,
            }),
            Err(DomainError::RetentionRuleNotFound {
                rule_id: missing_id,
            }),
        ) => {
            assert_eq!(*foreign_id, rule.rule_id);
            assert_eq!(*missing_id, missing_rule_id);
        }
        other => panic!(
            "both a foreign-tenant existing rule_id and a nonexistent rule_id must produce \
             RetentionRuleNotFound identically, got {other:?}"
        ),
    }

    // Sanity: the rule must genuinely still exist (owned by tenant A) — the
    // 404 tenant B sees is a scoping artifact, not an actual deletion.
    let still_there = h
        .policy_store
        .get_retention_rule(&AccessScope::allow_all(), rule.rule_id)
        .await
        .expect("get_retention_rule")
        .expect("rule must still exist, owned by tenant A");
    assert_eq!(still_there.tenant_id, tenant_a);
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

/// A `scope = Tenant` policy with `scope_owner_id = Some(_)` is the
/// mirror-image invalid shape: tenant policy rows never have an owner, so
/// this write would either be silently ignored or write/query an impossible
/// row. `set_policy` must reject it, same as the `User`-without-owner case.
/// Uses the caller's own id as the (invalid) owner so authorization itself
/// passes and the rejection is attributable to the scope/owner shape check.
#[tokio::test]
async fn set_policy_tenant_scope_with_owner_is_rejected() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_a = ctx(tenant, owner);

    let result = h
        .policy_svc
        .set_policy(
            &ctx_a,
            PolicyScope::Tenant,
            Some(owner),
            PolicyBody::default(),
        )
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
