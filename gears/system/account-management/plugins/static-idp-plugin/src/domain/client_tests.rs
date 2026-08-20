//! Pin the cursor-walk contract on the static `IdP` plugin's
//! `list_users` so a snapshot larger than `top` stays fully reachable
//! across page hops. Previously the plugin truncated to `top` and
//! signalled `next_cursor: None` unconditionally, silently dropping
//! every row past the first page; the regression guard below pins the
//! new `CursorV1` key-tuple cursor walk end-to-end.

use std::collections::HashSet;
use toolkit_gts::gts_id;

use account_management_sdk::{
    IdpDeprovisionTenantRequest, IdpDeprovisionUserRequest, IdpListServiceAccountsRequest,
    IdpListUsersRequest, IdpNewUser, IdpPluginClient, IdpProvisionServiceAccountRequest,
    IdpProvisionTenantRequest, IdpProvisionUserRequest, IdpRevokeServiceAccountRequest,
    IdpRotateServiceAccountSecretRequest, IdpServiceAccountFailure, IdpTenantContext,
    IdpUpdateUserRequest, IdpUserDuplicateField, IdpUserFilterField, IdpUserOperationFailure,
    IdpUserPagination, IdpUserPatch,
};
use serde_json::{Value, json};
use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::Service;

fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

const TENANT_TYPE: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~");

fn tenant_type() -> gts::GtsTypeId {
    gts::GtsTypeId::new(TENANT_TYPE)
}

fn tenant_ctx(tenant_id: Uuid) -> IdpTenantContext {
    IdpTenantContext::new(tenant_id, "static-idp-plugin-test", tenant_type(), None)
}

fn req(tenant_id: Uuid, top: u32, cursor: Option<&str>) -> IdpListUsersRequest {
    let pagination =
        IdpUserPagination::new(top, cursor.map(str::to_owned)).expect("pagination shape is valid");
    IdpListUsersRequest::new(tenant_ctx(tenant_id), pagination)
}

fn seed(svc: &Service, tenant_id: Uuid, count: usize) {
    for i in 0..count {
        let payload = IdpNewUser::new(format!("user-{i:03}"));
        let user = Service::echo_user(tenant_id, &payload);
        svc.record_user(tenant_id, user);
    }
}

#[tokio::test]
async fn empty_snapshot_returns_empty_page_without_cursors() {
    let svc = Service::new();
    let page = svc
        .list_users(&ctx(), &req(Uuid::new_v4(), 50, None))
        .await
        .expect("empty list");
    assert!(page.items.is_empty());
    assert!(page.page_info.next_cursor.is_none());
    assert!(page.page_info.prev_cursor.is_none());
}

#[tokio::test]
async fn page_size_at_least_snapshot_returns_one_page_no_next() {
    let svc = Service::new();
    let tenant = Uuid::new_v4();
    seed(&svc, tenant, 3);
    let page = svc
        .list_users(&ctx(), &req(tenant, 10, None))
        .await
        .expect("page");
    assert_eq!(page.items.len(), 3);
    assert!(page.page_info.next_cursor.is_none());
    assert!(page.page_info.prev_cursor.is_none());
}

#[tokio::test]
async fn cursor_walk_covers_full_snapshot_without_loss_or_duplication() {
    let svc = Service::new();
    let tenant = Uuid::new_v4();
    seed(&svc, tenant, 7);

    let top = 3;
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut cursor: Option<String> = None;
    let mut pages: usize = 0;
    loop {
        let page = svc
            .list_users(&ctx(), &req(tenant, top, cursor.as_deref()))
            .await
            .expect("paged list");
        assert!(
            !page.items.is_empty(),
            "every page in the walk MUST carry at least one row"
        );
        for user in &page.items {
            assert!(
                seen.insert(user.id),
                "cursor walk produced a duplicate user id {} across pages",
                user.id,
            );
        }
        pages += 1;
        match page.page_info.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
        assert!(pages < 10, "cursor walk failed to terminate");
    }
    assert_eq!(seen.len(), 7, "cursor walk MUST surface every seeded user");
    assert_eq!(pages, 3, "7 rows at top=3 -> 3 pages (3 + 3 + 1)");
}

#[tokio::test]
async fn final_page_carries_no_forward_or_backward_cursor() {
    // Under CursorV1 a caller can no longer synthesise an "offset past
    // the end" — every cursor is the projected key tuple of an item
    // that was already returned. The terminator contract is therefore
    // pinned by walking to the last page and asserting it carries no
    // forward token and (since the plugin is forward-only) no backward
    // token either. A client that followed `prev_cursor` blindly would
    // walk backwards from "past the end"; that ambiguity is structurally
    // ruled out here.
    let svc = Service::new();
    let tenant = Uuid::new_v4();
    seed(&svc, tenant, 3);

    let page1 = svc
        .list_users(&ctx(), &req(tenant, 2, None))
        .await
        .expect("page 1");
    assert_eq!(page1.items.len(), 2);
    let cur = page1
        .page_info
        .next_cursor
        .clone()
        .expect("page 1 must carry a forward cursor (3 rows / top=2)");

    let page2 = svc
        .list_users(&ctx(), &req(tenant, 2, Some(cur.as_str())))
        .await
        .expect("page 2");
    assert_eq!(page2.items.len(), 1, "final page carries the remaining row");
    assert!(
        page2.page_info.next_cursor.is_none(),
        "final page MUST NOT carry a forward cursor"
    );
    assert!(
        page2.page_info.prev_cursor.is_none(),
        "plugin is forward-only; prev_cursor MUST always be None"
    );
}

