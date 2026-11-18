use crate::test_support::transition;
use license_enforcement_sdk::models::{Binding, GrantState, TerminationReason, WindowInput};
use time::Duration;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::grants::{BindingChange, Verb};
use crate::test_support::{
    Env, Sides, USER, ctx, env, issue_one, now, request, terms, token, user,
};
use license_enforcement_sdk::error::reason;

async fn verb(
    env: &Env,
    tenant: Uuid,
    id: license_enforcement_sdk::GrantId,
    v: Verb,
) -> Result<license_enforcement_sdk::models::Grant, DomainError> {
    env.grants
        .grant_verb(&ctx(tenant), id, v, token(), None)
        .await
}

#[tokio::test]
async fn accept_and_reject_end_the_wait() {
    let env = env().await;
    let a = Uuid::now_v7();
    let mut t = terms();
    t.permissions.requires_acceptance = true;
    let g1 = issue_one(&env, a, t.clone()).await;
    let g2 = issue_one(&env, a, t).await;

    let accepted = verb(&env, a, g1.id, Verb::Accept).await.unwrap();
    assert_eq!(accepted.state, GrantState::Active);
    assert_eq!(accepted.version, 2);
    transition(&verb(&env, a, g1.id, Verb::Accept).await.unwrap_err());

    let rejected = verb(&env, a, g2.id, Verb::Reject).await.unwrap();
    assert_eq!(rejected.state, GrantState::Terminated);
    assert_eq!(
        rejected.termination_reason,
        Some(TerminationReason::Rejected)
    );
    transition(&verb(&env, a, g2.id, Verb::Accept).await.unwrap_err());
}

#[tokio::test]
async fn suspend_reinstate_revoke_a_root() {
    let env = env().await;
    let a = Uuid::now_v7();
    let g = issue_one(&env, a, terms()).await;
    env.grants
        .change_binding(
            &ctx(a),
            g.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let req = request(USER, "alice", a);

    verb(&env, a, g.id, Verb::Suspend).await.unwrap();
    let d = env.check.check(&req, now()).await.unwrap();
    assert_eq!(
        d.reason,
        Some(license_enforcement_sdk::DenialReason::Suspended)
    );
    transition(&verb(&env, a, g.id, Verb::Suspend).await.unwrap_err());

    verb(&env, a, g.id, Verb::Reinstate).await.unwrap();
    assert!(env.check.check(&req, now()).await.unwrap().granted);

    let revoked = verb(&env, a, g.id, Verb::Revoke).await.unwrap();
    assert_eq!(revoked.termination_reason, Some(TerminationReason::Revoked));
    assert!(
        matches!(revoked.binding, Binding::Subject { .. }),
        "bindings stay for audit"
    );
    transition(&verb(&env, a, g.id, Verb::Revoke).await.unwrap_err());
    transition(&verb(&env, a, g.id, Verb::Return).await.unwrap_err());
}

#[tokio::test]
async fn a_verb_replays_by_token_and_checks_the_version() {
    let env = env().await;
    let a = Uuid::now_v7();
    let g = issue_one(&env, a, terms()).await;
    let t = token();
    let first = env
        .grants
        .grant_verb(&ctx(a), g.id, Verb::Suspend, t.clone(), Some(1))
        .await
        .unwrap();
    let again = env
        .grants
        .grant_verb(&ctx(a), g.id, Verb::Suspend, t.clone(), Some(1))
        .await
        .unwrap();
    assert_eq!(first, again);
    // Another subject of the same tenant with the same token does not get the
    // stored response: its own request runs and meets the new version.
    let other = crate::test_support::ctx_as(a, Uuid::now_v7());
    let err = env
        .grants
        .grant_verb(&other, g.id, Verb::Suspend, t, Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionMismatch), "{err:?}");
    let err = env
        .grants
        .grant_verb(&ctx(a), g.id, Verb::Reinstate, token(), Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionMismatch), "{err:?}");
}

