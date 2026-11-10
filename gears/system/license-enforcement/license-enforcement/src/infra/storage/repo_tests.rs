use license_enforcement_sdk::models::{Binding, Grant, GrantState, TerminationReason, Validity};
use license_enforcement_sdk::{DelegationId, GrantId};
use time::Duration;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::*;
use crate::test_support::{Env, ctx, env, terms, token};

fn sub(id: Uuid, parent: &Grant, holder: Uuid, delegation: DelegationId) -> Grant {
    let mut g = parent.clone();
    g.id = GrantId(id);
    g.holder_tenant_id = holder;
    g.binding = Binding::None;
    g.validity = Validity::Inherited {
        window: *parent.validity.window(),
    };
    g.parent_grant_id = Some(parent.id);
    g.grantor_tenant_id = Some(parent.holder_tenant_id);
    g.delegation_id = Some(delegation);
    g.pack_id = None;
    g
}

async fn insert(env: &Env, g: &Grant, root: GrantId) {
    crate::test_support::insert_sub_grant(
        env,
        &AccessScope::for_tenant(g.holder_tenant_id),
        g,
        root,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn live_subtree_walks_down_across_tenants_parents_first() {
    let env = env().await;
    let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let top = crate::test_support::issue_one(&env, a, terms()).await;
    // The grandchild gets the smaller id, so one pass in id order meets it
    // before its parent.
    let (s2_id, s1_id) = (Uuid::now_v7(), Uuid::now_v7());
    let d = DelegationId(Uuid::now_v7());
    let s1 = sub(s1_id, &top, b, d);
    let s2 = sub(s2_id, &s1, c, d);
    insert(&env, &s1, top.id).await;
    insert(&env, &s2, top.id).await;
    let mut ended = sub(Uuid::now_v7(), &s2, a, d);
    ended.state = GrantState::Terminated;
    insert(&env, &ended, top.id).await;

    let conn = env.db.conn().unwrap();
    let ids = |v: Vec<Grant>| v.into_iter().map(|g| g.id).collect::<Vec<_>>();
    assert_eq!(
        ids(env.store.live_subtree(&conn, top.id, false).await.unwrap()),
        [top.id, s1.id, s2.id]
    );
    assert_eq!(
        ids(env.store.live_subtree(&conn, s1.id, false).await.unwrap()),
        [s1.id, s2.id]
    );
    assert!(
        env.store
            .live_subtree(&conn, ended.id, false)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_second_live_child_is_an_invalid_transition() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let top = crate::test_support::issue_one(&env, a, terms()).await;
    let d = DelegationId(Uuid::now_v7());
    insert(&env, &sub(Uuid::now_v7(), &top, b, d), top.id).await;

    let err = crate::test_support::insert_sub_grant(
        &env,
        &AccessScope::for_tenant(b),
        &sub(Uuid::now_v7(), &top, b, d),
        top.id,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, DomainError::InvalidTransition(_)), "{err:?}");
}

#[tokio::test]
async fn delegation_grants_are_of_a_party_and_ordered() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let (r1, r2) = (
        crate::test_support::issue_one(&env, a, terms()).await,
        crate::test_support::issue_one(&env, a, terms()).await,
    );
    let d = DelegationId(Uuid::now_v7());
    let (s1, s2) = (
        sub(Uuid::now_v7(), &r1, b, d),
        sub(Uuid::now_v7(), &r2, b, d),
    );
    insert(&env, &s2, r2.id).await;
    insert(&env, &s1, r1.id).await;

    let conn = env.db.conn().unwrap();
    for party in [a, b] {
        let got = env
            .store
            .delegation_grants(&conn, party, d, false)
            .await
            .unwrap();
        assert_eq!(got, (vec![s1.clone(), s2.clone()], 1));
    }
    let outside = env
        .store
        .delegation_grants(&conn, Uuid::now_v7(), d, false)
        .await
        .unwrap();
    assert!(outside.0.is_empty());
}

#[tokio::test]
async fn update_grant_writes_the_verb_fields_once_per_version() {
    let env = env().await;
    let a = Uuid::now_v7();
    let mut g = crate::test_support::issue_one(&env, a, terms()).await;
    let scope = AccessScope::for_tenant(a);
    let conn = env.db.conn().unwrap();

    let mut window = *g.validity.window();
    window.expires_at += Duration::days(10);
    window.grace_until = Some(window.expires_at + Duration::days(1));
    g.validity = Validity::Own { window };
    g.state = GrantState::Terminated;
    g.termination_reason = Some(TerminationReason::Revoked);
    g.version += 1;
    env.store
        .update_grant(&conn, &scope, &g, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(
        env.store.grant(&conn, &scope, g.id, false).await.unwrap(),
        Some(g.clone())
    );

    let err = env
        .store
        .update_grant(&conn, &scope, &g, OffsetDateTime::now_utc())
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionMismatch), "{err:?}");
}

