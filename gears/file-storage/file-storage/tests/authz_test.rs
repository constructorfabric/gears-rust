//! Authorization of policy, retention-rule, listing and idempotency-replay operations across
//! users. `ScopedTestAuthorizer` denies `WRITE` per file or subject and `ADMIN_POLICY` unless
//! `set_admin(true)`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::domain::policy::{
    AgeRetention, MimeSizeOverride, PolicyBody, PolicyScope, RetentionRuleBody, RetentionScope,
    SizeLimits,
};
use file_storage_sdk::{OwnerFilter, OwnerKind};

mod common;
use common::{authz_harness, ctx_for as ctx, owned_file, valid_rule_body};

/// Setting another user's policy needs the admin scope; one's own does not. A denied call writes
/// no row.
#[tokio::test]
async fn set_policy_for_another_owner_needs_admin_scope() {
    let h = authz_harness("authz", true).await;
    // (caller sets the policy of `target`, caller is admin, allowed)
    for (name, target_is_self, admin, allowed) in [
        ("foreign owner without admin", false, false, false),
        ("self", true, false, true),
        ("foreign owner as admin", false, true, true),
    ] {
        let tenant = Uuid::now_v7();
        let caller = Uuid::now_v7();
        let target = if target_is_self {
            caller
        } else {
            Uuid::now_v7()
        };
        h.authz.set_admin(admin);

        let result = h
            .policy_svc
            .set_policy(
                &ctx(tenant, caller),
                PolicyScope::User,
                Some(target),
                PolicyBody::default(),
            )
            .await;

        let row = h
            .policy_store
            .get_policy(
                &AccessScope::allow_all(),
                tenant,
                &PolicyScope::User,
                Some(target),
            )
            .await
            .expect("get_policy");
        if allowed {
            result.unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(row.is_some(), "{name}: the policy row must exist");
        } else {
            assert!(
                matches!(result, Err(DomainError::Forbidden)),
                "{name}: expected Forbidden, got {result:?}"
            );
            assert!(row.is_none(), "{name}: no policy row may be written");
        }
    }
}

/// A `scope=file` rule needs `WRITE` on its target: a denied file is `Forbidden`, a writable one
/// succeeds, a nonexistent one is `FileNotFound`; only the successful call writes a row.
#[tokio::test]
async fn create_retention_rule_for_a_file_needs_write_access_to_it() {
    let h = authz_harness("authz", true).await;
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let ctx_owner = ctx(tenant, owner);
    let create_rule = |target: Uuid, body: RetentionRuleBody| {
        h.policy_svc
            .create_retention_rule(&ctx_owner, RetentionScope::File, Some(target), body)
    };

    let denied = h
        .file_svc
        .create_file(&ctx_owner, owned_file(owner), None, false)
        .await
        .expect("create the file whose write is denied")
        .file_id;
    h.authz.deny_write_for_file(denied);
    let result = create_rule(denied, RetentionRuleBody::default()).await;
    assert!(
        matches!(result, Err(DomainError::Forbidden)),
        "expected Forbidden, got {result:?}"
    );

    let writable = h
        .file_svc
        .create_file(&ctx_owner, owned_file(owner), None, false)
        .await
        .expect("create a writable file")
        .file_id;
    let rule = create_rule(writable, valid_rule_body())
        .await
        .expect("a writable file accepts a rule");
    assert_eq!(rule.scope_target_id, Some(writable));

    let nonexistent = Uuid::now_v7();
    let result = create_rule(nonexistent, RetentionRuleBody::default()).await;
    assert!(
        matches!(result, Err(DomainError::FileNotFound { id }) if id == nonexistent),
        "expected FileNotFound, got {result:?}"
    );

    let rules = h
        .policy_store
        .list_retention_rules(&AccessScope::allow_all(), tenant)
        .await
        .expect("list_retention_rules");
    assert_eq!(rules.len(), 1, "only the writable-file rule may exist");
}

