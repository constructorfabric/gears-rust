//! Cross-user policy/retention authorization tests.
//! `ScopedTestAuthorizer` denies `WRITE` per file and `ADMIN_POLICY` unless `set_admin(true)`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::domain::authz::Authorizer;
use file_storage::domain::error::DomainError;
use file_storage::domain::policy::{
    AgeRetention, MimeSizeOverride, PolicyBody, PolicyScope, RetentionRuleBody, RetentionScope,
    SizeLimits,
};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::PolicyStore;
use file_storage::domain::service::FileService;
use file_storage_sdk::NewFile;

mod common;
use common::{ScopedTestAuthorizer, StackBuilder, ctx_for as ctx};

struct Harness {
    file_svc: Arc<FileService>,
    policy_svc: Arc<PolicyService>,
    policy_store: Arc<dyn PolicyStore>,
    authz: Arc<ScopedTestAuthorizer>,
}

async fn build_harness() -> Harness {
    let authz = Arc::new(ScopedTestAuthorizer::admin_gated());
    let s = StackBuilder::new("policy-authz-test")
        .authorizer(Arc::clone(&authz) as Arc<dyn Authorizer>)
        .build()
        .await;
    let policy_store: Arc<dyn PolicyStore> = Arc::new(s.store.clone());
    Harness {
        file_svc: s.svc,
        policy_svc: s.psvc,
        policy_store,
        authz,
    }
}

/// A semantically valid retention-rule body, for tests focused on authorization.
fn valid_rule_body() -> RetentionRuleBody {
    RetentionRuleBody {
        age: Some(AgeRetention { max_age_days: 30 }),
        inactivity: None,
        metadata: None,
    }
}

fn new_file(owner_id: Uuid) -> NewFile {
    common::new_file_for(owner_id, "victim.bin", "application/octet-stream")
}

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

/// A `scope=file` rule on a writable file succeeds; a nonexistent `scope_target_id` yields
/// `FileNotFound` and writes no row.
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

/// A missing rule id must surface `RetentionRuleNotFound`, not `FileNotFound`.
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
}

/// What a validation case asks the service to do.
enum Call {
    Retention(RetentionScope, Option<Uuid>, RetentionRuleBody),
    Policy(PolicyScope, Option<Uuid>, PolicyBody),
}

/// Semantically invalid input is rejected with `Validation` and no row is written.
#[tokio::test]
async fn invalid_policy_and_retention_inputs_are_rejected_without_writing() {
    let owner = Uuid::now_v7();
    let mime_policy = |body: PolicyBody| Call::Policy(PolicyScope::User, Some(owner), body);
    // (name, admin caller, call). `*/*` never matches a real mime type (acts as deny-all).
    let cases: Vec<(&str, bool, Call)> = vec![
        (
            "max_age_days = 0 would match every file",
            false,
            Call::Retention(
                RetentionScope::User,
                Some(owner),
                RetentionRuleBody {
                    age: Some(AgeRetention { max_age_days: 0 }),
                    inactivity: None,
                    metadata: None,
                },
            ),
        ),
        (
            "no criteria can never match",
            false,
            Call::Retention(
                RetentionScope::User,
                Some(owner),
                RetentionRuleBody::default(),
            ),
        ),
        (
            "user-scope rule without a target, even for an admin",
            true,
            Call::Retention(RetentionScope::User, None, valid_rule_body()),
        ),
        (
            "user-scope policy without an owner",
            false,
            Call::Policy(PolicyScope::User, None, PolicyBody::default()),
        ),
        (
            "`*/*` in allowed_mime_types",
            false,
            mime_policy(PolicyBody {
                allowed_mime_types: vec!["*/*".to_owned()],
                ..PolicyBody::default()
            }),
        ),
        (
            "`*/*` in size_limits.per_mime",
            false,
            mime_policy(PolicyBody {
                size_limits: SizeLimits {
                    max_bytes: None,
                    per_mime: vec![MimeSizeOverride {
                        mime: "*/*".to_owned(),
                        max_bytes: 1024,
                    }],
                },
                ..PolicyBody::default()
            }),
        ),
    ];

    for (name, admin, call) in cases {
        let h = build_harness().await;
        let tenant = Uuid::now_v7();
        let ctx_a = ctx(tenant, owner);
        h.authz.set_admin(admin);

        let result = match call {
            Call::Retention(scope, target, body) => h
                .policy_svc
                .create_retention_rule(&ctx_a, scope, target, body)
                .await
                .map(drop),
            Call::Policy(scope, scope_owner, body) => h
                .policy_svc
                .set_policy(&ctx_a, scope, scope_owner, body)
                .await
                .map(drop),
        };
        assert!(
            matches!(result, Err(DomainError::Validation { .. })),
            "{name}: expected Validation, got {result:?}"
        );

        let rules = h
            .policy_store
            .list_retention_rules(&AccessScope::allow_all(), tenant)
            .await
            .expect("list_retention_rules");
        assert!(
            rules.is_empty(),
            "{name}: no retention rule row may be written"
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
        assert!(row.is_none(), "{name}: no policy row may be written");
    }
}