#[tokio::test]
async fn invalid_cursor_surfaces_as_rejected() {
    // CursorV1 expects a base64url-encoded JSON envelope; this string
    // fails the base64 decode step. A hostile / buggy client must not
    // be able to smuggle arbitrary state through the cursor field —
    // any malformed token MUST surface as Rejected.
    let svc = Service::new();
    let tenant = Uuid::new_v4();
    seed(&svc, tenant, 1);
    let err = svc
        .list_users(
            &ctx(),
            &req(tenant, 10, Some("not-a-valid-base64-cursor!!!")),
        )
        .await
        .expect_err("malformed cursor MUST be rejected");
    assert!(
        matches!(err, IdpUserOperationFailure::Rejected { .. }),
        "expected Rejected on malformed cursor, got {err:?}",
    );
}

// ── provision_tenant ──────────────────────────────────────────────────

#[tokio::test]
async fn provision_tenant_root_returns_echo_metadata() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let request = IdpProvisionTenantRequest::for_root(tenant_id, "root-corp", tenant_type());

    let result = svc
        .provision_tenant(&ctx(), &request)
        .await
        .expect("provision ok");
    let metadata = result
        .metadata
        .expect("provision_tenant MUST emit Some metadata");

    assert_eq!(metadata["echo"], json!(true));
    assert_eq!(metadata["tenant_id"], json!(tenant_id));
    assert_eq!(metadata["tenant_name"], json!("root-corp"));
    assert_eq!(metadata["tenant_type"], json!(TENANT_TYPE));
    assert_eq!(metadata["target"], json!("root"));
    assert_eq!(metadata["parent_id"], Value::Null);
    assert_eq!(metadata["provisioning_metadata"], Value::Null);
}

#[tokio::test]
async fn provision_tenant_child_carries_parent_id_and_echoed_provisioning_metadata() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let parent_id = Uuid::new_v4();
    let request = IdpProvisionTenantRequest::new(tenant_id, parent_id, "acme", tenant_type())
        .with_metadata(json!({"realm": "acme-keycloak", "region": "eu-west-1"}));

    let result = svc
        .provision_tenant(&ctx(), &request)
        .await
        .expect("provision ok");
    let metadata = result
        .metadata
        .expect("provision_tenant MUST emit Some metadata");

    assert_eq!(metadata["target"], json!("child"));
    assert_eq!(metadata["parent_id"], json!(parent_id));
    assert_eq!(
        metadata["provisioning_metadata"],
        json!({"realm": "acme-keycloak", "region": "eu-west-1"}),
        "provisioning_metadata MUST be echoed verbatim",
    );
}

#[tokio::test]
async fn provision_tenant_is_deterministic_across_invocations() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let parent_id = Uuid::new_v4();
    let request = IdpProvisionTenantRequest::new(tenant_id, parent_id, "acme", tenant_type());

    let a = svc.provision_tenant(&ctx(), &request).await.expect("first");
    let b = svc
        .provision_tenant(&ctx(), &request)
        .await
        .expect("second");
    assert_eq!(
        a.metadata, b.metadata,
        "echo metadata MUST be a pure function of the input request"
    );
}

// ── deprovision_tenant ────────────────────────────────────────────────

#[tokio::test]
async fn deprovision_tenant_always_succeeds() {
    let svc = Service::new();
    let request = IdpDeprovisionTenantRequest::new(tenant_ctx(Uuid::new_v4()));
    svc.deprovision_tenant(&ctx(), &request)
        .await
        .expect("deprovision MUST succeed");
}

// ── provision_user ────────────────────────────────────────────────────

#[tokio::test]
async fn provision_user_records_user_and_returns_deterministic_id() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let payload = IdpNewUser::new("alice")
        .with_email("alice@example.com")
        .with_display_name("Alice");
    let request = IdpProvisionUserRequest::new(tenant_ctx(tenant_id), payload);

    let user_a = svc.provision_user(&ctx(), &request).await.expect("first");
    let user_b = svc.provision_user(&ctx(), &request).await.expect("second");

    assert_eq!(user_a.id, user_b.id, "same input MUST yield same UUIDv5");
    assert_eq!(user_a.username, "alice");
    assert_eq!(user_a.email.as_deref(), Some("alice@example.com"));
    assert_eq!(user_a.display_name.as_deref(), Some("Alice"));

    // The user must be observable through list_users.
    let page = svc
        .list_users(&ctx(), &req(tenant_id, 10, None))
        .await
        .expect("list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, user_a.id);
}

#[tokio::test]
async fn provision_user_different_tenants_yield_different_ids() {
    let svc = Service::new();
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let payload = IdpNewUser::new("alice");
    let ua = svc
        .provision_user(
            &ctx(),
            &IdpProvisionUserRequest::new(tenant_ctx(tenant_a), payload.clone()),
        )
        .await
        .expect("a");
    let ub = svc
        .provision_user(
            &ctx(),
            &IdpProvisionUserRequest::new(tenant_ctx(tenant_b), payload),
        )
        .await
        .expect("b");
    assert_ne!(
        ua.id, ub.id,
        "tenant scope MUST namespace the derived user id"
    );
}

