use crate::test_support::{by_ids, transition};
use license_enforcement_sdk::DenialReason;
use license_enforcement_sdk::models::{
    Binding, Conditions, DelegatedPermissions, GrantState, TerminationReason, Validity, WindowInput,
};
use time::Duration;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::grants::{BindingChange, Verb};
use crate::domain::repo::GrantRepo;
use crate::test_support::{
    Env, Sides, USER, ctx, delegable, env, issue_one, now, request, token, user,
};

/// A → B → C.
fn tree(env: &Env) -> (Uuid, Uuid, Uuid) {
    let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    env.tenants.link(b, c);
    (a, b, c)
}

#[tokio::test]
async fn delegate_bind_and_check_down_a_chain() {
    let env = env().await;
    let (a, b, c) = tree(&env);
    let root = issue_one(&env, a, delegable(2)).await;

    let d = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap();
    assert_eq!((d.grantor_tenant_id, d.recipient_tenant_id), (a, b));
    // The grantor gets its own grant, which names the recipient tenant.
    assert_eq!(d.given().len(), 1);
    assert_eq!(d.given()[0].id, root.id);
    let subs = crate::test_support::sub_grants(&env, &d).await;
    let s1 = &subs[0];
    assert_eq!(s1.permissions.max_depth, 1);
    assert!(matches!(s1.validity, Validity::Inherited { .. }));
    let root_now = env.grants.get_grant(&ctx(a), root.id).await.unwrap();
    assert_eq!(root_now.binding, Binding::delegated(s1.id, b));
    assert_eq!(d.given()[0], root_now);

    let d2 = env
        .grants
        .delegate(&ctx(b), by_ids(c, vec![s1.id]))
        .await
        .unwrap();
    let subs2 = crate::test_support::sub_grants(&env, &d2).await;
    let s2 = &subs2[0];
    assert_eq!(s2.permissions.max_depth, 0);
    env.grants
        .change_binding(
            &ctx(c),
            s2.id,
            BindingChange::Bind(user("carol")),
            token(),
            None,
        )
        .await
        .unwrap();
    assert!(
        env.check
            .check(&request(USER, "carol", c), now())
            .await
            .unwrap()
            .granted
    );

    // Depth is spent.
    let d_tenant = Uuid::now_v7();
    env.tenants.link(c, d_tenant);
    transition(
        &env.grants
            .delegate(&ctx(c), by_ids(d_tenant, vec![s2.id]))
            .await
            .unwrap_err(),
    );
    // A delegated grant is not delegated again.
    transition(
        &env.grants
            .delegate(&ctx(a), by_ids(b, vec![root.id]))
            .await
            .unwrap_err(),
    );

    // The vendor suspends the root: the end of the chain denies.
    env.grants
        .grant_verb(&ctx(a), root.id, Verb::Suspend, token(), None)
        .await
        .unwrap();
    let denied = env
        .check
        .check(&request(USER, "carol", c), now())
        .await
        .unwrap();
    assert_eq!(denied.reason, Some(DenialReason::Suspended));
}

#[tokio::test]
async fn recall_ends_the_subtree_and_unbinds_the_end() {
    let env = env().await;
    let (a, b, c) = tree(&env);
    let root = issue_one(&env, a, delegable(2)).await;
    let d = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap();
    let s1 = d.sub(0);
    let s2 = env
        .grants
        .delegate(&ctx(b), by_ids(c, vec![s1]))
        .await
        .unwrap()
        .sub(0);
    env.grants
        .change_binding(
            &ctx(c),
            s2,
            BindingChange::Bind(user("carol")),
            token(),
            None,
        )
        .await
        .unwrap();

    let recalled = env
        .grants
        .delegation_verb(&ctx(a), d.id, Verb::Recall, token(), None)
        .await
        .unwrap();
    assert_eq!(recalled.given()[0].binding, Binding::None);
    assert_eq!(
        crate::test_support::sub_grants(&env, &d).await[0].state,
        GrantState::Terminated
    );
    let conn = env.db.conn().unwrap();
    let end = env
        .store
        .grant(&conn, &AccessScope::allow_all(), s2, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end.termination_reason, Some(TerminationReason::Recalled));
    assert_eq!(end.binding, Binding::None);
    let root_now = env.grants.get_grant(&ctx(a), root.id).await.unwrap();
    assert_eq!(root_now.binding, Binding::None);
    // The check names the end to the subject that the recall unbound, and to
    // no other subject.
    let carol = env
        .check
        .check(&request(USER, "carol", c), now())
        .await
        .unwrap();
    assert!(!carol.granted);
    assert_eq!(carol.reason, Some(DenialReason::Terminated), "{carol:?}");
    assert_eq!(carol.termination_reason, Some(TerminationReason::Recalled));
    let bob = env
        .check
        .check(&request(USER, "bob", c), now())
        .await
        .unwrap();
    assert_eq!(
        bob.reason,
        Some(DenialReason::NoGrantForResource),
        "{bob:?}"
    );
    // The root is free to delegate again.
    env.grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap();
}

