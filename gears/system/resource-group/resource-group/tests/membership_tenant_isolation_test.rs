// Created: 2026-11-11 by Constructor Tech
#![allow(clippy::expect_used, clippy::doc_markdown)]
//! Membership tenant isolation tests (issue 5585).
//!
//! The membership table has no scope columns, so the caller's tenant scope is
//! applied through the owning group. These tests run the `MembershipService`
//! with a PDP that returns `In(OWNER_TENANT_ID, [subject tenant])` and check
//! that a caller from tenant B cannot list, add into or remove from groups of
//! tenant A, and that a foreign group looks exactly like a missing one.

mod common;

use std::sync::Arc;

use common::CreateTypeRequest;
use common::{
    create_root_group, create_root_type, make_ctx, make_group_service, make_membership_service,
    make_type_service, test_db,
};
use resource_group::domain::error::DomainError;
use resource_group::domain::membership_service::MembershipService;
use resource_group::domain::repo::MembershipRepositoryTrait;
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::membership_repo::MembershipRepository;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::GTS_ID_PREFIX;
use toolkit_odata::ODataQuery;
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

type MbrSvc = MembershipService<GroupRepository, MembershipRepository>;

struct Fixture {
    db: Arc<DBProvider<DbError>>,
    member_type: String,
    grp_code: String,
    tenant_a: Uuid,
    tenant_b: Uuid,
    group_a: Uuid,
    group_b: Uuid,
    /// Service under test: the PDP permits and scopes to the caller's tenant.
    /// Its `*_unscoped` methods skip the PDP and serve seeding and checks of
    /// what is stored.
    svc: MbrSvc,
}

/// Two tenants, one group each, one seeded membership in each group.
async fn setup() -> Fixture {
    let db = test_db().await;
    let type_svc = make_type_service(db.clone());
    let group_svc = make_group_service(db.clone());
    let svc = make_membership_service(db.clone());

    let member_type = create_root_type(&type_svc, "isombr").await;
    let grp_code = format!(
        "{GTS_ID_PREFIX}cf.core.rg.type.v1~x.test.isogrp.i{}.v1~",
        Uuid::now_v7().as_simple()
    );
    type_svc
        .create_type_unscoped(CreateTypeRequest {
            code: grp_code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![member_type.code.clone()],
            metadata_schema: None,
        })
        .await
        .expect("create group type");

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let group_a = create_root_group(&group_svc, &make_ctx(tenant_a), &grp_code, "GA", tenant_a)
        .await
        .id;
    let group_b = create_root_group(&group_svc, &make_ctx(tenant_b), &grp_code, "GB", tenant_b)
        .await
        .id;

    svc.add_membership(&make_ctx(tenant_a), group_a, &member_type.code, "res-a")
        .await
        .expect("seed membership in A");
    svc.add_membership(&make_ctx(tenant_b), group_b, &member_type.code, "res-b")
        .await
        .expect("seed membership in B");

    Fixture {
        db,
        member_type: member_type.code,
        grp_code,
        tenant_a,
        tenant_b,
        group_a,
        group_b,
        svc,
    }
}

async fn list_all_unscoped(f: &Fixture) -> Vec<(Uuid, String)> {
    let page = f
        .svc
        .list_memberships_unscoped(&ODataQuery::default())
        .await
        .expect("unscoped list");
    page.items
        .into_iter()
        .map(|m| (m.group_id, m.resource_id))
        .collect()
}

#[tokio::test]
async fn list_is_limited_to_callers_tenant() {
    let f = setup().await;

    let page_b = f
        .svc
        .list_memberships(&make_ctx(f.tenant_b), &ODataQuery::default())
        .await
        .expect("list as B");
    assert_eq!(page_b.items.len(), 1);
    assert_eq!(page_b.items[0].group_id, f.group_b);
    assert_eq!(page_b.items[0].resource_id, "res-b");

    let page_a = f
        .svc
        .list_memberships(&make_ctx(f.tenant_a), &ODataQuery::default())
        .await
        .expect("list as A");
    assert_eq!(page_a.items.len(), 1);
    assert_eq!(page_a.items[0].group_id, f.group_a);
    assert_eq!(page_a.items[0].resource_id, "res-a");

    let all = list_all_unscoped(&f).await;
    assert_eq!(all.len(), 2, "unscoped list still sees both tenants");
}

#[tokio::test]
async fn list_with_filter_on_foreign_group_is_empty() {
    let f = setup().await;

    let filter = toolkit_odata::parse_filter_string(&format!("group_id eq {}", f.group_a))
        .expect("parse filter");
    let query = ODataQuery::new().with_filter(filter.into_expr());

    let page = f
        .svc
        .list_memberships(&make_ctx(f.tenant_b), &query)
        .await
        .expect("filtered list as B");
    assert!(page.items.is_empty(), "foreign group must yield no rows");

    let page = f
        .svc
        .list_memberships(&make_ctx(f.tenant_a), &query)
        .await
        .expect("filtered list as A");
    assert_eq!(page.items.len(), 1, "owner still sees its own row");
}