#[tokio::test]
async fn provision_user_re_provision_overwrites_with_new_payload() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let req_one = IdpProvisionUserRequest::new(
        tenant_ctx(tenant_id),
        IdpNewUser::new("bob").with_email("bob@old.example.com"),
    );
    let req_two = IdpProvisionUserRequest::new(
        tenant_ctx(tenant_id),
        IdpNewUser::new("bob")
            .with_email("bob@new.example.com")
            .with_display_name("Bob"),
    );

    let first = svc.provision_user(&ctx(), &req_one).await.expect("first");
    let second = svc.provision_user(&ctx(), &req_two).await.expect("second");
    assert_eq!(first.id, second.id);

    let page = svc
        .list_users(&ctx(), &req(tenant_id, 10, None))
        .await
        .expect("list");
    assert_eq!(
        page.items.len(),
        1,
        "re-provision MUST overwrite, not append"
    );
    assert_eq!(
        page.items[0].email.as_deref(),
        Some("bob@new.example.com"),
        "post-overwrite snapshot MUST reflect the new payload"
    );
    assert_eq!(page.items[0].display_name.as_deref(), Some("Bob"));
}

// ── deprovision_user ──────────────────────────────────────────────────

#[tokio::test]
async fn deprovision_user_removes_existing_user() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let payload = IdpNewUser::new("carol");
    let user = svc
        .provision_user(
            &ctx(),
            &IdpProvisionUserRequest::new(tenant_ctx(tenant_id), payload),
        )
        .await
        .expect("provision");

    svc.deprovision_user(
        &ctx(),
        &IdpDeprovisionUserRequest::new(tenant_ctx(tenant_id), user.id),
    )
    .await
    .expect("deprovision");

    let page = svc
        .list_users(&ctx(), &req(tenant_id, 10, None))
        .await
        .expect("list");
    assert!(
        page.items.is_empty(),
        "deprovision MUST remove the row from the per-tenant cache"
    );
}

#[tokio::test]
async fn deprovision_user_is_idempotent_when_already_absent() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    // Never provisioned — the call still resolves to Ok per the SDK
    // contract (`removed` and `already-absent` are both success).
    svc.deprovision_user(
        &ctx(),
        &IdpDeprovisionUserRequest::new(tenant_ctx(tenant_id), Uuid::new_v4()),
    )
    .await
    .expect("absent deprovision MUST be Ok");
}

// ── id eq filter existence-check ──────────────────────────────────────

#[tokio::test]
async fn list_users_with_id_eq_filter_returns_single_row_or_empty() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let user = svc
        .provision_user(
            &ctx(),
            &IdpProvisionUserRequest::new(tenant_ctx(tenant_id), IdpNewUser::new("dave")),
        )
        .await
        .expect("provision");

    // Hit: filter on the known id.
    let hit_pagination = IdpUserPagination::new(50, None).expect("pagination");
    let hit = svc
        .list_users(
            &ctx(),
            &IdpListUsersRequest::new(tenant_ctx(tenant_id), hit_pagination).with_filter(
                FilterNode::binary(
                    IdpUserFilterField::Id,
                    FilterOp::Eq,
                    ODataValue::Uuid(user.id),
                ),
            ),
        )
        .await
        .expect("filtered list hit");
    assert_eq!(hit.items.len(), 1);
    assert_eq!(hit.items[0].id, user.id);

    // Miss: filter on an unknown id.
    let miss_pagination = IdpUserPagination::new(50, None).expect("pagination");
    let miss = svc
        .list_users(
            &ctx(),
            &IdpListUsersRequest::new(tenant_ctx(tenant_id), miss_pagination).with_filter(
                FilterNode::binary(
                    IdpUserFilterField::Id,
                    FilterOp::Eq,
                    ODataValue::Uuid(Uuid::new_v4()),
                ),
            ),
        )
        .await
        .expect("filtered list miss");
    assert!(
        miss.items.is_empty(),
        "id eq filter on absent id MUST surface an empty page"
    );
}

#[tokio::test]
async fn provisioned_user_round_trips_first_last_name_through_list_users() {
    let svc = Service::new();
    let tenant = Uuid::new_v4();

    let req_provision = IdpProvisionUserRequest::new(
        tenant_ctx(tenant),
        IdpNewUser::new("alice")
            .with_first_name("Alice")
            .with_last_name("Anderson"),
    );
    let provisioned = svc
        .provision_user(&ctx(), &req_provision)
        .await
        .expect("provision succeeds");
    assert_eq!(provisioned.first_name.as_deref(), Some("Alice"));
    assert_eq!(provisioned.last_name.as_deref(), Some("Anderson"));

    let page = svc
        .list_users(&ctx(), &req(tenant, 10, None))
        .await
        .expect("list succeeds");
    let echoed = page
        .items
        .iter()
        .find(|u| u.username == "alice")
        .expect("alice surfaces in list");
    assert_eq!(echoed.first_name.as_deref(), Some("Alice"));
    assert_eq!(echoed.last_name.as_deref(), Some("Anderson"));
}