#[tokio::test]
async fn return_needs_an_unbound_sub_grant() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let root = issue_one(&env, a, delegable(1)).await;
    let s = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);
    env.grants
        .change_binding(&ctx(b), s, BindingChange::Bind(user("bob")), token(), None)
        .await
        .unwrap();
    transition(
        &env.grants
            .grant_verb(&ctx(b), s, Verb::Return, token(), None)
            .await
            .unwrap_err(),
    );
    env.grants
        .change_binding(&ctx(b), s, BindingChange::Unbind, token(), None)
        .await
        .unwrap();
    let returned = env
        .grants
        .grant_verb(&ctx(b), s, Verb::Return, token(), None)
        .await
        .unwrap();
    assert_eq!(
        returned.termination_reason,
        Some(TerminationReason::Returned)
    );
    let root_now = env.grants.get_grant(&ctx(a), root.id).await.unwrap();
    assert_eq!(root_now.binding, Binding::None);
}

#[tokio::test]
async fn a_delegation_that_awaits_acceptance_is_accepted_as_one() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let r1 = issue_one(&env, a, delegable(1)).await;
    let r2 = issue_one(&env, a, delegable(1)).await;
    let mut req = by_ids(b, vec![r1.id, r2.id]);
    req.permissions = Some(DelegatedPermissions::new(true).requiring_acceptance());
    let d = env.grants.delegate(&ctx(a), req).await.unwrap();
    assert!(
        crate::test_support::sub_grants(&env, &d)
            .await
            .iter()
            .all(|g| g.state == GrantState::AwaitingAcceptance)
    );
    let accepted = env
        .grants
        .delegation_verb(&ctx(b), d.id, Verb::Accept, token(), None)
        .await
        .unwrap();
    assert!(
        accepted
            .received()
            .iter()
            .all(|g| g.state == GrantState::Active)
    );
    assert_eq!(
        env.grants.get_delegation(&ctx(b), d.id).await.unwrap(),
        accepted
    );
    // The grantor reads its own grants in it; another tenant reads nothing.
    let own = env.grants.get_delegation(&ctx(a), d.id).await.unwrap();
    assert_eq!(
        own.given().iter().map(|g| g.id).collect::<Vec<_>>(),
        vec![r1.id, r2.id]
    );
    assert!(own.given().iter().all(|g| matches!(
        g.binding,
        Binding::Delegated { tenant_id, .. } if tenant_id == b
    )));
    assert!(matches!(
        env.grants
            .get_delegation(&ctx(Uuid::now_v7()), d.id)
            .await
            .unwrap_err(),
        DomainError::NotFound { .. }
    ));
}

fn not_found(e: &DomainError) {
    assert!(matches!(e, DomainError::NotFound { .. }), "{e:?}");
}

/// The holder acts in its own tenant: an ancestor whose scope covers the
/// holder's tenant does not take the holder's verbs.
#[tokio::test]
async fn an_ancestor_does_not_take_the_verbs_of_the_holder() {
    // The policy does not give a grantor `read_subtree`.
    let pdp = std::sync::Arc::new(crate::test_support::RuleAuthZ::default());
    pdp.set("license_enforcement.grant", "read_subtree", None);
    let env = crate::test_support::env_with(pdp).await;
    let (a, b, _) = tree(&env);
    let root = issue_one(&env, a, delegable(1)).await;
    let mut req = by_ids(b, vec![root.id]);
    req.permissions = Some(DelegatedPermissions::new(true).requiring_acceptance());
    let d = env.grants.delegate(&ctx(a), req).await.unwrap();
    let s1 = d.sub(0);

    // A neither reads the grant of its child tenant B, nor accepts, binds, or
    // returns it.
    not_found(&env.grants.get_grant(&ctx(a), s1).await.unwrap_err());
    assert!(
        env.grants
            .list_grants(&ctx(a), &toolkit_odata::ODataQuery::new(), false)
            .await
            .unwrap()
            .items
            .iter()
            .all(|g| g.id != s1)
    );
    assert!(matches!(
        env.grants
            .list_grants(&ctx(a), &toolkit_odata::ODataQuery::new(), true)
            .await,
        Err(DomainError::PermissionDenied)
    ));
    not_found(
        &env.grants
            .grant_verb(&ctx(a), s1, Verb::Accept, token(), None)
            .await
            .unwrap_err(),
    );
    not_found(
        &env.grants
            .delegation_verb(&ctx(a), d.id, Verb::Accept, token(), None)
            .await
            .unwrap_err(),
    );
    env.grants
        .grant_verb(&ctx(b), s1, Verb::Accept, token(), None)
        .await
        .unwrap();
    not_found(
        &env.grants
            .change_binding(&ctx(a), s1, BindingChange::Bind(user("bob")), token(), None)
            .await
            .unwrap_err(),
    );
    not_found(
        &env.grants
            .grant_verb(&ctx(a), s1, Verb::Return, token(), None)
            .await
            .unwrap_err(),
    );
}