#[tokio::test]
async fn update_pack_version_needs_the_previous_version() {
    let env = env().await;
    let a = Uuid::now_v7();
    let pack = env
        .grants
        .issue(&ctx(a), a, vec![terms()], true, token())
        .await
        .unwrap()
        .pack
        .unwrap();
    let scope = AccessScope::for_tenant(a);
    let conn = env.db.conn().unwrap();

    env.store
        .update_pack_version(&conn, &scope, pack.id, 2)
        .await
        .unwrap();
    assert_eq!(
        env.store
            .pack(&conn, &scope, pack.id)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
    let err = env
        .store
        .update_pack_version(&conn, &scope, pack.id, 2)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionMismatch), "{err:?}");
}

#[tokio::test]
async fn list_packs_filters_by_a_member_resource_type() {
    let env = env().await;
    let a = Uuid::now_v7();
    for _ in 0..2 {
        env.grants
            .issue(&ctx(a), a, vec![terms()], true, token())
            .await
            .unwrap();
    }
    let list = |filter: &str| {
        let q = {
            let mut q = license_enforcement_sdk::dto::ListQuery::default();
            q.filter = Some(filter.to_owned());
            q
        };
        crate::domain::local_client::odata_query(&q).unwrap()
    };
    let all = env
        .grants
        .list_packs(
            &ctx(a),
            &list(&format!(
                "resource_type eq '{}'",
                crate::test_support::RESOURCE
            )),
            false,
        )
        .await
        .unwrap();
    assert_eq!(all.items.len(), 2);
    assert_eq!(all.items[0].counts[0].unbound, 1);
    let none = env
        .grants
        .list_packs(
            &ctx(a),
            &list(&format!("resource_type eq '{}'", crate::test_support::USER)),
            false,
        )
        .await
        .unwrap();
    assert!(none.items.is_empty());
    let other = env
        .grants
        .list_packs(
            &ctx(Uuid::now_v7()),
            &toolkit_odata::ODataQuery::new(),
            false,
        )
        .await
        .unwrap();
    assert!(other.items.is_empty());
}

/// The filter keeps the tenants of the scope: an ancestor finds the packs of
/// its subtree, and not those behind a barrier.
#[tokio::test]
async fn list_packs_filters_in_the_subtree() {
    let env = env().await;
    let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    env.tenants.link(a, c);
    env.tenants.self_managed(c);
    for t in [b, c] {
        env.grants
            .issue(&ctx(t), t, vec![terms()], true, token())
            .await
            .unwrap();
    }
    let q = {
        let mut q = license_enforcement_sdk::dto::ListQuery::default();
        q.filter = Some(format!(
            "resource_type eq '{}'",
            crate::test_support::RESOURCE
        ));
        q
    };
    let q = crate::domain::local_client::odata_query(&q).unwrap();
    let found = env.grants.list_packs(&ctx(a), &q, true).await.unwrap();
    assert_eq!(found.items.len(), 1);
    assert_eq!(found.items[0].holder_tenant_id, b);
}