#[tokio::test]
async fn list_users_filter_eq_username_returns_only_matching_user() {
    use account_management_sdk::IdpUserFilterField;
    use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};

    let svc = Service::new();
    let tenant = Uuid::new_v4();
    // Seed two users via provision_user so first_name/last_name and the
    // IdpUser shape match production behaviour.
    for (uname, fname) in [("alice", "Alice"), ("bob", "Bob")] {
        let req = IdpProvisionUserRequest::new(
            tenant_ctx(tenant),
            IdpNewUser::new(uname).with_first_name(fname),
        );
        svc.provision_user(&ctx(), &req).await.expect("provision");
    }

    let req = IdpListUsersRequest::new(
        tenant_ctx(tenant),
        IdpUserPagination::new(50, None).expect("valid pagination"),
    )
    .with_filter(FilterNode::binary(
        IdpUserFilterField::Username,
        FilterOp::Eq,
        ODataValue::String("alice".into()),
    ));
    let page = svc.list_users(&ctx(), &req).await.expect("list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].username, "alice");
}

#[tokio::test]
async fn list_users_filter_contains_first_name_is_case_insensitive() {
    use account_management_sdk::IdpUserFilterField;
    use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};

    let svc = Service::new();
    let tenant = Uuid::new_v4();
    for (uname, fname) in [("alice", "Alice"), ("bob", "Bob")] {
        let req = IdpProvisionUserRequest::new(
            tenant_ctx(tenant),
            IdpNewUser::new(uname).with_first_name(fname),
        );
        svc.provision_user(&ctx(), &req).await.expect("provision");
    }
    // Lowercase needle finds capitalised "Alice".
    let req = IdpListUsersRequest::new(
        tenant_ctx(tenant),
        IdpUserPagination::new(50, None).expect("valid pagination"),
    )
    .with_filter(FilterNode::binary(
        IdpUserFilterField::FirstName,
        FilterOp::Contains,
        ODataValue::String("ali".into()),
    ));
    let page = svc.list_users(&ctx(), &req).await.expect("list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].username, "alice");
}

#[tokio::test]
async fn list_users_default_order_is_username_asc_with_id_tiebreaker() {
    let svc = Service::new();
    let tenant = Uuid::new_v4();
    for uname in ["carl", "alice", "bob"] {
        let req = IdpProvisionUserRequest::new(tenant_ctx(tenant), IdpNewUser::new(uname));
        svc.provision_user(&ctx(), &req).await.expect("provision");
    }
    // No order set on the request -> plugin must inject default.
    let req = IdpListUsersRequest::new(
        tenant_ctx(tenant),
        IdpUserPagination::new(50, None).expect("valid pagination"),
    );
    let page = svc.list_users(&ctx(), &req).await.expect("list");
    let names: Vec<&str> = page.items.iter().map(|u| u.username.as_str()).collect();
    assert_eq!(names, vec!["alice", "bob", "carl"], "default username ASC");
}

#[tokio::test]
async fn list_users_caller_order_last_name_desc_sorts_correctly_with_id_tiebreaker() {
    use toolkit_odata::{ODataOrderBy, OrderKey, SortDir};

    let svc = Service::new();
    let tenant = Uuid::new_v4();
    for (uname, lname) in [("u1", "Charlie"), ("u2", "Alpha"), ("u3", "Bravo")] {
        let req = IdpProvisionUserRequest::new(
            tenant_ctx(tenant),
            IdpNewUser::new(uname).with_last_name(lname),
        );
        svc.provision_user(&ctx(), &req).await.expect("provision");
    }
    let req = IdpListUsersRequest::new(
        tenant_ctx(tenant),
        IdpUserPagination::new(50, None).expect("valid pagination"),
    )
    .with_order(ODataOrderBy(vec![OrderKey {
        field: "last_name".into(),
        dir: SortDir::Desc,
    }]));
    let page = svc.list_users(&ctx(), &req).await.expect("list");
    let names: Vec<&str> = page.items.iter().map(|u| u.username.as_str()).collect();
    assert_eq!(names, vec!["u1", "u3", "u2"], "C, B, A by last_name desc");
}

#[tokio::test]
async fn list_users_order_id_eq_tiebreaker_is_idempotent() {
    use toolkit_odata::{ODataOrderBy, OrderKey, SortDir};

    let svc = Service::new();
    let tenant = Uuid::new_v4();
    for uname in ["c", "a", "b"] {
        let req = IdpProvisionUserRequest::new(tenant_ctx(tenant), IdpNewUser::new(uname));
        svc.provision_user(&ctx(), &req).await.expect("provision");
    }
    // Caller orders by id ASC explicitly; the plugin's
    // ensure_tiebreaker("id", Asc) must not append a duplicate
    // (idempotent: id is already in the keys).
    let req = IdpListUsersRequest::new(
        tenant_ctx(tenant),
        IdpUserPagination::new(50, None).expect("valid pagination"),
    )
    .with_order(ODataOrderBy(vec![OrderKey {
        field: "id".into(),
        dir: SortDir::Asc,
    }]));
    // The test only asserts no panic + a stable result; concrete row
    // order is a function of the v5 UUID derivation in Service::echo_user
    // and not pinned here.
    let page = svc.list_users(&ctx(), &req).await.expect("list");
    assert_eq!(page.items.len(), 3);
}

