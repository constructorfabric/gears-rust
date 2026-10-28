//! `Store::upsert_policy` upsert-race tests on a temp-file `SQLite` DB (`sqlite::memory:` would
//! give each pooled connection its own DB).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use sea_orm::{EntityTrait, PaginatorTrait};
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::domain::policy::{PolicyBody, PolicyScope, SizeLimits};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::entity::policy::Entity as PolicyEntity;

mod common;

const TENANT: &str = "00000000-0000-0000-0000-0000000000a1";
const OWNER: &str = "00000000-0000-0000-0000-0000000000b1";

/// `Store` over a fresh temp-file DB with migrations, plus the raw DSN for a second connection.
async fn build_store() -> (Store, String) {
    let (db, dsn) = common::build_db_with_dsn("policy").await;
    (Store::new(db), dsn)
}

fn body_with_max_bytes(max_bytes: u64) -> PolicyBody {
    PolicyBody {
        size_limits: SizeLimits {
            max_bytes: Some(max_bytes),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Two sequential upserts for one scope leave exactly one row carrying the second body, for a
/// tenant-scope row (no owner) and a user-scope row (`scope_owner_id = Some(..)`).
#[tokio::test]
async fn policy_upsert_on_conflict_updates_existing_row_not_duplicates() {
    let owner_id: Uuid = OWNER.parse().expect("valid uuid");
    for (policy_scope, scope_owner_id) in [
        (PolicyScope::Tenant, None),
        (PolicyScope::User, Some(owner_id)),
    ] {
        let (store, dsn) = build_store().await;
        let scope = AccessScope::allow_all();
        let tenant_id: Uuid = TENANT.parse().expect("valid uuid");
        let now = time::OffsetDateTime::now_utc();

        let first_body = body_with_max_bytes(100);
        let second_body = body_with_max_bytes(200);

        for body in [&first_body, &second_body] {
            store
                .upsert_policy(&scope, tenant_id, &policy_scope, scope_owner_id, body, now)
                .await
                .expect("upsert must succeed");
        }

        // Independent raw connection, purely for the row count.
        let raw = sea_orm::Database::connect(&dsn)
            .await
            .expect("second raw connection");
        let count = PolicyEntity::find()
            .count(&raw)
            .await
            .expect("count policies");
        assert_eq!(
            count, 1,
            "two upserts for the same {policy_scope:?} scope must leave exactly one row, not {count}"
        );

        let stored = store
            .get_policy(&scope, tenant_id, &policy_scope, scope_owner_id)
            .await
            .expect("get_policy must succeed")
            .expect("policy row must exist");
        assert_eq!(
            stored.body, second_body,
            "the surviving row must carry the second call's body, not the first's"
        );
    }
}