#[tokio::test]
async fn renew_only_extends() {
    let env = env().await;
    let a = Uuid::now_v7();
    let g = issue_one(&env, a, terms()).await;
    let w = *g.validity.window();
    let longer = WindowInput {
        not_before: w.not_before,
        expires_at: w.expires_at + Duration::days(30),
        grace_until: Some(w.expires_at + Duration::days(40)),
    };
    let renewed = env
        .grants
        .renew_grant(&ctx(a), g.id, longer, token(), None)
        .await
        .unwrap();
    assert_eq!(renewed.validity.window().expires_at, longer.expires_at);

    let shorter = WindowInput {
        expires_at: w.expires_at,
        grace_until: None,
        ..longer
    };
    transition(
        &env.grants
            .renew_grant(&ctx(a), g.id, shorter, token(), None)
            .await
            .unwrap_err(),
    );
    // A later grace does not make up for an earlier `expires_at`.
    let earlier = WindowInput {
        expires_at: longer.expires_at - Duration::days(1),
        grace_until: Some(longer.expires_at + Duration::days(100)),
        ..longer
    };
    transition(
        &env.grants
            .renew_grant(&ctx(a), g.id, earlier, token(), None)
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn pack_verbs_apply_to_members_and_converge() {
    let env = env().await;
    let a = Uuid::now_v7();
    let c = ctx(a);
    let issued = env
        .grants
        .issue(&c, a, vec![terms(), terms()], true, token())
        .await
        .unwrap();
    let pack = issued.pack.unwrap();
    let member = issued.grants[0].id;

    // A member takes vendor verbs only through its pack.
    transition(&verb(&env, a, member, Verb::Suspend).await.unwrap_err());

    let suspended = env
        .grants
        .pack_verb(&c, pack.id, Verb::Suspend, token(), Some(1))
        .await
        .unwrap();
    assert_eq!(suspended.version, 2);
    for g in &issued.grants {
        let g = env.grants.get_grant(&c, g.id).await.unwrap();
        assert_eq!(g.state, GrantState::Suspended);
    }
    // Every member is already suspended: nothing changes.
    transition(
        &env.grants
            .pack_verb(&c, pack.id, Verb::Suspend, token(), None)
            .await
            .unwrap_err(),
    );

    let w = *issued.grants[0].validity.window();
    env.grants
        .renew_pack(
            &c,
            pack.id,
            WindowInput {
                not_before: w.not_before,
                expires_at: w.expires_at + Duration::days(5),
                grace_until: None,
            },
            token(),
            None,
        )
        .await
        .unwrap();
    let revoked = env
        .grants
        .pack_verb(&c, pack.id, Verb::Revoke, token(), None)
        .await
        .unwrap();
    assert_eq!(revoked.version, 4);
    assert!(revoked.counts.is_empty(), "no live member: {revoked:?}");
}

/// A pack is accepted or rejected as a whole, never member by member; until
/// it is accepted, its members block a vendor verb on it.
#[tokio::test]
async fn a_pack_is_accepted_or_rejected_as_a_whole() {
    let env = env().await;
    let a = Uuid::now_v7();
    let c = ctx(a);
    let mut waiting = terms();
    waiting.permissions.requires_acceptance = true;
    let issue = |grants| {
        let (env, c) = (&env, &c);
        async move { env.grants.issue(c, a, grants, true, token()).await }
    };

    // The members of a pack share `requires_acceptance`.
    let err = issue(vec![terms(), waiting.clone()]).await.unwrap_err();
    assert!(
        matches!(&err, DomainError::InvalidArgument(v)
            if v.iter().any(|v| v.reason == reason::MIXED_ACCEPTANCE)),
        "{err:?}"
    );

    let issued = issue(vec![waiting.clone(), waiting.clone()]).await.unwrap();
    let pack = issued.pack.unwrap().id;
    for v in [Verb::Accept, Verb::Reject] {
        transition(&verb(&env, a, issued.grants[0].id, v).await.unwrap_err());
    }
    transition(
        &env.grants
            .pack_verb(&c, pack, Verb::Suspend, token(), None)
            .await
            .unwrap_err(),
    );
    let accepted = env
        .grants
        .pack_verb(&c, pack, Verb::Accept, token(), Some(1))
        .await
        .unwrap();
    assert_eq!(accepted.version, 2);
    for g in &issued.grants {
        let g = env.grants.get_grant(&c, g.id).await.unwrap();
        assert_eq!(g.state, GrantState::Active);
    }
    env.grants
        .pack_verb(&c, pack, Verb::Suspend, token(), None)
        .await
        .unwrap();

    let issued = issue(vec![waiting.clone(), waiting]).await.unwrap();
    let pack = issued.pack.unwrap().id;
    let rejected = env
        .grants
        .pack_verb(&c, pack, Verb::Reject, token(), None)
        .await
        .unwrap();
    assert!(rejected.counts.is_empty(), "no live member: {rejected:?}");
    for g in &issued.grants {
        let g = env.grants.get_grant(&c, g.id).await.unwrap();
        assert_eq!(g.termination_reason, Some(TerminationReason::Rejected));
    }
}

#[tokio::test]
async fn renewing_a_root_carries_the_inheriting_chain() {
    let env = env().await;
    let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    env.tenants.link(b, c);
    let root = issue_one(&env, a, crate::test_support::delegable(2)).await;
    let delegate = |recipient, id| crate::test_support::by_ids(recipient, vec![id]);
    let s1 = env
        .grants
        .delegate(&ctx(a), delegate(b, root.id))
        .await
        .unwrap()
        .sub(0);
    let s2 = env
        .grants
        .delegate(&ctx(b), delegate(c, s1))
        .await
        .unwrap()
        .sub(0);

    let window = *root.validity.window();
    let longer = WindowInput {
        not_before: window.not_before,
        expires_at: window.expires_at + Duration::days(30),
        grace_until: None,
    };
    env.grants
        .renew_grant(&ctx(a), root.id, longer, token(), None)
        .await
        .unwrap();
    for (tenant, id) in [(b, s1), (c, s2)] {
        let g = env.grants.get_grant(&ctx(tenant), id).await.unwrap();
        assert_eq!(g.validity.window().expires_at, longer.expires_at);
    }
}

/// A pack verb waits for the members that another pack writer locked, then
/// reads the version that writer committed.
#[cfg(feature = "integration")]
#[tokio::test]
async fn a_pack_verb_reads_the_version_after_the_lock() {
    use crate::domain::repo::GrantRepo;
    use toolkit_security::AccessScope;
    let env = env().await;
    let a = Uuid::now_v7();
    let pack = env
        .grants
        .issue(&ctx(a), a, vec![terms(), terms()], true, token())
        .await
        .unwrap()
        .pack
        .unwrap();

    let (release, held) = tokio::sync::oneshot::channel::<()>();
    let (locked, is_locked) = tokio::sync::oneshot::channel::<()>();
    let (db, store) = (env.db.clone(), env.store);
    let writer = tokio::spawn(async move {
        db.transaction(move |tx| {
            Box::pin(async move {
                store.pack_grants(tx, a, pack.id, true).await?;
                store
                    .update_pack_version(tx, &AccessScope::allow_all(), pack.id, pack.version + 1)
                    .await?;
                locked.send(()).unwrap();
                held.await.unwrap();
                Ok(())
            })
        })
        .await
    });
    is_locked.await.unwrap();

    let grants = env.grants.clone();
    let suspend = tokio::spawn(async move {
        grants
            .pack_verb(&ctx(a), pack.id, Verb::Suspend, token(), None)
            .await
    });
    crate::test_support::until_a_lock_waits(&env).await;
    assert!(!suspend.is_finished());

    release.send(()).unwrap();
    writer.await.unwrap().unwrap();
    let done = suspend.await.unwrap().unwrap();
    assert_eq!(done.version, pack.version + 2);
}

/// Without the `issue` intake the vendor verbs, pack verbs, `renew`, and
/// `replace` of a root are not served.
#[tokio::test]
async fn vendor_verbs_need_the_issue_intake() {
    use crate::domain::grants::Target;
    use license_enforcement_sdk::dto::{ReplaceRequest, Successor};
    let plugin = std::sync::Arc::new(crate::test_support::TestPlugin::default());
    let env = crate::test_support::env_with_plugin(
        std::sync::Arc::new(crate::test_support::AllowAuthZ),
        plugin.clone(),
    )
    .await;
    let a = Uuid::now_v7();
    let g = issue_one(&env, a, terms()).await;
    let pack = env
        .grants
        .issue(&ctx(a), a, vec![terms()], true, token())
        .await
        .unwrap()
        .pack
        .unwrap();
    plugin
        .no_issue
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let not_served = |e: DomainError| assert!(matches!(e, DomainError::IntakeNotServed), "{e:?}");
    not_served(verb(&env, a, g.id, Verb::Suspend).await.unwrap_err());
    not_served(
        env.grants
            .pack_verb(&ctx(a), pack.id, Verb::Suspend, token(), None)
            .await
            .unwrap_err(),
    );
    not_served(
        env.grants
            .renew_grant(&ctx(a), g.id, terms().window, token(), None)
            .await
            .unwrap_err(),
    );
    not_served(
        env.grants
            .replace(
                &ctx(a),
                Target::Grant(g.id),
                ReplaceRequest::new(vec![Successor::new(terms(), None)], token()),
            )
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn revoking_a_root_ends_its_delegated_chain() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let root = issue_one(&env, a, crate::test_support::delegable(1)).await;
    let s1 = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);
    env.grants
        .change_binding(&ctx(b), s1, BindingChange::Bind(user("bob")), token(), None)
        .await
        .unwrap();
    let req = request(USER, "bob", b);
    assert!(env.check.check(&req, now()).await.unwrap().granted);

    verb(&env, a, root.id, Verb::Revoke).await.unwrap();
    let d = env.check.check(&req, now()).await.unwrap();
    assert!(!d.granted, "{d:?}");
    let sub = env.grants.get_grant(&ctx(b), s1).await.unwrap();
    assert_eq!(sub.state, GrantState::Terminated, "{sub:?}");
}

/// The counts stored in pack `id`, checked against a count of its live member
/// rows.
async fn pack_counts(
    env: &Env,
    holder: Uuid,
    id: license_enforcement_sdk::PackId,
) -> (u64, u64, u64) {
    use crate::domain::repo::GrantRepo;
    let stored = env.grants.get_pack(&ctx(holder), id).await.unwrap().counts;
    let conn = env.db.conn().unwrap();
    let mut rows = (0, 0, 0);
    for g in env
        .store
        .pack_grants(&conn, holder, id, false)
        .await
        .unwrap()
    {
        match (g.state, g.binding) {
            (GrantState::Terminated, _) => {}
            (_, Binding::Subject { .. }) => rows.1 += 1,
            (_, Binding::Delegated { .. }) => rows.2 += 1,
            _ => rows.0 += 1,
        }
    }
    let stored = stored.iter().fold((0, 0, 0), |s, c| {
        (s.0 + c.unbound, s.1 + c.bound, s.2 + c.delegated)
    });
    assert_eq!(stored, rows, "stored counts against the member rows");
    stored
}

/// The counts stored in a pack follow every member write: they equal a count
/// of the live member rows after each step.
#[tokio::test]
async fn pack_counts_follow_every_member_write() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let d = crate::test_support::delegable(1);
    let issued = env
        .grants
        .issue(&ctx(a), a, vec![d.clone(), d.clone(), d], true, token())
        .await
        .unwrap();
    let pack = issued.pack.unwrap().id;
    let (g0, g1) = (issued.grants[0].id, issued.grants[1].id);
    assert_eq!(pack_counts(&env, a, pack).await, (3, 0, 0));

    let bind = |id, change| {
        let env = &env;
        async move {
            env.grants
                .change_binding(&ctx(a), id, change, token(), None)
                .await
                .unwrap();
        }
    };
    bind(g0, BindingChange::Bind(user("alice"))).await;
    assert_eq!(pack_counts(&env, a, pack).await, (2, 1, 0));

    let delegation = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![g1]))
        .await
        .unwrap();
    assert_eq!(pack_counts(&env, a, pack).await, (1, 1, 1));

    bind(g0, BindingChange::Unbind).await;
    assert_eq!(pack_counts(&env, a, pack).await, (2, 0, 1));

    env.grants
        .delegation_verb(&ctx(a), delegation.id, Verb::Recall, token(), None)
        .await
        .unwrap();
    assert_eq!(pack_counts(&env, a, pack).await, (3, 0, 0));

    let revoked = env
        .grants
        .pack_verb(&ctx(a), pack, Verb::Revoke, token(), None)
        .await
        .unwrap();
    assert!(revoked.counts.is_empty(), "{revoked:?}");
    assert_eq!(pack_counts(&env, a, pack).await, (0, 0, 0));
}