#[tokio::test]
async fn list_users_filter_and_composite_returns_intersection() {
    use account_management_sdk::IdpUserFilterField;
    use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};

    let svc = Service::new();
    let tenant = Uuid::new_v4();
    let seed = [
        ("alice", "A", "Anderson"),
        ("alex", "A", "Brown"),
        ("bob", "B", "Anderson"),
    ];
    for (uname, fname, lname) in seed {
        let req = IdpProvisionUserRequest::new(
            tenant_ctx(tenant),
            IdpNewUser::new(uname)
                .with_first_name(fname)
                .with_last_name(lname),
        );
        svc.provision_user(&ctx(), &req).await.expect("provision");
    }
    let req = IdpListUsersRequest::new(
        tenant_ctx(tenant),
        IdpUserPagination::new(50, None).expect("valid pagination"),
    )
    .with_filter(FilterNode::and(vec![
        FilterNode::binary(
            IdpUserFilterField::FirstName,
            FilterOp::Eq,
            ODataValue::String("A".into()),
        ),
        FilterNode::binary(
            IdpUserFilterField::LastName,
            FilterOp::Eq,
            ODataValue::String("Anderson".into()),
        ),
    ]));
    let page = svc.list_users(&ctx(), &req).await.expect("list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].username, "alice");
}

#[tokio::test]
async fn list_users_filtered_ordered_cursor_continues_across_pages() {
    use account_management_sdk::IdpUserFilterField;
    use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};

    let svc = Service::new();
    let tenant = Uuid::new_v4();
    // 5 matching, 2 non-matching (filtered out).
    for (uname, fname) in [
        ("u_a", "X"),
        ("u_b", "X"),
        ("u_c", "X"),
        ("u_d", "X"),
        ("u_e", "X"),
        ("noise_1", "Y"),
        ("noise_2", "Y"),
    ] {
        let req = IdpProvisionUserRequest::new(
            tenant_ctx(tenant),
            IdpNewUser::new(uname).with_first_name(fname),
        );
        svc.provision_user(&ctx(), &req).await.expect("provision");
    }

    let mk_req = |cursor: Option<String>| {
        IdpListUsersRequest::new(
            tenant_ctx(tenant),
            IdpUserPagination::new(2, cursor).expect("valid pagination"),
        )
        .with_filter(FilterNode::binary(
            IdpUserFilterField::FirstName,
            FilterOp::Eq,
            ODataValue::String("X".into()),
        ))
    };

    let p1 = svc.list_users(&ctx(), &mk_req(None)).await.expect("p1");
    let p1_names: Vec<&str> = p1.items.iter().map(|u| u.username.as_str()).collect();
    assert_eq!(p1_names, vec!["u_a", "u_b"]);
    let cur1 = p1.page_info.next_cursor.expect("page1 has next cursor");

    let p2 = svc
        .list_users(&ctx(), &mk_req(Some(cur1)))
        .await
        .expect("p2");
    let p2_names: Vec<&str> = p2.items.iter().map(|u| u.username.as_str()).collect();
    assert_eq!(p2_names, vec!["u_c", "u_d"]);
    let cur2 = p2.page_info.next_cursor.expect("page2 has next cursor");

    let p3 = svc
        .list_users(&ctx(), &mk_req(Some(cur2)))
        .await
        .expect("p3");
    let p3_names: Vec<&str> = p3.items.iter().map(|u| u.username.as_str()).collect();
    assert_eq!(p3_names, vec!["u_e"]);
    assert!(
        p3.page_info.next_cursor.is_none(),
        "final page has no next cursor"
    );
}

#[tokio::test]
async fn cursor_with_drifted_orderby_surfaces_as_rejected() {
    // Permanent regression guard for the order-drift detection wired via
    // toolkit_odata::validate_cursor_against. Page 1 is fetched under the
    // default order (`username ASC, id ASC` after tiebreaker injection);
    // page 2 reuses that cursor but with an explicit `$orderby=last_name
    // asc` -- a different signed-token form. The plugin MUST reject.
    use toolkit_odata::{ODataOrderBy, OrderKey, SortDir};

    let svc = Service::new();
    let tenant = Uuid::new_v4();
    for uname in ["alice", "bob", "carl"] {
        let req = IdpProvisionUserRequest::new(tenant_ctx(tenant), IdpNewUser::new(uname));
        svc.provision_user(&ctx(), &req).await.expect("provision");
    }

    let p1 = svc
        .list_users(
            &ctx(),
            &IdpListUsersRequest::new(
                tenant_ctx(tenant),
                IdpUserPagination::new(1, None).expect("valid pagination"),
            ),
        )
        .await
        .expect("page 1");
    let cur1 = p1
        .page_info
        .next_cursor
        .expect("page 1 emits a next cursor");

    let drifted = IdpListUsersRequest::new(
        tenant_ctx(tenant),
        IdpUserPagination::new(1, Some(cur1)).expect("valid pagination"),
    )
    .with_order(ODataOrderBy(vec![OrderKey {
        field: "last_name".into(),
        dir: SortDir::Asc,
    }]));
    let err = svc
        .list_users(&ctx(), &drifted)
        .await
        .expect_err("order drift MUST be rejected");
    let IdpUserOperationFailure::Rejected { detail } = err else {
        panic!("expected Rejected on order drift, got {err:?}");
    };
    assert!(
        detail.contains("$filter / $orderby"),
        "rejection detail mentions the contract: got {detail:?}"
    );
}