#[tokio::test]
async fn sweep_deletes_ended_trees_and_their_empty_packs() {
    use crate::domain::grants::Verb;
    let env = env().await;
    let a = Uuid::now_v7();
    let ended = env
        .grants
        .issue(&ctx(a), a, vec![terms()], true, token())
        .await
        .unwrap();
    let live = crate::test_support::issue_one(&env, a, terms()).await;
    env.grants
        .pack_verb(
            &ctx(a),
            ended.pack.as_ref().unwrap().id,
            Verb::Revoke,
            token(),
            None,
        )
        .await
        .unwrap();

    // Not old enough yet.
    assert_eq!(
        crate::test_support::sweep(&env, Duration::days(1)).await.0,
        0
    );
    let (grants, _) = crate::test_support::sweep(&env, Duration::ZERO).await;
    assert_eq!(grants, 1);
    let conn = env.db.conn().unwrap();
    let scope = AccessScope::for_tenant(a);
    assert!(
        env.store
            .grant(&conn, &scope, ended.grants[0].id, false)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .pack(&conn, &scope, ended.pack.unwrap().id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .grant(&conn, &scope, live.id, false)
            .await
            .unwrap()
            .is_some()
    );

    let later = OffsetDateTime::now_utc() + Duration::days(2);
    let first = env.store.purge_idempotency(&conn, later, 1).await.unwrap();
    assert_eq!(first, 1);
    let rest = env
        .store
        .purge_idempotency(&conn, later, 100)
        .await
        .unwrap();
    assert!(rest >= 2, "{rest}");
    let none = env
        .store
        .purge_idempotency(&conn, later, 100)
        .await
        .unwrap();
    assert_eq!(none, 0);
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn sweep_skips_while_another_replica_holds_the_lock() {
    use crate::domain::grants::SWEEP_LOCK;
    let env = env().await;
    let held = env.db.db().lock(SWEEP_LOCK.0, SWEEP_LOCK.1).await.unwrap();
    assert!(env.grants.sweep(Duration::ZERO).await.unwrap().is_none());
    held.release().await.unwrap();
    assert!(env.grants.sweep(Duration::ZERO).await.unwrap().is_some());
}

/// A live row under a terminated root breaks the rule the sweep relies on:
/// the sweep keeps that tree.
#[tokio::test]
async fn sweep_keeps_a_tree_with_a_live_row_under_a_terminated_root() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let mut top = crate::test_support::issue_one(&env, a, terms()).await;
    let s1 = sub(Uuid::now_v7(), &top, b, DelegationId(Uuid::now_v7()));
    insert(&env, &s1, top.id).await;
    top.state = GrantState::Terminated;
    top.termination_reason = Some(TerminationReason::Revoked);
    top.version += 1;
    let conn = env.db.conn().unwrap();
    env.store
        .update_grant(
            &conn,
            &AccessScope::for_tenant(a),
            &top,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    let (grants, _) = crate::test_support::sweep(&env, Duration::ZERO).await;
    assert_eq!(grants, 0);
    assert!(
        env.store
            .grant(&conn, &AccessScope::allow_all(), s1.id, false)
            .await
            .unwrap()
            .is_some()
    );

    // The kept tree does not hold a batch: a batch of one moves past it.
    let mut next = crate::test_support::issue_one(&env, a, terms()).await;
    next.state = GrantState::Terminated;
    next.termination_reason = Some(TerminationReason::Revoked);
    next.version += 1;
    env.store
        .update_grant(
            &conn,
            &AccessScope::for_tenant(a),
            &next,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    let swept = env
        .store
        .sweep_terminated(&conn, OffsetDateTime::now_utc(), 1)
        .await
        .unwrap();
    assert_eq!(swept, 1);
}

#[tokio::test]
async fn sweep_deletes_an_ended_branch_under_a_live_root() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let top = crate::test_support::issue_one(&env, a, terms()).await;
    let mut s1 = sub(Uuid::now_v7(), &top, b, DelegationId(Uuid::now_v7()));
    insert(&env, &s1, top.id).await;
    s1.state = GrantState::Terminated;
    s1.termination_reason = Some(TerminationReason::Returned);
    s1.version += 1;
    let conn = env.db.conn().unwrap();
    env.store
        .update_grant(
            &conn,
            &AccessScope::for_tenant(b),
            &s1,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    let (grants, _) = crate::test_support::sweep(&env, Duration::ZERO).await;
    assert_eq!(grants, 1);
    let read = |id| {
        let (env, conn) = (&env, &conn);
        async move {
            env.store
                .grant(conn, &AccessScope::allow_all(), id, false)
                .await
                .unwrap()
        }
    };
    assert!(read(s1.id).await.is_none());
    assert!(read(top.id).await.is_some());
}

#[tokio::test]
async fn list_packs_pages_by_cursor() {
    let env = env().await;
    let a = Uuid::now_v7();
    for _ in 0..3 {
        env.grants
            .issue(&ctx(a), a, vec![terms()], true, token())
            .await
            .unwrap();
    }
    let page = |cursor: Option<String>| {
        crate::domain::local_client::odata_query(&{
            let mut q = license_enforcement_sdk::dto::ListQuery::default();
            q.limit = Some(2);
            q.cursor = cursor;
            q
        })
        .unwrap()
    };
    let first = env
        .grants
        .list_packs(&ctx(a), &page(None), false)
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let second = env
        .grants
        .list_packs(&ctx(a), &page(first.page_info.next_cursor.clone()), false)
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.page_info.next_cursor.is_none());
    let ids: std::collections::BTreeSet<_> = first
        .items
        .iter()
        .chain(&second.items)
        .map(|p| p.id)
        .collect();
    assert_eq!(ids.len(), 3);
}

#[tokio::test]
async fn insert_evidence_names_the_reused_index() {
    use license_enforcement_sdk::error::precondition;
    use license_enforcement_sdk::{EvidenceId, Intake, RejectionReason};

    use crate::domain::repo::NewEvidenceRow;
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let conn = env.db.conn().unwrap();
    let tag = Uuid::now_v7().to_string();
    let row = |holder: Uuid, n: &str, replaces: Option<EvidenceId>| NewEvidenceRow {
        id: EvidenceId(Uuid::now_v7()),
        holder,
        plugin_id: "test",
        intake: Intake::Issue,
        bytes: Vec::new(),
        replay_key: Some(format!("{tag}-{n}")),
        external_id: Some(format!("{tag}-{n}")),
        replaces,
    };
    let insert = |r: NewEvidenceRow| {
        let scope = AccessScope::for_tenant(r.holder);
        let (store, conn) = (env.store, &conn);
        async move {
            store
                .insert_evidence(conn, &scope, r, time::OffsetDateTime::now_utc())
                .await
        }
    };
    let first = row(a, "1", None);
    let (first_id, first_key) = (first.id, first.replay_key.clone());
    insert(first).await.unwrap();

    let mut same_key = row(a, "2", None);
    same_key.replay_key.clone_from(&first_key);
    let err = insert(same_key).await.unwrap_err();
    assert!(
        matches!(&err, DomainError::InvalidArgument(v)
            if v.iter().any(|v| v.reason == license_enforcement_sdk::error::reason::TOKEN_REUSED)),
        "{err:?}"
    );

    let mut same_external = row(b, "3", None);
    same_external.external_id.clone_from(&first_key);
    let err = insert(same_external).await.unwrap_err();
    assert!(
        matches!(err, DomainError::EvidenceRejected(RejectionReason::Replay)),
        "{err:?}"
    );

    insert(row(a, "4", Some(first_id))).await.unwrap();
    let err = insert(row(a, "5", Some(first_id))).await.unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::EvidenceChainBroken(precondition::EVIDENCE_ALREADY_REPLACED)
        ),
        "{err:?}"
    );

    let mut same_id = row(a, "6", None);
    same_id.id = first_id;
    let err = insert(same_id).await.unwrap_err();
    assert!(matches!(err, DomainError::AdmissionConflict), "{err:?}");
}