/// Recall is the verb of the grantor alone: the root holder A does not recall
/// the sub-grant that B delegated to C.
#[tokio::test]
async fn only_the_grantor_recalls() {
    let env = env().await;
    let (a, b, c) = tree(&env);
    let root = issue_one(&env, a, delegable(2)).await;
    let s1 = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);
    let d2 = env
        .grants
        .delegate(&ctx(b), by_ids(c, vec![s1]))
        .await
        .unwrap();
    let s2 = d2.sub(0);

    not_found(
        &env.grants
            .grant_verb(&ctx(a), s2, Verb::Recall, token(), None)
            .await
            .unwrap_err(),
    );
    not_found(
        &env.grants
            .delegation_verb(&ctx(a), d2.id, Verb::Recall, token(), None)
            .await
            .unwrap_err(),
    );
    let recalled = env
        .grants
        .grant_verb(&ctx(b), s2, Verb::Recall, token(), None)
        .await
        .unwrap();
    // The grantor gets its own grant, free again.
    assert_eq!((recalled.id, recalled.binding), (s1, Binding::None));
    assert_eq!(
        env.store
            .grant(
                &env.db.conn().unwrap(),
                &AccessScope::allow_all(),
                s2,
                false
            )
            .await
            .unwrap()
            .unwrap()
            .state,
        GrantState::Terminated
    );
}

/// A barrier blocks a delegation into the self-managed tenant. The grantor
/// acts in its own tenant, so a barrier raised later does not stop a recall.
#[tokio::test]
async fn the_grantor_recalls_from_a_self_managed_tenant() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let root = issue_one(&env, a, delegable(1)).await;
    let s1 = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);
    env.tenants.self_managed(b);

    let other = issue_one(&env, a, delegable(1)).await;
    let err = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![other.id]))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::InvalidTransition(_)), "{err:?}");
    let recalled = env
        .grants
        .grant_verb(&ctx(a), s1, Verb::Recall, token(), None)
        .await
        .unwrap();
    assert_eq!((recalled.id, recalled.binding), (root.id, Binding::None));
    assert_eq!(
        env.store
            .grant(
                &env.db.conn().unwrap(),
                &AccessScope::allow_all(),
                s1,
                false
            )
            .await
            .unwrap()
            .unwrap()
            .state,
        GrantState::Terminated
    );
}

#[tokio::test]
async fn delegate_refuses_what_is_not_a_subset_or_not_below() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let root = issue_one(&env, a, delegable(1)).await;
    let stranger = Uuid::now_v7();
    transition(
        &env.grants
            .delegate(&ctx(a), by_ids(stranger, vec![root.id]))
            .await
            .unwrap_err(),
    );

    let reasons = |e: DomainError| match e {
        DomainError::InvalidArgument(v) => v.into_iter().map(|v| v.reason).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let mut req = by_ids(b, vec![root.id]);
    req.conditions = Some(Conditions(serde_json::json!({ "region": "eu" })));
    let err = env.grants.delegate(&ctx(a), req).await.unwrap_err();
    assert!(matches!(err, DomainError::NotImplemented(_)), "{err:?}");
    // I4 holds for every permission: neither bind nor a dropped acceptance.
    let mut t = delegable(1);
    t.permissions.may_bind = false;
    let narrow = issue_one(&env, a, t).await;
    let mut req = by_ids(b, vec![narrow.id]);
    req.permissions = Some(DelegatedPermissions::new(true));
    assert_eq!(
        reasons(env.grants.delegate(&ctx(a), req).await.unwrap_err()),
        ["NOT_A_SUBSET"]
    );
    let mut t = delegable(1);
    t.permissions.requires_acceptance = true;
    let gated = issue_one(&env, a, t).await;
    env.grants
        .grant_verb(&ctx(a), gated.id, Verb::Accept, token(), None)
        .await
        .unwrap();
    let mut req = by_ids(b, vec![gated.id]);
    req.permissions = Some(DelegatedPermissions::new(true));
    assert_eq!(
        reasons(env.grants.delegate(&ctx(a), req).await.unwrap_err()),
        ["NOT_A_SUBSET"]
    );

    // Another holder's grant is not visible.
    let other = issue_one(&env, b, delegable(1)).await;
    assert!(matches!(
        env.grants
            .delegate(&ctx(a), by_ids(b, vec![other.id]))
            .await
            .unwrap_err(),
        DomainError::NotFound { .. }
    ));
}