/// More member writes against the stored counts: a bind after a recall,
/// renew, revoke of a delegated member, and replace.
#[tokio::test]
async fn pack_counts_follow_rebind_renew_revoke_and_replace() {
    use license_enforcement_sdk::dto::{ReplaceRequest, Successor};
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let delegable = crate::test_support::delegable(1);
    let issued = env
        .grants
        .issue(
            &ctx(a),
            a,
            vec![delegable.clone(), delegable],
            true,
            token(),
        )
        .await
        .unwrap();
    let pack = issued.pack.unwrap().id;
    let (g0, g1) = (issued.grants[0].id, issued.grants[1].id);
    assert_eq!(pack_counts(&env, a, pack).await, (2, 0, 0));

    let delegate = |id| {
        let env = &env;
        async move {
            env.grants
                .delegate(&ctx(a), crate::test_support::by_ids(b, vec![id]))
                .await
                .unwrap()
                .sub(0)
        }
    };
    let recalled = delegate(g0).await;
    assert_eq!(pack_counts(&env, a, pack).await, (1, 0, 1));
    env.grants
        .grant_verb(&ctx(a), recalled, Verb::Recall, token(), None)
        .await
        .unwrap();
    env.grants
        .change_binding(
            &ctx(a),
            g0,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(pack_counts(&env, a, pack).await, (1, 1, 0));

    delegate(g1).await;
    assert_eq!(pack_counts(&env, a, pack).await, (0, 1, 1));

    let window = *issued.grants[0].validity.window();
    let longer = WindowInput {
        not_before: window.not_before,
        expires_at: window.expires_at + Duration::days(5),
        grace_until: None,
    };
    env.grants
        .renew_pack(&ctx(a), pack, longer, token(), None)
        .await
        .unwrap();
    for id in [g0, g1] {
        let g = env.grants.get_grant(&ctx(a), id).await.unwrap();
        assert_eq!(
            g.validity.window().expires_at,
            window.expires_at + Duration::days(5)
        );
    }
    assert_eq!(pack_counts(&env, a, pack).await, (0, 1, 1));

    env.grants
        .pack_verb(&ctx(a), pack, Verb::Revoke, token(), None)
        .await
        .unwrap();
    assert_eq!(pack_counts(&env, a, pack).await, (0, 0, 0));

    // Replace: the old pack ends, the new one counts the moved binding.
    let issued = env
        .grants
        .issue(&ctx(a), a, vec![terms(), terms()], true, token())
        .await
        .unwrap();
    let old = issued.pack.unwrap().id;
    let bound = issued.grants[0].id;
    env.grants
        .change_binding(
            &ctx(a),
            bound,
            BindingChange::Bind(user("bob")),
            token(),
            None,
        )
        .await
        .unwrap();
    let done = env
        .grants
        .replace(
            &ctx(a),
            crate::domain::grants::Target::Pack(old),
            ReplaceRequest::new(
                vec![
                    Successor::new(terms(), Some(bound)),
                    Successor::new(terms(), None),
                ],
                token(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(pack_counts(&env, a, old).await, (0, 0, 0));
    assert_eq!(pack_counts(&env, a, done.pack.unwrap().id).await, (1, 1, 0));
}

#[tokio::test]
async fn a_repeat_after_the_window_ended_returns_the_first_result() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let start = now() - Duration::days(1);
    let end = now() + Duration::seconds(2);
    let window = |expires_at| WindowInput {
        not_before: start,
        expires_at,
        grace_until: None,
    };
    let mut short = terms();
    short.window = window(end - Duration::seconds(1));
    let g = issue_one(&env, a, short.clone()).await;
    let pack = env
        .grants
        .issue(&ctx(a), a, vec![short], true, token())
        .await
        .unwrap()
        .pack
        .unwrap()
        .id;
    let parent = issue_one(&env, a, crate::test_support::delegable(1)).await;
    let delegate = crate::test_support::by_ids(b, vec![parent.id]);
    let (t1, t2) = (token(), token());
    let c = ctx(a);

    let renew = || {
        env.grants
            .renew_grant(&c, g.id, window(end), t1.clone(), None)
    };
    let renew_pack = || {
        env.grants
            .renew_pack(&c, pack, window(end), t2.clone(), None)
    };
    let first = (
        renew().await.unwrap(),
        renew_pack().await.unwrap(),
        env.grants
            .delegate(&ctx(a), delegate.clone())
            .await
            .unwrap(),
    );
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let again = (
        renew().await.unwrap(),
        renew_pack().await.unwrap(),
        env.grants.delegate(&ctx(a), delegate).await.unwrap(),
    );
    assert_eq!(first, again);
}

/// Verbs on a sub-grant through its grant route and a pack replacement that moves a delegation keep the
/// stored counts equal to the member rows.
#[tokio::test]
async fn pack_counts_follow_the_verbs_on_sub_grants() {
    use crate::domain::grants::Target;
    use crate::test_support::{by_ids, delegable};
    use license_enforcement_sdk::dto::{ReplaceRequest, Successor};
    use license_enforcement_sdk::models::DelegatedPermissions;
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let issued = env
        .grants
        .issue(&ctx(a), a, vec![delegable(1), terms()], true, token())
        .await
        .unwrap();
    let pack = issued.pack.unwrap().id;
    let g = issued.grants[0].id;
    let delegate = |acceptance: bool| {
        let env = &env;
        async move {
            let mut req = by_ids(b, vec![g]);
            let permissions = DelegatedPermissions::new(true);
            req.permissions = Some(if acceptance {
                permissions.requiring_acceptance()
            } else {
                permissions
            });
            let sub = env.grants.delegate(&ctx(a), req).await.unwrap().sub(0);
            assert_eq!(pack_counts(env, a, pack).await, (1, 0, 1));
            sub
        }
    };
    for (acceptance, tenant, v) in [
        (true, b, Verb::Reject),
        (false, b, Verb::Return),
        (false, a, Verb::Recall),
    ] {
        let sub = delegate(acceptance).await;
        verb(&env, tenant, sub, v).await.unwrap();
        assert_eq!(pack_counts(&env, a, pack).await, (2, 0, 0), "{v:?}");
    }

    delegate(false).await;

    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Pack(pack),
            ReplaceRequest::new(
                vec![
                    Successor::new(delegable(1), Some(g)),
                    Successor::new(terms(), None),
                ],
                token(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(pack_counts(&env, a, pack).await, (0, 0, 0));
    assert_eq!(pack_counts(&env, a, done.pack.unwrap().id).await, (1, 0, 1));
}

#[tokio::test]
async fn revoke_ends_grants_that_await_acceptance() {
    let env = env().await;
    let a = Uuid::now_v7();
    let mut waiting = terms();
    waiting.permissions.requires_acceptance = true;
    let root = issue_one(&env, a, waiting.clone()).await;
    let revoked = verb(&env, a, root.id, Verb::Revoke).await.unwrap();
    assert_eq!(revoked.state, GrantState::Terminated);
    assert_eq!(revoked.termination_reason, Some(TerminationReason::Revoked));

    let issued = env
        .grants
        .issue(&ctx(a), a, vec![waiting.clone(), waiting], true, token())
        .await
        .unwrap();
    let pack = issued.pack.unwrap().id;
    env.grants
        .pack_verb(&ctx(a), pack, Verb::Revoke, token(), None)
        .await
        .unwrap();
    for g in &issued.grants {
        let g = env.grants.get_grant(&ctx(a), g.id).await.unwrap();
        assert_eq!(g.termination_reason, Some(TerminationReason::Revoked));
    }
    assert_eq!(pack_counts(&env, a, pack).await, (0, 0, 0));
}

/// A suspended root can be replaced: its binding moves to the active
/// successor, and the check grants again.
#[tokio::test]
async fn replace_lifts_a_suspended_root() {
    use crate::domain::grants::Target;
    use license_enforcement_sdk::dto::{ReplaceRequest, Successor};
    let env = env().await;
    let a = Uuid::now_v7();
    let g = issue_one(&env, a, terms()).await;
    env.grants
        .change_binding(
            &ctx(a),
            g.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    verb(&env, a, g.id, Verb::Suspend).await.unwrap();
    let req = request(USER, "alice", a);
    assert!(!env.check.check(&req, now()).await.unwrap().granted);

    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(g.id),
            ReplaceRequest::new(vec![Successor::new(terms(), Some(g.id))], token()),
        )
        .await
        .unwrap();
    assert!(matches!(done.grants[0].binding, Binding::Subject { .. }));
    let old = env.grants.get_grant(&ctx(a), g.id).await.unwrap();
    assert_eq!(old.termination_reason, Some(TerminationReason::Replaced));
    assert!(env.check.check(&req, now()).await.unwrap().granted);
}

#[tokio::test]
async fn a_bad_window_names_its_field() {
    let env = env().await;
    let a = Uuid::now_v7();
    let grant = issue_one(&env, a, terms()).await;
    let window = *grant.validity.window();
    let bad = WindowInput {
        not_before: window.expires_at,
        expires_at: window.not_before,
        grace_until: None,
    };
    let field = |err: DomainError| match err {
        DomainError::InvalidArgument(v) => v.into_iter().map(|f| f.field).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let renewed = env
        .grants
        .renew_grant(&ctx(a), grant.id, bad, token(), None)
        .await
        .unwrap_err();
    assert_eq!(field(renewed), ["window"]);
}

#[tokio::test]
async fn renew_with_the_same_window_is_refused() {
    let env = env().await;
    let a = Uuid::now_v7();
    let g = issue_one(&env, a, terms()).await;
    let w = *g.validity.window();
    let same = WindowInput {
        not_before: w.not_before,
        expires_at: w.expires_at,
        grace_until: w.grace_until,
    };
    let err = env
        .grants
        .renew_grant(&ctx(a), g.id, same, token(), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::InvalidTransition(m) if m.contains("already has this window")),
        "{err:?}"
    );
}

#[tokio::test]
async fn renew_pack_refuses_a_window_that_does_not_contain_a_member() {
    let env = env().await;
    let a = Uuid::now_v7();
    let issued = env
        .grants
        .issue(&ctx(a), a, vec![terms()], true, token())
        .await
        .unwrap();
    let pack = issued.pack.unwrap();
    let w = *issued.grants[0].validity.window();
    let later_start = WindowInput {
        not_before: w.not_before + Duration::hours(1),
        expires_at: w.expires_at + Duration::days(5),
        grace_until: None,
    };
    let err = env
        .grants
        .renew_pack(&ctx(a), pack.id, later_start, token(), None)
        .await
        .unwrap_err();
    transition(&err);
    assert_eq!(
        env.grants.get_pack(&ctx(a), pack.id).await.unwrap().version,
        pack.version
    );
}

/// A pack renew skips the members that already have the window, and refuses
/// when every member has it.
#[tokio::test]
async fn renew_pack_converges_on_the_window() {
    use crate::infra::storage::entity::grant::Column;
    let env = env().await;
    let a = Uuid::now_v7();
    let issued = env
        .grants
        .issue(&ctx(a), a, vec![terms(), terms()], true, token())
        .await
        .unwrap();
    let pack = issued.pack.unwrap();
    let w = *issued.grants[0].validity.window();
    let longer = WindowInput {
        not_before: w.not_before,
        expires_at: w.expires_at + Duration::days(5),
        grace_until: None,
    };
    // One member already has the new window.
    let first = issued.grants[0].id;
    crate::test_support::edit_row(&env, first, Column::ExpiresAt, longer.expires_at).await;
    crate::test_support::edit_row(&env, first, Column::EffectiveEnd, longer.expires_at).await;
    let renewed = env
        .grants
        .renew_pack(&ctx(a), pack.id, longer, token(), None)
        .await
        .unwrap();
    assert_eq!(renewed.version, pack.version + 1);
    for g in &issued.grants {
        let g = env.grants.get_grant(&ctx(a), g.id).await.unwrap();
        assert_eq!(g.validity.window().expires_at, longer.expires_at);
    }
    let err = env
        .grants
        .renew_pack(&ctx(a), pack.id, longer, token(), None)
        .await
        .unwrap_err();
    transition(&err);
    assert_eq!(
        env.grants.get_pack(&ctx(a), pack.id).await.unwrap().version,
        renewed.version
    );
}

/// A plugin that verifies evidence takes the window from it: renew of a grant
/// or a pack is refused, before anything is written.
#[tokio::test]
async fn renew_needs_a_plugin_that_does_not_verify_evidence() {
    let env = crate::test_support::env_with_plugin(
        std::sync::Arc::new(crate::test_support::AllowAuthZ),
        std::sync::Arc::new(crate::test_support::TestPlugin::verifying(true, |_, _| {
            None
        })),
    )
    .await;
    let a = Uuid::now_v7();
    let g = issue_one(&env, a, terms()).await;
    let pack = env
        .grants
        .issue(&ctx(a), a, vec![terms()], true, token())
        .await
        .unwrap()
        .pack
        .unwrap();
    let w = *g.validity.window();
    let longer = WindowInput {
        not_before: w.not_before,
        expires_at: w.expires_at + Duration::days(5),
        grace_until: None,
    };
    let err = env
        .grants
        .renew_grant(&ctx(a), g.id, longer, token(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::RenewNeedsNewEvidence), "{err:?}");
    let err = env
        .grants
        .renew_pack(&ctx(a), pack.id, longer, token(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::RenewNeedsNewEvidence), "{err:?}");
    assert_eq!(
        env.grants.get_grant(&ctx(a), g.id).await.unwrap().version,
        g.version
    );
}

/// The serde form of what a verb hashes for idempotency: a stored hash must
/// match the replay of the same request after a deploy.
#[test]
fn the_hashed_names_stay_fixed() {
    use crate::domain::grants::Target;
    use license_enforcement_sdk::{GrantId, PackId};
    let id = Uuid::nil();
    let json = |v: serde_json::Value| v.to_string();
    let verbs = [
        Verb::Accept,
        Verb::Reject,
        Verb::Suspend,
        Verb::Reinstate,
        Verb::Revoke,
        Verb::Return,
        Verb::Recall,
    ]
    .map(|v| json(serde_json::to_value(v).unwrap()));
    assert_eq!(
        verbs,
        [
            r#""Accept""#,
            r#""Reject""#,
            r#""Suspend""#,
            r#""Reinstate""#,
            r#""Revoke""#,
            r#""Return""#,
            r#""Recall""#
        ]
    );
    assert_eq!(
        json(serde_json::to_value(super::PackChange::Verb(Verb::Revoke)).unwrap()),
        r#"{"Verb":"Revoke"}"#
    );
    assert!(
        json(serde_json::to_value(super::PackChange::Renew(terms().window)).unwrap())
            .starts_with(r#"{"Renew":"#)
    );
    assert_eq!(
        json(serde_json::to_value(Target::Grant(GrantId(id))).unwrap()),
        format!(r#"{{"Grant":"{id}"}}"#)
    );
    assert_eq!(
        json(serde_json::to_value(Target::Pack(PackId(id))).unwrap()),
        format!(r#"{{"Pack":"{id}"}}"#)
    );
    assert_eq!(
        json(serde_json::to_value(BindingChange::Unbind).unwrap()),
        r#""Unbind""#
    );
    assert!(
        json(serde_json::to_value(BindingChange::Bind(user("alice"))).unwrap())
            .starts_with(r#"{"Bind":"#)
    );
}