/// A filter that names both the foreign group and a resource id of tenant A
/// must not widen what tenant B sees: the scope applies on top of the filter.
#[tokio::test]
async fn list_with_or_filter_on_foreign_group_and_resource_is_empty() {
    let f = setup().await;

    let filter = toolkit_odata::parse_filter_string(&format!(
        "group_id eq {} or resource_id eq 'res-a'",
        f.group_a
    ))
    .expect("parse filter");
    let query = ODataQuery::new().with_filter(filter.into_expr());

    let page = f
        .svc
        .list_memberships(&make_ctx(f.tenant_b), &query)
        .await
        .expect("filtered list as B");
    assert!(page.items.is_empty(), "foreign rows must not leak via OR");

    let page = f
        .svc
        .list_memberships(&make_ctx(f.tenant_a), &query)
        .await
        .expect("filtered list as A");
    assert_eq!(page.items.len(), 1, "owner sees exactly its own row");
    assert_eq!(page.items[0].group_id, f.group_a);
    assert_eq!(page.items[0].resource_id, "res-a");
}

/// Groups are created alternating B1, A1, B2, A2, B3, A3, so the newest group
/// (first in the default `group_id DESC` order) belongs to tenant A. Each
/// group holds one membership, so keyset pagination on `group_id` is well
/// defined. Paging as tenant B must yield exactly B's rows, and no page may
/// contain a row of A.
#[tokio::test]
async fn paging_as_tenant_b_never_returns_tenant_a_rows() {
    let f = setup().await;
    let group_svc = make_group_service(f.db.clone());

    let mut expected_b = vec![(f.group_b, "res-b".to_owned())];
    let mut rows_a = vec![(f.group_a, "res-a".to_owned())];
    for i in 0..3 {
        for (tenant, rows) in [(f.tenant_b, &mut expected_b), (f.tenant_a, &mut rows_a)] {
            let group = create_root_group(
                &group_svc,
                &make_ctx(tenant),
                &f.grp_code,
                &format!("G{i}"),
                tenant,
            )
            .await;
            let res_id = format!("res-{tenant}-{i}");
            f.svc
                .add_membership(&make_ctx(tenant), group.id, &f.member_type, &res_id)
                .await
                .expect("seed membership");
            rows.push((group.id, res_id));
        }
    }

    let mut collected = Vec::new();
    let mut query = ODataQuery::default().with_limit(2);
    let mut pages = 0;
    loop {
        let page = f
            .svc
            .list_memberships(&make_ctx(f.tenant_b), &query)
            .await
            .expect("page as B");
        pages += 1;
        for m in &page.items {
            assert!(
                !rows_a.iter().any(|(g, _)| *g == m.group_id),
                "page contained a row of tenant A: {m:?}"
            );
            collected.push((m.group_id, m.resource_id.clone()));
        }
        match page.page_info.next_cursor {
            Some(cursor) => {
                query = ODataQuery::default()
                    .with_limit(2)
                    .with_cursor(toolkit_odata::CursorV1::decode(&cursor).expect("cursor"));
            }
            None => break,
        }
        assert!(pages < 10, "paging must terminate");
    }

    collected.sort();
    expected_b.sort();
    assert_eq!(collected, expected_b);
}

#[tokio::test]
async fn add_into_foreign_group_looks_like_missing_group() {
    let f = setup().await;
    let ctx_b = make_ctx(f.tenant_b);

    let foreign = f
        .svc
        .add_membership(&ctx_b, f.group_a, &f.member_type, "intruder")
        .await
        .expect_err("add into foreign group must fail");
    let missing_id = Uuid::now_v7();
    let missing = f
        .svc
        .add_membership(&ctx_b, missing_id, &f.member_type, "intruder")
        .await
        .expect_err("add into missing group must fail");

    assert!(
        matches!(foreign, DomainError::GroupNotFound { id } if id == f.group_a),
        "got: {foreign:?}"
    );
    assert!(
        matches!(missing, DomainError::GroupNotFound { id } if id == missing_id),
        "got: {missing:?}"
    );
    // Same message shape once the requested id is masked out.
    let foreign_msg = foreign.to_string().replace(&f.group_a.to_string(), "<id>");
    let missing_msg = missing.to_string().replace(&missing_id.to_string(), "<id>");
    assert_eq!(foreign_msg, missing_msg);

    let all = list_all_unscoped(&f).await;
    assert!(
        !all.iter().any(|(_, r)| r == "intruder"),
        "no row may be inserted"
    );
}