/// A sub-grant has the window of its parent: renew addresses the root, and
/// the grantor cannot renew the sub-grant on its own.
#[tokio::test]
async fn a_sub_grant_is_renewed_with_its_root() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let t = delegable(1);
    let root = issue_one(&env, a, t.clone()).await;
    let s1 = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);
    let longer = WindowInput {
        expires_at: t.window.expires_at + Duration::days(5),
        ..t.window
    };
    let ca = ctx(a);
    let renew = |id| env.grants.renew_grant(&ca, id, longer, token(), None);
    crate::test_support::transition(&renew(s1).await.unwrap_err());
    renew(root.id).await.unwrap();
    let sub = env
        .store
        .grant(
            &env.db.conn().unwrap(),
            &AccessScope::allow_all(),
            s1,
            false,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sub.validity.window().expires_at, longer.expires_at);
}

/// A verb on a tree waits for the lock of its root, so it sees what the
/// operation holding the lock wrote.
#[cfg(feature = "integration")]
#[tokio::test]
async fn delegate_waits_for_the_lock_of_the_root() {
    let env = env().await;
    let (a, b, c) = tree(&env);
    let root = issue_one(&env, a, delegable(2)).await;
    let s1 = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);

    let (release, held) = tokio::sync::oneshot::channel::<()>();
    let (locked, is_locked) = tokio::sync::oneshot::channel::<()>();
    let (db, store) = (env.db.clone(), env.store);
    let holder = tokio::spawn(async move {
        db.transaction(move |tx| {
            Box::pin(async move {
                store.lock_roots(tx, &[s1]).await?;
                locked.send(()).unwrap();
                held.await.unwrap();
                Ok(())
            })
        })
        .await
    });
    is_locked.await.unwrap();

    let grants = env.grants.clone();
    let delegate = tokio::spawn(async move { grants.delegate(&ctx(b), by_ids(c, vec![s1])).await });
    crate::test_support::until_a_lock_waits(&env).await;
    assert!(!delegate.is_finished());

    release.send(()).unwrap();
    holder.await.unwrap().unwrap();
    delegate.await.unwrap().unwrap();
}

/// A replacement that moves a tree under a root with a smaller id while a
/// writer waits for the old root ends the writer with `TreeMoved`: the writer
/// reads the roots again after the lock and cannot lock out of id order.
#[cfg(feature = "integration")]
#[tokio::test]
async fn a_tree_moved_below_a_locked_root_ends_the_writer() {
    use crate::infra::storage::entity::grant;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr};
    use toolkit_db::secure::SecureUpdateExt as _;
    let env = env().await;
    let (a, b, _) = tree(&env);
    let lower = issue_one(&env, a, delegable(1)).await.id;
    let root = issue_one(&env, a, delegable(1)).await.id;
    assert!(lower.0 < root.0);
    let sub = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root]))
        .await
        .unwrap()
        .sub(0);

    let (release, held) = tokio::sync::oneshot::channel::<()>();
    let (locked, is_locked) = tokio::sync::oneshot::channel::<()>();
    let (db, store) = (env.db.clone(), env.store);
    let mover = tokio::spawn(async move {
        db.transaction(move |tx| {
            Box::pin(async move {
                store.lock_roots(tx, &[root]).await?;
                grant::Entity::update_many()
                    .col_expr(grant::Column::RootGrantId, Expr::value(lower.0))
                    .filter(grant::Column::Id.eq(sub.0))
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .exec(tx)
                    .await
                    .unwrap();
                locked.send(()).unwrap();
                held.await.unwrap();
                Ok(())
            })
        })
        .await
    });
    is_locked.await.unwrap();

    let (db, store) = (env.db.clone(), env.store);
    let writer = tokio::spawn(async move {
        db.transaction(move |tx| Box::pin(async move { store.lock_roots(tx, &[sub]).await }))
            .await
    });
    crate::test_support::until_a_lock_waits(&env).await;
    assert!(!writer.is_finished());

    release.send(()).unwrap();
    mover.await.unwrap().unwrap();
    let err = writer.await.unwrap().unwrap_err();
    assert!(matches!(err, DomainError::TreeMoved), "{err:?}");
}