// ─── update_user ─────────────────────────────────────────────────────

/// Provision `username` (with an email) and return its stored id.
async fn provision(svc: &Service, tenant_id: Uuid, username: &str) -> Uuid {
    let payload =
        IdpNewUser::new(username.to_owned()).with_email(format!("{username}@example.com"));
    let req = IdpProvisionUserRequest::new(tenant_ctx(tenant_id), payload);
    svc.provision_user(&ctx(), &req)
        .await
        .expect("provision precondition")
        .id
}

#[tokio::test]
async fn update_user_applies_patch_and_returns_projection() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let id = provision(&svc, tenant_id, "alice").await;

    let patch = IdpUserPatch::new()
        .with_email(Some("new@example.com".to_owned()))
        .with_display_name(Some("Alice A.".to_owned()));
    let updated = svc
        .update_user(
            &ctx(),
            &IdpUpdateUserRequest::new(tenant_ctx(tenant_id), id, patch),
        )
        .await
        .expect("update applies");
    assert_eq!(updated.id, id, "id is stable");
    assert_eq!(updated.email.as_deref(), Some("new@example.com"));
    assert_eq!(updated.display_name.as_deref(), Some("Alice A."));
}

#[tokio::test]
async fn update_user_clears_nullable_field() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let id = provision(&svc, tenant_id, "bob").await;

    let patch = IdpUserPatch::new().with_email(None); // Some(None) → clear
    let updated = svc
        .update_user(
            &ctx(),
            &IdpUpdateUserRequest::new(tenant_ctx(tenant_id), id, patch),
        )
        .await
        .expect("update applies");
    assert!(updated.email.is_none(), "email cleared");
}

#[tokio::test]
async fn update_user_rename_keeps_stable_id() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let id = provision(&svc, tenant_id, "carol").await;

    let patch = IdpUserPatch::new().with_username("carol2");
    let updated = svc
        .update_user(
            &ctx(),
            &IdpUpdateUserRequest::new(tenant_ctx(tenant_id), id, patch),
        )
        .await
        .expect("rename applies");
    assert_eq!(updated.id, id, "rename does not re-key the user");
    assert_eq!(updated.username, "carol2");
}

#[tokio::test]
async fn update_user_absent_returns_not_found() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    let patch = IdpUserPatch::new().with_email(Some("x@example.com".to_owned()));
    let err = svc
        .update_user(
            &ctx(),
            &IdpUpdateUserRequest::new(tenant_ctx(tenant_id), Uuid::new_v4(), patch),
        )
        .await
        .expect_err("absent user must not be a silent success");
    assert!(matches!(err, IdpUserOperationFailure::NotFound { .. }));
}

#[tokio::test]
async fn update_user_rename_collision_returns_duplicate() {
    let svc = Service::new();
    let tenant_id = Uuid::new_v4();
    provision(&svc, tenant_id, "alice").await;
    let bob_id = provision(&svc, tenant_id, "bob").await;

    let patch = IdpUserPatch::new().with_username("alice");
    let err = svc
        .update_user(
            &ctx(),
            &IdpUpdateUserRequest::new(tenant_ctx(tenant_id), bob_id, patch),
        )
        .await
        .expect_err("rename onto an existing login must conflict");
    assert!(matches!(
        err,
        IdpUserOperationFailure::DuplicateUser {
            field: IdpUserDuplicateField::Username,
            ..
        }
    ));
}

// ─── Service accounts ──────────────────────────────────────────────
//
// The four overrides exist so AM's machine-identity surface is a working
// lifecycle in dev deploys and E2E rather than a uniform 501. These pin
// the two obligations a caller can actually observe — `(tenant_id, name)`
// uniqueness over live accounts, and a secret that changes on rotation
// — plus the scoped-addressing rule that keeps one tenant out of
// another's accounts.

fn sa_provision_req(tenant_id: Uuid, name: &str) -> IdpProvisionServiceAccountRequest {
    IdpProvisionServiceAccountRequest::new(
        tenant_ctx(tenant_id),
        name.to_owned(),
        vec!["platform.read".to_owned()],
    )
}

#[tokio::test]
async fn provision_service_account_returns_credentials_and_records_the_account() {
    let svc = Service::new();
    let tenant = Uuid::from_u128(0x5001);

    let creds = svc
        .provision_service_account(&ctx(), &sa_provision_req(tenant, "ci"))
        .await
        .expect("provision succeeds");

    assert_eq!(creds.client_id, format!("svc-{tenant}-ci"));
    assert!(!creds.token_url.is_empty(), "token_url must be populated");
    assert!(!creds.subject_id.is_nil(), "subject_id must be assigned");

    let listed = svc
        .list_service_accounts(
            &ctx(),
            &IdpListServiceAccountsRequest::new(tenant_ctx(tenant)),
        )
        .await
        .expect("list succeeds");
    assert_eq!(listed.len(), 1);
    // Reported verbatim: the only contractual bridge from a submitted
    // name back to an adapter-assigned client id.
    assert_eq!(listed[0].name, "ci");
    assert_eq!(listed[0].client_id, creds.client_id);
    assert!(listed[0].enabled);
    assert_eq!(listed[0].scopes, vec!["platform.read".to_owned()]);
}