/// Whatever is wrong with the request beyond the group, a caller that cannot
/// see the group gets the same answer as for a missing group: the group check
/// comes first, so neither an unregistered nor a disallowed resource type
/// reveals that the group exists.
async fn assert_foreign_add_looks_like_missing(f: &Fixture, resource_type: &str) {
    let ctx_b = make_ctx(f.tenant_b);

    let foreign = f
        .svc
        .add_membership(&ctx_b, f.group_a, resource_type, "intruder")
        .await
        .expect_err("add into foreign group must fail");
    let missing_id = Uuid::now_v7();
    let missing = f
        .svc
        .add_membership(&ctx_b, missing_id, resource_type, "intruder")
        .await
        .expect_err("add into missing group must fail");

    assert!(
        matches!(foreign, DomainError::GroupNotFound { id } if id == f.group_a),
        "got: {foreign:?}"
    );
    assert!(
        matches!(missing, DomainError::GroupNotFound { id } if id == missing_id),
        "got: {missing:?}"
    );
    let foreign_msg = foreign.to_string().replace(&f.group_a.to_string(), "<id>");
    let missing_msg = missing.to_string().replace(&missing_id.to_string(), "<id>");
    assert_eq!(foreign_msg, missing_msg);

    let all = list_all_unscoped(f).await;
    assert!(
        !all.iter().any(|(_, r)| r == "intruder"),
        "no row may be inserted"
    );
}

#[tokio::test]
async fn add_foreign_group_with_unregistered_type_looks_like_missing_group() {
    let f = setup().await;
    let unregistered = format!(
        "{GTS_ID_PREFIX}cf.core.rg.type.v1~x.test.nosuch.i{}.v1~",
        Uuid::now_v7().as_simple()
    );
    assert_foreign_add_looks_like_missing(&f, &unregistered).await;
}

#[tokio::test]
async fn add_foreign_group_with_disallowed_type_looks_like_missing_group() {
    let f = setup().await;
    let type_svc = make_type_service(f.db.clone());
    let other = create_root_type(&type_svc, "othermbr").await;
    assert_foreign_add_looks_like_missing(&f, &other.code).await;
}

#[tokio::test]
async fn remove_foreign_membership_looks_like_missing_membership() {
    let f = setup().await;
    let ctx_b = make_ctx(f.tenant_b);
    let missing_group = Uuid::now_v7();

    let foreign = f
        .svc
        .remove_membership(&ctx_b, f.group_a, &f.member_type, "res-a")
        .await
        .expect_err("remove foreign membership must fail");
    let missing = f
        .svc
        .remove_membership(&ctx_b, f.group_b, &f.member_type, "no-such-resource")
        .await
        .expect_err("remove missing membership must fail");
    let no_group = f
        .svc
        .remove_membership(&ctx_b, missing_group, &f.member_type, "res-x")
        .await
        .expect_err("remove from nonexistent group must fail");

    for err in [&foreign, &missing, &no_group] {
        assert!(
            matches!(err, DomainError::MembershipNotFound { .. }),
            "got: {err:?}"
        );
    }

    // Rendered messages are equal once the ids the caller itself sent are masked.
    let mask = |err: &DomainError, group: Uuid, res: &str| {
        err.to_string()
            .replace(&group.to_string(), "<group>")
            .replace(res, "<res>")
    };
    let foreign_msg = mask(&foreign, f.group_a, "res-a");
    let missing_msg = mask(&missing, f.group_b, "no-such-resource");
    let no_group_msg = mask(&no_group, missing_group, "res-x");
    assert_eq!(foreign_msg, missing_msg);
    assert_eq!(foreign_msg, no_group_msg);

    let all = list_all_unscoped(&f).await;
    assert!(
        all.contains(&(f.group_a, "res-a".to_owned())),
        "A's membership must survive"
    );
}

#[tokio::test]
async fn owner_can_add_list_and_remove_in_own_group() {
    let f = setup().await;

    for (tenant, group) in [(f.tenant_a, f.group_a), (f.tenant_b, f.group_b)] {
        let ctx = make_ctx(tenant);
        let res_id = format!("own-{tenant}");
        f.svc
            .add_membership(&ctx, group, &f.member_type, &res_id)
            .await
            .expect("add into own group");

        let page = f
            .svc
            .list_memberships(&ctx, &ODataQuery::default())
            .await
            .expect("list own");
        assert!(
            page.items
                .iter()
                .any(|m| m.group_id == group && m.resource_id == res_id)
        );
        assert!(page.items.iter().all(|m| m.group_id == group));

        f.svc
            .remove_membership(&ctx, group, &f.member_type, &res_id)
            .await
            .expect("remove from own group");
        let all = list_all_unscoped(&f).await;
        assert!(!all.contains(&(group, res_id)));
    }
}