/// The other side of a delegation gets `NotFound` without waiting for the
/// lock of the trees: it does not hold the verbs of the grantor up.
#[cfg(feature = "integration")]
#[tokio::test]
async fn the_other_side_is_refused_before_the_lock() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let root = issue_one(&env, a, delegable(1)).await;
    let d = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap();

    let (release, held) = tokio::sync::oneshot::channel::<()>();
    let (locked, is_locked) = tokio::sync::oneshot::channel::<()>();
    let (db, store) = (env.db.clone(), env.store);
    let holder = tokio::spawn(async move {
        db.transaction(move |tx| {
            Box::pin(async move {
                store.lock_roots(tx, &[root.id]).await?;
                locked.send(()).unwrap();
                held.await.unwrap();
                Ok(())
            })
        })
        .await
    });
    is_locked.await.unwrap();

    let recipient = ctx(b);
    let recall = env
        .grants
        .delegation_verb(&recipient, d.id, Verb::Recall, token(), None);
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), recall)
        .await
        .expect("the recipient waited for the lock of the grantor")
        .unwrap_err();
    not_found(&err);

    release.send(()).unwrap();
    holder.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_delegation_is_rejected_or_recalled_as_one() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let roots = [
        issue_one(&env, a, delegable(1)).await.id,
        issue_one(&env, a, delegable(1)).await.id,
    ];
    let mut req = by_ids(b, roots.to_vec());
    req.permissions = Some(DelegatedPermissions::new(true).requiring_acceptance());
    let d = env.grants.delegate(&ctx(a), req).await.unwrap();
    let rejected = env
        .grants
        .delegation_verb(&ctx(b), d.id, Verb::Reject, token(), None)
        .await
        .unwrap();
    assert!(
        rejected
            .received()
            .iter()
            .all(|g| g.termination_reason == Some(TerminationReason::Rejected))
    );
    // The roots are free again.
    let d = env
        .grants
        .delegate(&ctx(a), by_ids(b, roots.to_vec()))
        .await
        .unwrap();
    let recalled = env
        .grants
        .delegation_verb(&ctx(a), d.id, Verb::Recall, token(), None)
        .await
        .unwrap();
    // The grantor gets its own grants back, free again.
    assert_eq!(
        recalled.given().iter().map(|g| g.id).collect::<Vec<_>>(),
        roots.to_vec()
    );
    assert!(
        crate::test_support::sub_grants(&env, &d)
            .await
            .iter()
            .all(|g| g.termination_reason == Some(TerminationReason::Recalled))
    );
    for id in roots {
        assert_eq!(
            env.grants.get_grant(&ctx(a), id).await.unwrap().binding,
            Binding::None
        );
    }
}

#[tokio::test]
async fn recall_in_the_middle_of_a_deep_chain() {
    let env = env().await;
    let (a, b, c) = tree(&env);
    let d_tenant = Uuid::now_v7();
    env.tenants.link(c, d_tenant);
    let root = issue_one(&env, a, delegable(3)).await;
    let s1 = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);
    let s2 = env
        .grants
        .delegate(&ctx(b), by_ids(c, vec![s1]))
        .await
        .unwrap()
        .sub(0);
    let s3 = env
        .grants
        .delegate(&ctx(c), by_ids(d_tenant, vec![s2]))
        .await
        .unwrap()
        .sub(0);
    env.grants
        .change_binding(
            &ctx(d_tenant),
            s3,
            BindingChange::Bind(user("dan")),
            token(),
            None,
        )
        .await
        .unwrap();
    assert!(
        env.check
            .check(&request(USER, "dan", d_tenant), now())
            .await
            .unwrap()
            .granted
    );

    env.grants
        .grant_verb(&ctx(b), s2, Verb::Recall, token(), None)
        .await
        .unwrap();
    let end = env.grants.get_grant(&ctx(d_tenant), s3).await.unwrap();
    assert_eq!(end.state, GrantState::Terminated);
    assert_eq!(end.binding, Binding::None);
    assert_eq!(
        env.grants.get_grant(&ctx(b), s1).await.unwrap().binding,
        Binding::None
    );
    assert!(
        !env.check
            .check(&request(USER, "dan", d_tenant), now())
            .await
            .unwrap()
            .granted
    );
}

#[tokio::test]
async fn delegate_refuses_a_grant_with_recipient_rules() {
    use crate::infra::storage::entity::grant::Column;
    let env = env().await;
    let (a, b, _) = tree(&env);
    let ruled = issue_one(&env, a, delegable(1)).await.id;
    crate::test_support::edit_row(
        &env,
        ruled,
        Column::RecipientRules,
        serde_json::json!({"op": "eq"}),
    )
    .await;
    let err = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![ruled]))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotImplemented(_)), "{err:?}");
}

