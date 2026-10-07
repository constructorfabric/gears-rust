// Created: 2026-09-07 by Virtuozzo International GmbH
//! The repository over an in-memory database, through the resolution harness.

use serde_json::json;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::AccessRepo;
use crate::domain::access::{AccessRepository, RestrictionDraft, TenantAccess, strictest};
use crate::domain::error::DomainError;
use crate::domain::resolution::scope_class;
use crate::test_support::ResolutionHarness;

fn draft(declaration: Uuid, tenant: Uuid, access: TenantAccess) -> RestrictionDraft {
    RestrictionDraft {
        declaration_id: declaration,
        tenant_id: tenant,
        access,
        set_by: "root-admin".to_owned(),
    }
}

#[tokio::test]
async fn rows_are_upserted_per_pair_and_the_chain_query_is_an_exact_set() {
    let h = ResolutionHarness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let conn = h.db.conn().expect("connection");
    let all = AccessScope::allow_all();
    let t = &h.tree;

    let first = AccessRepo
        .upsert(&conn, &all, draft(d, t.a, TenantAccess::ReadOnly), None)
        .await
        .expect("insert");
    let second = AccessRepo
        .upsert(
            &conn,
            &all,
            draft(d, t.a, TenantAccess::Hidden),
            Some(first.updated_at),
        )
        .await
        .expect("replace");
    assert_eq!(first.id, second.id, "one row per pair");
    assert_eq!(second.access, TenantAccess::Hidden);
    assert!(second.updated_at >= first.updated_at);

    AccessRepo
        .upsert(&conn, &all, draft(d, t.c, TenantAccess::ReadOnly), None)
        .await
        .expect("sibling row");

    // The chain of `b` is root → a → b: `c`'s row is outside it.
    let chain = vec![t.root, t.a, t.b];
    let rows = AccessRepo
        .find_in_tenants(&conn, &all, d, &chain)
        .await
        .expect("chain query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tenant_id, t.a);
    assert_eq!(strictest(&rows).access, TenantAccess::Hidden);

    AccessRepo
        .delete(&conn, &all, d, t.a, second.updated_at)
        .await
        .expect("delete");
    // Gone: the version the caller holds matches no row any more.
    assert!(matches!(
        AccessRepo
            .delete(&conn, &all, d, t.a, second.updated_at)
            .await,
        Err(DomainError::PreconditionFailed { .. })
    ));
    assert!(
        AccessRepo
            .find_one(&conn, &all, d, t.a)
            .await
            .expect("lookup")
            .is_none()
    );
}

#[tokio::test]
async fn rows_survive_a_retire_and_only_read_only_or_hidden_is_storable() {
    let h = ResolutionHarness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let conn = h.db.conn().expect("connection");
    let all = AccessScope::allow_all();
    AccessRepo
        .upsert(
            &conn,
            &all,
            draft(d, h.tree.b, TenantAccess::ReadOnly),
            None,
        )
        .await
        .expect("insert");
    h.retire(d).await;
    assert!(
        AccessRepo
            .find_one(&conn, &all, d, h.tree.b)
            .await
            .expect("lookup")
            .is_some()
    );

    // `overridable` is the absence of a row; the schema refuses it as a value.
    let refused = AccessRepo
        .upsert(
            &conn,
            &all,
            draft(d, h.tree.a, TenantAccess::Overridable),
            None,
        )
        .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn the_row_write_itself_is_conditional_on_the_version_the_tag_was_compared_against() {
    let h = ResolutionHarness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let conn = h.db.conn().expect("connection");
    let all = AccessScope::allow_all();
    let tenant = h.tree.a;
    let first = AccessRepo
        .upsert(&conn, &all, draft(d, tenant, TenantAccess::ReadOnly), None)
        .await
        .expect("insert");

    // A second delegate that compared the absent-state tag finds the row
    // taken: the unique index refuses the insert as a stale precondition,
    // neither a conflict nor a fault.
    let refused = AccessRepo
        .upsert(&conn, &all, draft(d, tenant, TenantAccess::Hidden), None)
        .await
        .expect_err("a row appeared");
    assert!(
        matches!(refused, DomainError::PreconditionFailed { .. }),
        "{refused:?}"
    );

    // A version that moved since the comparison matches no row.
    let stale = first.updated_at - time::Duration::seconds(1);
    let refused = AccessRepo
        .upsert(
            &conn,
            &all,
            draft(d, tenant, TenantAccess::Hidden),
            Some(stale),
        )
        .await
        .expect_err("moved");
    assert!(
        matches!(refused, DomainError::PreconditionFailed { .. }),
        "{refused:?}"
    );
    let refused = AccessRepo
        .delete(&conn, &all, d, tenant, stale)
        .await
        .expect_err("moved");
    assert!(
        matches!(refused, DomainError::PreconditionFailed { .. }),
        "{refused:?}"
    );
    let kept = AccessRepo
        .find_one(&conn, &all, d, tenant)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(kept.access, TenantAccess::ReadOnly, "untouched");

    // At the version read, the write lands.
    let replaced = AccessRepo
        .upsert(
            &conn,
            &all,
            draft(d, tenant, TenantAccess::Hidden),
            Some(first.updated_at),
        )
        .await
        .expect("current version");
    assert_eq!(replaced.access, TenantAccess::Hidden);
    AccessRepo
        .delete(&conn, &all, d, tenant, replaced.updated_at)
        .await
        .expect("current version");
}