// -- Repository level: the scope is applied through the owning group --

/// Memberships key the member type by its GTS identifier.
fn type_id(f: &Fixture) -> String {
    f.member_type.clone()
}

#[tokio::test]
async fn repo_list_under_deny_all_scope_is_empty() {
    let f = setup().await;
    let conn = f.db.conn().expect("conn");
    let page = MembershipRepository
        .list_memberships(&conn, &AccessScope::deny_all(), &ODataQuery::default())
        .await
        .expect("list");
    assert!(page.items.is_empty());
}

#[tokio::test]
async fn repo_delete_under_deny_all_scope_deletes_nothing() {
    let f = setup().await;
    let conn = f.db.conn().expect("conn");
    let deleted = MembershipRepository
        .delete(
            &conn,
            &AccessScope::deny_all(),
            f.group_a,
            &type_id(&f),
            "res-a",
        )
        .await
        .expect("delete");
    assert_eq!(deleted, 0);
    assert!(
        list_all_unscoped(&f)
            .await
            .contains(&(f.group_a, "res-a".to_owned()))
    );
}

/// A scope that constrains by resource id would be read as a group id once
/// applied to the group entity, so it must grant nothing.
#[tokio::test]
async fn repo_resource_id_scope_fails_closed() {
    let f = setup().await;
    let conn = f.db.conn().expect("conn");
    let scope = AccessScope::for_resource(f.group_a);

    let page = MembershipRepository
        .list_memberships(&conn, &scope, &ODataQuery::default())
        .await
        .expect("list");
    assert!(page.items.is_empty());

    let deleted = MembershipRepository
        .delete(&conn, &scope, f.group_a, &type_id(&f), "res-a")
        .await
        .expect("delete");
    assert_eq!(deleted, 0);
    assert!(
        list_all_unscoped(&f)
            .await
            .contains(&(f.group_a, "res-a".to_owned()))
    );
}

/// One constraint with a foreign property closes the whole scope, including
/// the constraint that would otherwise match tenant B.
#[tokio::test]
async fn repo_mixed_scope_fails_closed() {
    let f = setup().await;
    let conn = f.db.conn().expect("conn");
    let scope = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![ScopeFilter::in_uuids(
            pep_properties::OWNER_TENANT_ID,
            vec![f.tenant_b],
        )]),
        ScopeConstraint::new(vec![ScopeFilter::in_uuids(
            pep_properties::RESOURCE_ID,
            vec![f.group_a],
        )]),
    ]);

    let page = MembershipRepository
        .list_memberships(&conn, &scope, &ODataQuery::default())
        .await
        .expect("list");
    assert!(page.items.is_empty(), "whole scope must be closed");

    let deleted = MembershipRepository
        .delete(&conn, &scope, f.group_b, &type_id(&f), "res-b")
        .await
        .expect("delete");
    assert_eq!(deleted, 0, "whole scope must be closed");
    assert!(
        list_all_unscoped(&f)
            .await
            .contains(&(f.group_b, "res-b".to_owned()))
    );
}

#[tokio::test]
async fn repo_list_respects_tenant_scopes() {
    let f = setup().await;
    let conn = f.db.conn().expect("conn");
    let query = ODataQuery::default();

    let only_a = MembershipRepository
        .list_memberships(&conn, &AccessScope::for_tenant(f.tenant_a), &query)
        .await
        .expect("list A");
    assert_eq!(only_a.items.len(), 1);
    assert_eq!(only_a.items[0].group_id, f.group_a);

    let both = MembershipRepository
        .list_memberships(
            &conn,
            &AccessScope::for_tenants(vec![f.tenant_a, f.tenant_b]),
            &query,
        )
        .await
        .expect("list both");
    assert_eq!(both.items.len(), 2);
}

#[tokio::test]
async fn repo_delete_respects_tenant_scopes() {
    let f = setup().await;
    let conn = f.db.conn().expect("conn");
    let type_id = type_id(&f);

    let by_b = MembershipRepository
        .delete(
            &conn,
            &AccessScope::for_tenant(f.tenant_b),
            f.group_a,
            &type_id,
            "res-a",
        )
        .await
        .expect("delete as B");
    assert_eq!(by_b, 0, "B's scope must not reach A's row");
    assert!(
        list_all_unscoped(&f)
            .await
            .contains(&(f.group_a, "res-a".to_owned()))
    );

    let by_a = MembershipRepository
        .delete(
            &conn,
            &AccessScope::for_tenant(f.tenant_a),
            f.group_a,
            &type_id,
            "res-a",
        )
        .await
        .expect("delete as A");
    assert_eq!(by_a, 1);
}