#[tokio::test]
async fn delete_retention_rule_foreign_owner_is_denied() {
    let h = authz_harness("authz", true).await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();

    let rule = h
        .policy_svc
        .create_retention_rule(
            &ctx(tenant, user_a),
            RetentionScope::User,
            Some(user_a),
            valid_rule_body(),
        )
        .await
        .expect("user A creates own rule");

    let result = h
        .policy_svc
        .delete_retention_rule(&ctx(tenant, Uuid::now_v7()), rule.rule_id)
        .await;
    assert!(
        matches!(result, Err(DomainError::Forbidden)),
        "expected Forbidden, got {result:?}"
    );
    assert!(
        h.policy_store
            .get_retention_rule(&AccessScope::allow_all(), rule.rule_id)
            .await
            .expect("get_retention_rule")
            .is_some(),
        "the rule must still exist"
    );
}

/// A missing rule id is `RetentionRuleNotFound`, not `FileNotFound`.
#[tokio::test]
async fn delete_missing_retention_rule_returns_retention_not_found() {
    let h = authz_harness("authz", true).await;
    let missing_rule_id = Uuid::now_v7();

    let result = h
        .policy_svc
        .delete_retention_rule(&ctx(Uuid::now_v7(), Uuid::now_v7()), missing_rule_id)
        .await;
    assert!(
        matches!(
            result,
            Err(DomainError::RetentionRuleNotFound { rule_id }) if rule_id == missing_rule_id
        ),
        "expected RetentionRuleNotFound({missing_rule_id}), got {result:?}"
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
    let h = authz_harness("authz", true).await;
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
        // A tenant per case keeps the row checks independent.
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

/// Listing another user's files needs the admin scope; without it the call is denied.
#[tokio::test]
async fn list_files_of_another_owner_needs_admin_scope() {
    let h = authz_harness("authz", true).await;
    let tenant = Uuid::now_v7();
    let user_a = Uuid::now_v7();
    let ticket = h
        .file_svc
        .create_file(&ctx(tenant, user_a), owned_file(user_a), None, false)
        .await
        .expect("user A creates own file");
    let ctx_b = ctx(tenant, Uuid::now_v7());
    let list_as_b = || {
        h.file_svc.list_files(
            &ctx_b,
            OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id: user_a,
            },
            Some(10),
            None,
        )
    };

    let denied = list_as_b().await;
    assert!(
        matches!(denied, Err(DomainError::Forbidden)),
        "expected Forbidden, got {denied:?}"
    );

    h.authz.set_admin(true);
    let found = list_as_b()
        .await
        .expect("an admin may list another owner's files");
    assert!(found.items.iter().any(|f| f.file_id == ticket.file_id));
}

/// A revoked caller must not replay a stored ticket (authorize runs before the lookup).
#[tokio::test]
async fn idempotency_replay_requires_authorization() {
    let h = authz_harness("authz", false).await;
    let subject = Uuid::now_v7();
    let ctx_caller = ctx(Uuid::now_v7(), subject);
    let key = "idem-authz-1".to_owned();

    h.file_svc
        .create_file(&ctx_caller, owned_file(subject), Some(key.clone()), false)
        .await
        .expect("initial create should succeed while authorized");

    h.authz.deny_write_for_subject(subject);
    let replay = h
        .file_svc
        .create_file(&ctx_caller, owned_file(subject), Some(key), false)
        .await;

    assert!(
        matches!(replay, Err(DomainError::Forbidden)),
        "expected Forbidden on replay after WRITE was revoked, got {replay:?}"
    );
}

/// The key is scoped to the creating subject: another subject reusing the same
/// `(owner_kind, owner_id, key)` is `Forbidden`, not handed the stored ticket, while the creator's
/// own replay still returns it.
#[tokio::test]
async fn idempotency_key_scoped_to_subject() {
    let h = authz_harness("authz", false).await;
    let tenant = Uuid::now_v7();
    let subject_a = Uuid::now_v7();
    let key = "shared-key".to_owned();
    let create = async |subject: Uuid| {
        h.file_svc
            .create_file(
                &ctx(tenant, subject),
                owned_file(subject_a),
                Some(key.clone()),
                false,
            )
            .await
    };

    let ticket_a = create(subject_a)
        .await
        .expect("caller A creates and stores the idempotency ticket");

    let result_b = create(Uuid::now_v7()).await;
    assert!(
        matches!(result_b, Err(DomainError::Forbidden)),
        "caller B must be denied, got {result_b:?}"
    );

    let replay_a = create(subject_a)
        .await
        .expect("caller A's own replay must still succeed");
    assert_eq!(replay_a.file_id, ticket_a.file_id);
}