/// A terminated branch under a live root goes leaf first: no row outlives
/// its parent, whatever the batch.
#[tokio::test]
async fn a_terminated_branch_goes_leaf_first() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let top = crate::test_support::issue_one(&env, a, terms()).await;
    let mut chain = vec![top.clone()];
    for _ in 0..3 {
        let mut g = sub(
            Uuid::now_v7(),
            chain.last().unwrap(),
            b,
            DelegationId(Uuid::now_v7()),
        );
        g.state = GrantState::Terminated;
        g.termination_reason = Some(TerminationReason::Recalled);
        insert(&env, &g, top.id).await;
        chain.push(g);
    }
    let conn = env.db.conn().unwrap();
    let cutoff = OffsetDateTime::now_utc() + Duration::seconds(1);
    let exists = |id| {
        let conn = &conn;
        let store = env.store;
        async move {
            store
                .grant(conn, &AccessScope::allow_all(), id, false)
                .await
                .unwrap()
                .is_some()
        }
    };
    for left in (1..=3).rev() {
        assert_eq!(
            env.store.sweep_terminated(&conn, cutoff, 1).await.unwrap(),
            1
        );
        // The first `left` rows of the chain stay: the root and its nearest subs.
        for (i, g) in chain.iter().enumerate() {
            assert_eq!(exists(g.id).await, i < left, "row {i} after a batch");
        }
    }
    assert_eq!(
        env.store.sweep_terminated(&conn, cutoff, 1).await.unwrap(),
        0
    );
}

/// A binding row without the id its kind requires is a broken row, not a
/// binding to the nil id.
#[tokio::test]
async fn a_delegated_binding_without_its_grant_is_internal() {
    use crate::infra::storage::entity::grant::Column;
    let env = env().await;
    let a = Uuid::now_v7();
    let g = crate::test_support::issue_one(&env, a, terms()).await;
    crate::test_support::edit_row(&env, g.id, Column::BindingKind, "delegated").await;
    let err = env.grants.get_grant(&ctx(a), g.id).await.unwrap_err();
    assert!(
        matches!(err, crate::domain::error::DomainError::Internal(_)),
        "{err:?}"
    );
}