/// The contract requires a name already live in the tenant to be refused
/// *and* the existing account left unrevealed — the failure carries no
/// client id, and AM discards the text anyway.
#[tokio::test]
async fn provision_service_account_rejects_a_name_already_live_in_the_tenant() {
    let svc = Service::new();
    let tenant = Uuid::from_u128(0x5002);
    let first = svc
        .provision_service_account(&ctx(), &sa_provision_req(tenant, "ci"))
        .await
        .expect("first provision succeeds");

    let err = svc
        .provision_service_account(&ctx(), &sa_provision_req(tenant, "ci"))
        .await
        .expect_err("duplicate name is refused");

    assert!(matches!(err, IdpServiceAccountFailure::InvalidInput { .. }));
    assert!(
        !err.detail().contains(&first.client_id),
        "the refusal must not reveal the existing account: {}",
        err.detail()
    );
    // The existing account is untouched — still exactly one, still
    // answering on its original credentials' client id.
    let listed = svc
        .list_service_accounts(
            &ctx(),
            &IdpListServiceAccountsRequest::new(tenant_ctx(tenant)),
        )
        .await
        .expect("list succeeds");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].client_id, first.client_id);
}

/// Uniqueness is per tenant, not global.
#[tokio::test]
async fn the_same_name_is_available_in_a_different_tenant() {
    let svc = Service::new();
    let (a, b) = (Uuid::from_u128(0x5003), Uuid::from_u128(0x5004));

    let ca = svc
        .provision_service_account(&ctx(), &sa_provision_req(a, "ci"))
        .await
        .expect("tenant a provision succeeds");
    let cb = svc
        .provision_service_account(&ctx(), &sa_provision_req(b, "ci"))
        .await
        .expect("tenant b provision succeeds");

    assert_ne!(ca.client_id, cb.client_id);
    for (tenant, expected) in [(a, &ca.client_id), (b, &cb.client_id)] {
        let listed = svc
            .list_service_accounts(
                &ctx(),
                &IdpListServiceAccountsRequest::new(tenant_ctx(tenant)),
            )
            .await
            .expect("list succeeds");
        assert_eq!(listed.len(), 1, "listing must be tenant-scoped");
        assert_eq!(&listed[0].client_id, expected);
    }
}

/// Rotation must be observably different from a no-op while leaving the
/// identity intact.
#[tokio::test]
async fn rotate_changes_the_secret_but_not_the_identity() {
    use secrecy::ExposeSecret as _;

    let svc = Service::new();
    let tenant = Uuid::from_u128(0x5005);
    let before = svc
        .provision_service_account(&ctx(), &sa_provision_req(tenant, "ci"))
        .await
        .expect("provision succeeds");

    let after = svc
        .rotate_service_account_secret(
            &ctx(),
            &IdpRotateServiceAccountSecretRequest::new(
                tenant_ctx(tenant),
                before.client_id.clone(),
            ),
        )
        .await
        .expect("rotate succeeds");

    assert_eq!(after.client_id, before.client_id, "client id is stable");
    assert_eq!(after.subject_id, before.subject_id, "subject id is stable");
    assert_ne!(
        after.client_secret.expose_secret(),
        before.client_secret.expose_secret(),
        "a rotation that returned the same secret would be a silent no-op"
    );
}

#[tokio::test]
async fn rotate_on_an_absent_account_is_not_found() {
    let svc = Service::new();
    let tenant = Uuid::from_u128(0x5006);

    let err = svc
        .rotate_service_account_secret(
            &ctx(),
            &IdpRotateServiceAccountSecretRequest::new(tenant_ctx(tenant), "svc-nope".to_owned()),
        )
        .await
        .expect_err("absent account is NotFound");

    assert!(matches!(err, IdpServiceAccountFailure::NotFound { .. }));
}

/// Scoped addressing: holding another tenant's exact client id must not
/// grant rotation, and the answer is indistinguishable from "never
/// existed" so it cannot be used as a probe.
#[tokio::test]
async fn rotate_cannot_reach_another_tenants_account() {
    let svc = Service::new();
    let (a, b) = (Uuid::from_u128(0x5007), Uuid::from_u128(0x5008));
    let owned_by_a = svc
        .provision_service_account(&ctx(), &sa_provision_req(a, "ci"))
        .await
        .expect("tenant a provision succeeds");

    let err = svc
        .rotate_service_account_secret(
            &ctx(),
            &IdpRotateServiceAccountSecretRequest::new(tenant_ctx(b), owned_by_a.client_id.clone()),
        )
        .await
        .expect_err("cross-tenant rotate is refused");

    assert!(matches!(err, IdpServiceAccountFailure::NotFound { .. }));
}