/// The administrator finds the grants to delegate by the kind of binding.
#[tokio::test]
async fn list_filters_by_binding_kind() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let free = issue_one(&env, a, delegable(1)).await.id;
    let bound = issue_one(&env, a, delegable(1)).await.id;
    let given = issue_one(&env, a, delegable(1)).await.id;
    env.grants
        .change_binding(
            &ctx(a),
            bound,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let d = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![given]))
        .await
        .unwrap();
    let ids = |raw: &str| {
        let q = toolkit_odata::ODataQuery::new()
            .with_filter(toolkit_odata::parse_filter_string(raw).unwrap().into_expr());
        let env = &env;
        async move {
            env.grants
                .list_grants(&ctx(a), &q, false)
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|g| g.id)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(ids("binding_kind eq 'subject'").await, vec![bound]);
    assert_eq!(ids("binding_kind eq 'delegated'").await, vec![given]);
    // What went to B: the grantor's own grant that names B.
    assert_eq!(ids(&format!("binding_tenant_id eq {b}")).await, vec![given]);
    // The free sub-grant of B is not the grantor's to read.
    assert_eq!(ids("binding_kind eq 'none'").await, vec![free]);
    assert!(
        !ids(&format!("holder_tenant_id eq {b}"))
            .await
            .contains(&d.sub(0))
    );
}

#[tokio::test]
async fn a_delegation_verb_checks_the_version() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let r1 = issue_one(&env, a, delegable(1)).await;
    let r2 = issue_one(&env, a, delegable(1)).await;
    let d = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![r1.id, r2.id]))
        .await
        .unwrap();
    assert_eq!(d.version, 1);
    // A bind of the recipient is not a change of the delegation.
    env.grants
        .change_binding(
            &ctx(b),
            d.sub(0),
            BindingChange::Bind(user("bob")),
            token(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        env.grants
            .get_delegation(&ctx(a), d.id)
            .await
            .unwrap()
            .version,
        1
    );
    let err = env
        .grants
        .delegation_verb(&ctx(a), d.id, Verb::Recall, token(), Some(2))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionMismatch), "{err:?}");
    let recalled = env
        .grants
        .delegation_verb(&ctx(a), d.id, Verb::Recall, token(), Some(1))
        .await
        .unwrap();
    assert_eq!(recalled.version, 2);
}

fn refused(e: &DomainError, why: &str) {
    assert!(
        matches!(e, DomainError::InvalidTransition(m) if m.contains(why)),
        "{e:?}"
    );
}

#[tokio::test]
async fn delegate_refuses_a_grant_that_cannot_be_delegated_now() {
    let env = env().await;
    let (a, b, c) = tree(&env);
    let delegate = |grantor, recipient, id| {
        let env = &env;
        async move {
            env.grants
                .delegate(&ctx(grantor), by_ids(recipient, vec![id]))
                .await
        }
    };

    let own = issue_one(&env, a, delegable(1)).await;
    refused(
        &delegate(a, a, own.id).await.unwrap_err(),
        "not a descendant",
    );

    let bound = issue_one(&env, a, delegable(1)).await;
    env.grants
        .change_binding(
            &ctx(a),
            bound.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    refused(&delegate(a, b, bound.id).await.unwrap_err(), "is bound");

    let mut t = delegable(1);
    t.fixed_subject = Some(crate::test_support::input(user("alice")));
    let fixed = issue_one(&env, a, t).await;
    refused(
        &delegate(a, b, fixed.id).await.unwrap_err(),
        "fixed at admission",
    );

    let suspended = issue_one(&env, a, delegable(2)).await;
    let s1 = delegate(a, b, suspended.id).await.unwrap().sub(0);
    env.grants
        .grant_verb(&ctx(a), suspended.id, Verb::Suspend, token(), None)
        .await
        .unwrap();
    refused(&delegate(b, c, s1).await.unwrap_err(), "root is not active");
    let other = issue_one(&env, a, delegable(1)).await;
    env.grants
        .grant_verb(&ctx(a), other.id, Verb::Suspend, token(), None)
        .await
        .unwrap();
    refused(
        &delegate(a, b, other.id).await.unwrap_err(),
        "is not active",
    );
}

/// The recipient's subject is granted only after the acceptance, and only in
/// the recipient's context.
#[tokio::test]
async fn a_check_grants_after_the_acceptance_in_the_recipient_context() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let root = issue_one(&env, a, delegable(1)).await;
    let mut req = by_ids(b, vec![root.id]);
    req.permissions = Some(DelegatedPermissions::new(true).requiring_acceptance());
    let d = env.grants.delegate(&ctx(a), req).await.unwrap();
    let check = |tenant| {
        let env = &env;
        async move {
            env.check
                .check(&request(USER, "bob", tenant), now())
                .await
                .unwrap()
        }
    };
    env.grants
        .change_binding(
            &ctx(b),
            d.sub(0),
            BindingChange::Bind(user("bob")),
            token(),
            None,
        )
        .await
        .unwrap();
    assert!(!check(b).await.granted);

    env.grants
        .delegation_verb(&ctx(b), d.id, Verb::Accept, token(), None)
        .await
        .unwrap();
    assert!(check(b).await.granted);
    let elsewhere = check(a).await;
    assert_eq!(elsewhere.reason, Some(DenialReason::SubjectNotBound));
}