#[tokio::test]
async fn revoke_removes_the_account_then_reports_absence() {
    let svc = Service::new();
    let tenant = Uuid::from_u128(0x5009);
    let creds = svc
        .provision_service_account(&ctx(), &sa_provision_req(tenant, "ci"))
        .await
        .expect("provision succeeds");
    let first_req =
        IdpRevokeServiceAccountRequest::new(tenant_ctx(tenant), creds.client_id.clone());
    svc.revoke_service_account(&ctx(), &first_req)
        .await
        .expect("first revoke removes it");

    // Second revoke reports absence 1:1 — AM is what turns that into an
    // idempotent 204, so the plugin must NOT fold it into success here.
    let repeat_req =
        IdpRevokeServiceAccountRequest::new(tenant_ctx(tenant), creds.client_id.clone());
    let err = svc
        .revoke_service_account(&ctx(), &repeat_req)
        .await
        .expect_err("repeat revoke reports absence to AM");
    assert!(matches!(err, IdpServiceAccountFailure::NotFound { .. }));

    let listed = svc
        .list_service_accounts(
            &ctx(),
            &IdpListServiceAccountsRequest::new(tenant_ctx(tenant)),
        )
        .await
        .expect("list succeeds");
    assert!(listed.is_empty(), "revoked account must be gone");
}

/// Uniqueness is over *live* accounts, which is what makes
/// revoke-then-provision a valid recovery from an ambiguous outcome.
#[tokio::test]
async fn revoke_frees_the_name_for_a_fresh_provision() {
    use secrecy::ExposeSecret as _;

    let svc = Service::new();
    let tenant = Uuid::from_u128(0x500A);
    let first = svc
        .provision_service_account(&ctx(), &sa_provision_req(tenant, "ci"))
        .await
        .expect("provision succeeds");
    svc.revoke_service_account(
        &ctx(),
        &IdpRevokeServiceAccountRequest::new(tenant_ctx(tenant), first.client_id.clone()),
    )
    .await
    .expect("revoke succeeds");

    let second = svc
        .provision_service_account(&ctx(), &sa_provision_req(tenant, "ci"))
        .await
        .expect("the freed name is available again");

    // Re-provisioning the same name must not hand back the revoked
    // credential. An earlier revision derived the secret from
    // `(client_id, generation)`, both of which are pure functions of
    // `(tenant_id, name)` — so this recovery path reproduced the
    // original secret byte-for-byte and revocation did not revoke.
    assert_eq!(
        second.client_id, first.client_id,
        "identity is still derived from (tenant_id, name)"
    );
    assert_ne!(
        second.client_secret.expose_secret(),
        first.client_secret.expose_secret(),
        "a revoked secret must never be resurrected by re-provisioning the freed name"
    );
}

/// A name that could not survive being embedded in the client id is
/// rejected before anything is stored, rather than minting an account the
/// item routes can never address.
#[tokio::test]
async fn unaddressable_name_is_rejected_and_stores_nothing() {
    let svc = Service::new();
    let tenant = Uuid::from_u128(0x500F);

    for bad in ["a/b", "", "a b", "a%2Fb"] {
        let err = svc
            .provision_service_account(&ctx(), &sa_provision_req(tenant, bad))
            .await
            .expect_err("an unaddressable name must be refused");
        assert!(
            matches!(
                err,
                IdpServiceAccountFailure::InvalidInput { ref field, .. }
                    if field.as_deref() == Some("name")
            ),
            "expected InvalidInput on the name field, got {err:?}"
        );
    }

    let listed = svc
        .list_service_accounts(
            &ctx(),
            &IdpListServiceAccountsRequest::new(tenant_ctx(tenant)),
        )
        .await
        .expect("listing succeeds");
    assert!(
        listed.is_empty(),
        "a rejected name must leave no account behind"
    );
}

/// Two accounts must not share secret material, and a secret must not be
/// derivable from the identity a `list` caller can already see.
#[tokio::test]
async fn secrets_are_unguessable_and_unique_per_account() {
    use secrecy::ExposeSecret as _;

    let svc = Service::new();
    let tenant = Uuid::from_u128(0x500E);
    let a = svc
        .provision_service_account(&ctx(), &sa_provision_req(tenant, "one"))
        .await
        .expect("provision succeeds");
    let b = svc
        .provision_service_account(&ctx(), &sa_provision_req(tenant, "two"))
        .await
        .expect("provision succeeds");

    assert_ne!(
        a.client_secret.expose_secret(),
        b.client_secret.expose_secret()
    );
    // The client id and the subject id are both public (the listing
    // exposes the name, and AM hands the subject id to RBAC). Neither may
    // appear in the secret, or reading the listing would be enough to
    // reconstruct it.
    for cred in [&a, &b] {
        let secret = cred.client_secret.expose_secret();
        assert!(
            !secret.contains(&cred.client_id),
            "secret must not embed the client id"
        );
        assert!(
            !secret.contains(&cred.subject_id.to_string()),
            "secret must not embed the subject id"
        );
    }
}

#[tokio::test]
async fn listing_is_name_ordered_and_carries_no_secret() {
    let svc = Service::new();
    let tenant = Uuid::from_u128(0x500B);
    for name in ["zeta", "alpha", "mid"] {
        svc.provision_service_account(&ctx(), &sa_provision_req(tenant, name))
            .await
            .expect("provision succeeds");
    }

    let listed = svc
        .list_service_accounts(
            &ctx(),
            &IdpListServiceAccountsRequest::new(tenant_ctx(tenant)),
        )
        .await
        .expect("list succeeds");

    // Stable ordering matters: the backing map's iteration order is not
    // deterministic, and a caller reconciling an ambiguous provision
    // scans this listing.
    let names: Vec<&str> = listed.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    // The summary type carries no secret field at all; assert the debug
    // form is clean as a belt-and-braces guard against a future field.
    assert!(!format!("{listed:?}").contains("static-idp-secret"));
}