/// `delegate` is conditional on the versions of the grants that it delegates.
#[tokio::test]
async fn delegate_checks_the_expected_versions_of_its_grants() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let g = issue_one(&env, a, delegable(1)).await;
    let mut req = by_ids(b, vec![g.id]);
    req.expected_versions.insert(g.id, g.version + 1);
    let err = env.grants.delegate(&ctx(a), req).await.unwrap_err();
    assert!(matches!(err, DomainError::VersionMismatch), "{err:?}");

    let mut req = by_ids(b, vec![g.id]);
    req.expected_versions.insert(g.id, g.version);
    req.expected_versions
        .insert(license_enforcement_sdk::GrantId(Uuid::now_v7()), 1);
    let err = env.grants.delegate(&ctx(a), req).await.unwrap_err();
    assert!(
        matches!(&err, DomainError::InvalidArgument(v) if v.iter().any(|f| f.field == "expected_versions" && f.reason == "NOT_IN_REQUEST")),
        "{err:?}"
    );

    let mut req = by_ids(b, vec![g.id]);
    req.expected_versions.insert(g.id, g.version);
    env.grants.delegate(&ctx(a), req).await.unwrap();
}

#[tokio::test]
async fn the_vendor_reads_the_grants_below_its_tenant() {
    let env = env().await;
    // V is the vendor; C its customer, which delegates to its child R.
    let (v, c, r) = tree(&env);
    let root = env
        .grants
        .issue(&ctx(v), c, vec![delegable(1)], false, token())
        .await
        .unwrap()
        .grants
        .remove(0);
    let sub = env
        .grants
        .delegate(&ctx(c), by_ids(r, vec![root.id]))
        .await
        .unwrap()
        .sub(0);

    for id in [root.id, sub] {
        assert_eq!(env.grants.get_grant(&ctx(v), id).await.unwrap().id, id);
    }
    let ids = |subtree| {
        let grants = &env.grants;
        async move {
            grants
                .list_grants(&ctx(v), &toolkit_odata::ODataQuery::new(), subtree)
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|g| g.id)
                .collect::<Vec<_>>()
        }
    };
    let below = ids(true).await;
    assert!(
        below.contains(&root.id) && below.contains(&sub),
        "{below:?}"
    );
    assert!(ids(false).await.is_empty());
}

/// Packs follow the rule of grants: a grantor without `read_subtree` does not
/// read the packs of its recipient tenant, by id, in a list, or through a
/// member filter; the vendor reads them with `read_subtree`.
#[tokio::test]
async fn packs_below_the_caller_need_read_subtree() {
    let pdp = std::sync::Arc::new(crate::test_support::RuleAuthZ::default());
    let env = crate::test_support::env_with(pdp.clone()).await;
    let (a, b, _) = tree(&env);
    let pack = env
        .grants
        .issue(
            &ctx(a),
            b,
            vec![crate::test_support::terms()],
            true,
            token(),
        )
        .await
        .unwrap()
        .pack
        .unwrap()
        .id;
    let filter = crate::domain::local_client::odata_query(&{
        let mut q = license_enforcement_sdk::dto::ListQuery::default();
        q.filter = Some(format!(
            "resource_type eq '{}'",
            crate::test_support::RESOURCE
        ));
        q
    })
    .unwrap();
    let listed = |query: &toolkit_odata::ODataQuery, subtree| {
        let grants = &env.grants;
        let query = query.clone();
        async move {
            grants
                .list_packs(&ctx(a), &query, subtree)
                .await
                .map(|p| p.items.iter().any(|p| p.id == pack))
        }
    };

    // A as the vendor.
    assert_eq!(env.grants.get_pack(&ctx(a), pack).await.unwrap().id, pack);
    assert!(listed(&filter, true).await.unwrap());
    assert!(!listed(&filter, false).await.unwrap());
    assert!(
        !listed(&toolkit_odata::ODataQuery::new(), false)
            .await
            .unwrap()
    );

    // A as a grantor: the policy does not give it `read_subtree`.
    pdp.set("license_enforcement.pack", "read_subtree", None);
    not_found(&env.grants.get_pack(&ctx(a), pack).await.unwrap_err());
    assert!(matches!(
        listed(&filter, true).await,
        Err(DomainError::PermissionDenied)
    ));
    // B reads its own pack without `read_subtree`.
    assert_eq!(env.grants.get_pack(&ctx(b), pack).await.unwrap().id, pack);
}

#[tokio::test]
async fn the_grantor_states_the_version_of_its_own_grant() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let root = issue_one(&env, a, delegable(1)).await;
    let sub = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);
    // The recipient changes its row: its version moves away from the grantor's.
    for change in [BindingChange::Bind(user("bob")), BindingChange::Unbind] {
        env.grants
            .change_binding(&ctx(b), sub, change, token(), None)
            .await
            .unwrap();
    }
    let own = env
        .grants
        .get_grant(&ctx(a), root.id)
        .await
        .unwrap()
        .version;
    let theirs = env.grants.get_grant(&ctx(b), sub).await.unwrap().version;
    assert_ne!(own, theirs);

    let grantor = ctx(a);
    let recall = |v| {
        env.grants
            .grant_verb(&grantor, sub, Verb::Recall, token(), Some(v))
    };
    assert!(matches!(
        recall(theirs).await,
        Err(DomainError::VersionMismatch)
    ));
    assert_eq!(recall(own).await.unwrap().id, root.id);
}

/// PRD: a delegation that names a bound grant changes none of the grants it
/// names.
#[tokio::test]
async fn a_bound_grant_fails_the_whole_delegation() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let free = issue_one(&env, a, delegable(1)).await;
    let bound = issue_one(&env, a, delegable(1)).await;
    env.grants
        .change_binding(
            &ctx(a),
            bound.id,
            BindingChange::Bind(user("bob")),
            token(),
            None,
        )
        .await
        .unwrap();
    let err = env
        .grants
        .delegate(&ctx(a), by_ids(b, vec![free.id, bound.id]))
        .await
        .unwrap_err();
    transition(&err);
    assert_eq!(env.grants.get_grant(&ctx(a), free.id).await.unwrap(), free);
}

/// A refusal of `get` does not stop `read_subtree`: the vendor reads a grant
/// and a pack below it with `read_subtree` alone.
#[tokio::test]
async fn read_subtree_alone_reads_below_the_caller() {
    let pdp = std::sync::Arc::new(crate::test_support::RuleAuthZ::default());
    let env = crate::test_support::env_with(pdp.clone()).await;
    let (a, b, _) = tree(&env);
    let issued = env
        .grants
        .issue(
            &ctx(a),
            b,
            vec![crate::test_support::terms()],
            true,
            token(),
        )
        .await
        .unwrap();
    let pack = issued.pack.unwrap().id;
    let grant = issued.grants[0].id;
    for resource in ["license_enforcement.grant", "license_enforcement.pack"] {
        pdp.set(resource, "get", None);
    }
    assert_eq!(
        env.grants.get_grant(&ctx(a), grant).await.unwrap().id,
        grant
    );
    assert_eq!(env.grants.get_pack(&ctx(a), pack).await.unwrap().id, pack);

    // Neither action: the PDP refuses.
    for resource in ["license_enforcement.grant", "license_enforcement.pack"] {
        pdp.set(resource, "read_subtree", None);
    }
    assert!(matches!(
        env.grants.get_grant(&ctx(a), grant).await,
        Err(DomainError::PermissionDenied)
    ));
    assert!(matches!(
        env.grants.get_pack(&ctx(a), pack).await,
        Err(DomainError::PermissionDenied)
    ));
}

/// `return` of a delegation ends every sub-grant and frees every parent; a
/// bound sub-grant refuses it for all of them.
#[tokio::test]
async fn return_of_a_delegation_applies_to_all_or_none() {
    let env = env().await;
    let (a, b, _) = tree(&env);
    let roots = [
        issue_one(&env, a, delegable(1)).await.id,
        issue_one(&env, a, delegable(1)).await.id,
    ];
    let d = env
        .grants
        .delegate(&ctx(a), by_ids(b, roots.to_vec()))
        .await
        .unwrap();
    env.grants
        .change_binding(
            &ctx(b),
            d.sub(0),
            BindingChange::Bind(user("bob")),
            token(),
            None,
        )
        .await
        .unwrap();
    let err = env
        .grants
        .delegation_verb(&ctx(b), d.id, Verb::Return, token(), None)
        .await
        .unwrap_err();
    transition(&err);
    for root in roots {
        let g = env.grants.get_grant(&ctx(a), root).await.unwrap();
        assert!(matches!(g.binding, Binding::Delegated { .. }), "{g:?}");
    }

    env.grants
        .change_binding(&ctx(b), d.sub(0), BindingChange::Unbind, token(), None)
        .await
        .unwrap();
    env.grants
        .delegation_verb(&ctx(b), d.id, Verb::Return, token(), None)
        .await
        .unwrap();
    for (i, root) in roots.into_iter().enumerate() {
        assert_eq!(
            env.grants.get_grant(&ctx(a), root).await.unwrap().binding,
            Binding::None
        );
        let conn = env.db.conn().unwrap();
        let sub = env
            .store
            .grant(&conn, &AccessScope::allow_all(), d.sub(i), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sub.termination_reason, Some(TerminationReason::Returned));
    }
}
