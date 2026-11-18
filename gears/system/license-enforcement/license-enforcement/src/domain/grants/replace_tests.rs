use std::sync::Arc;

use license_enforcement_sdk::GrantId;
use license_enforcement_sdk::dto::{ReplaceRequest, Successor};
use license_enforcement_sdk::models::{Binding, Conditions, GrantState, TerminationReason};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::grants::{BindingChange, Target, Verb};
use crate::domain::repo::GrantRepo;
use crate::test_support::{
    Sides, USER, ctx, delegable, env, input, issue_one, now, request, terms, token, user,
};

fn successors(list: Vec<Successor>) -> ReplaceRequest {
    ReplaceRequest::new(list, token())
}

#[tokio::test]
async fn replace_moves_a_subject_binding_to_its_successor() {
    let env = env().await;
    let a = Uuid::now_v7();
    let old = issue_one(&env, a, terms()).await;
    env.grants
        .change_binding(
            &ctx(a),
            old.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();

    // No successor names the bound grant: nothing changes.
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(terms(), None)]),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::BindingsWithoutSuccessor(ids) if ids == &[(old.id, None)]),
        "{err:?}"
    );
    assert_eq!(
        env.grants.get_grant(&ctx(a), old.id).await.unwrap().state,
        GrantState::Active
    );

    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![
                Successor::new(terms(), Some(old.id)),
                Successor::new(terms(), None),
            ]),
        )
        .await
        .unwrap();
    assert!(matches!(done.grants[0].binding, Binding::Subject { .. }));
    assert_eq!(done.grants[1].binding, Binding::None);
    let old_now = env.grants.get_grant(&ctx(a), old.id).await.unwrap();
    assert_eq!(
        old_now.termination_reason,
        Some(TerminationReason::Replaced)
    );
    let d = env
        .check
        .check(&request(USER, "alice", a), now())
        .await
        .unwrap();
    assert_eq!(d.grant_id, Some(done.grants[0].id));
}

#[tokio::test]
async fn replace_moves_a_delegation_under_the_successor() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let old = issue_one(&env, a, delegable(1)).await;
    let sub = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![old.id]))
        .await
        .unwrap();
    let sub = crate::test_support::sub_grants(&env, &sub).await.remove(0);
    env.grants
        .change_binding(
            &ctx(b),
            sub.id,
            BindingChange::Bind(user("bob")),
            token(),
            None,
        )
        .await
        .unwrap();

    // A successor that cannot be the parent (depth 0) does not take it.
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(terms(), Some(old.id))]),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::BindingsWithoutSuccessor(_)),
        "{err:?}"
    );

    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(delegable(1), Some(old.id))]),
        )
        .await
        .unwrap();
    let successor = &done.grants[0];
    assert_eq!(successor.binding, Binding::delegated(sub.id, b));
    let conn = env.db.conn().unwrap();
    let moved = env
        .store
        .grant(&conn, &AccessScope::allow_all(), sub.id, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.parent_grant_id, Some(successor.id));
    assert_eq!(moved.evidence_id, successor.evidence_id);
    assert_eq!(
        env.store
            .chain(&conn, sub.id)
            .await
            .unwrap()
            .last()
            .unwrap()
            .id,
        successor.id
    );
    assert!(
        env.check
            .check(&request(USER, "bob", b), now())
            .await
            .unwrap()
            .granted
    );
}

#[tokio::test]
async fn replace_pack_gives_a_new_pack_and_refuses_members_alone() {
    let env = env().await;
    let a = Uuid::now_v7();
    let issued = env
        .grants
        .issue(&ctx(a), a, vec![terms(), terms()], true, token())
        .await
        .unwrap();
    let pack = issued.pack.unwrap();
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(issued.grants[0].id),
            successors(vec![Successor::new(terms(), None)]),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::InvalidTransition(_)), "{err:?}");

    let stranger = license_enforcement_sdk::GrantId(Uuid::now_v7());
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Pack(pack.id),
            successors(vec![Successor::new(terms(), Some(stranger))]),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::InvalidArgument(_)), "{err:?}");

    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Pack(pack.id),
            successors(vec![Successor::new(terms(), None)]),
        )
        .await
        .unwrap();
    let new_pack = done.pack.unwrap();
    assert_ne!(new_pack.id, pack.id);
    for g in &issued.grants {
        let g = env.grants.get_grant(&ctx(a), g.id).await.unwrap();
        assert_eq!(g.termination_reason, Some(TerminationReason::Replaced));
    }
    assert_eq!(
        env.grants.get_pack(&ctx(a), pack.id).await.unwrap().version,
        2
    );
}

#[tokio::test]
async fn a_binding_moves_only_to_a_successor_that_can_use_it_now() {
    let env = env().await;
    let a = Uuid::now_v7();
    let old = issue_one(&env, a, terms()).await;
    env.grants
        .change_binding(
            &ctx(a),
            old.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();

    let mut later = terms();
    later.window.not_before = now() + time::Duration::days(1);
    let mut no_bind = terms();
    no_bind.permissions.may_bind = false;
    let mut waiting = terms();
    waiting.permissions.requires_acceptance = true;
    for t in [later, no_bind, waiting] {
        let err = env
            .grants
            .replace(
                &ctx(a),
                Target::Grant(old.id),
                successors(vec![Successor::new(t, Some(old.id))]),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::BindingsWithoutSuccessor(ids) if ids == &[(old.id, None)]),
            "{err:?}"
        );
    }
}

#[tokio::test]
async fn a_fixed_binding_moves_only_to_a_successor_fixed_on_the_same_subject() {
    let env = env().await;
    let a = Uuid::now_v7();
    let fixed = |id: &str| {
        let mut t = terms();
        t.fixed_subject = Some(input(user(id)));
        t
    };
    let old = issue_one(&env, a, fixed("alice")).await;

    for t in [terms(), fixed("bob")] {
        let err = env
            .grants
            .replace(
                &ctx(a),
                Target::Grant(old.id),
                successors(vec![Successor::new(t, Some(old.id))]),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::BindingsWithoutSuccessor(ids) if ids == &[(old.id, None)]),
            "{err:?}"
        );
    }

    // The stored binding names a later minor version of the same type: the
    // subject is the same.
    crate::test_support::edit_row(
        &env,
        old.id,
        crate::infra::storage::entity::grant::Column::BindingSubjectType,
        "gts.cf.core.lic.subj.v1~test.le.app.user.v1.1~".to_owned(),
    )
    .await;
    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(fixed("alice"), Some(old.id))]),
        )
        .await
        .unwrap();
    assert!(done.grants[0].binding_fixed);
    // The successor keeps the subject as its admission fixed it.
    assert_eq!(done.grants[0].binding, Binding::subject(user("alice")));
    let d = env
        .check
        .check(&request(USER, "alice", a), now())
        .await
        .unwrap();
    assert_eq!(d.grant_id, Some(done.grants[0].id));
}

#[tokio::test]
async fn a_successor_fixed_on_the_subject_takes_a_binding_that_is_not_fixed() {
    let env = env().await;
    let a = Uuid::now_v7();
    let old = issue_one(&env, a, terms()).await;
    env.grants
        .change_binding(
            &ctx(a),
            old.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let mut fixed = terms();
    fixed.fixed_subject = Some(input(user("alice")));
    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(fixed, Some(old.id))]),
        )
        .await
        .unwrap();
    assert!(done.grants[0].binding_fixed);
    assert_eq!(done.grants[0].binding, Binding::subject(user("alice")));
    let d = env
        .check
        .check(&request(USER, "alice", a), now())
        .await
        .unwrap();
    assert_eq!(d.grant_id, Some(done.grants[0].id));
}

#[tokio::test]
async fn revoking_the_successor_ends_the_moved_chain() {
    use crate::domain::grants::Verb;
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let old = issue_one(&env, a, delegable(1)).await;
    let sub = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![old.id]))
        .await
        .unwrap()
        .sub(0);
    let successor = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(delegable(1), Some(old.id))]),
        )
        .await
        .unwrap()
        .grants[0]
        .id;

    env.grants
        .grant_verb(&ctx(a), successor, Verb::Revoke, token(), None)
        .await
        .unwrap();
    let moved = env.grants.get_grant(&ctx(b), sub).await.unwrap();
    assert_eq!(moved.state, GrantState::Terminated);
    assert_eq!(moved.termination_reason, Some(TerminationReason::Revoked));
}

#[tokio::test]
async fn a_grant_that_awaits_acceptance_blocks_replace() {
    let env = env().await;
    let a = Uuid::now_v7();
    let mut waiting = terms();
    waiting.permissions.requires_acceptance = true;
    let issued = env
        .grants
        .issue(
            &ctx(a),
            a,
            vec![waiting.clone(), waiting.clone()],
            true,
            token(),
        )
        .await
        .unwrap();
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Pack(issued.pack.unwrap().id),
            successors(vec![Successor::new(terms(), None)]),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::InvalidTransition(d) if d.contains(&issued.grants[1].id.0.to_string())),
        "{err:?}"
    );
    let first = env
        .grants
        .get_grant(&ctx(a), issued.grants[0].id)
        .await
        .unwrap();
    assert_eq!(first.state, GrantState::AwaitingAcceptance);

    let single = issue_one(&env, a, waiting).await;
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(single.id),
            successors(vec![Successor::new(terms(), None)]),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::InvalidTransition(_)), "{err:?}");
}

#[tokio::test]
async fn replace_pack_answers_with_the_moved_bindings() {
    let env = env().await;
    let a = Uuid::now_v7();
    let issued = env
        .grants
        .issue(&ctx(a), a, vec![terms(), terms()], true, token())
        .await
        .unwrap();
    let bound = issued.grants[0].id;
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
    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Pack(issued.pack.unwrap().id),
            successors(vec![
                Successor::new(terms(), Some(bound)),
                Successor::new(terms(), None),
            ]),
        )
        .await
        .unwrap();
    let counts = &done.pack.unwrap().counts;
    assert_eq!(counts.len(), 1, "{counts:?}");
    assert_eq!((counts[0].bound, counts[0].unbound), (1, 1));
}

#[tokio::test]
async fn two_successors_cannot_name_the_same_grant() {
    let env = env().await;
    let a = Uuid::now_v7();
    let old = issue_one(&env, a, terms()).await;
    let same = || Successor::new(terms(), Some(old.id));
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![same(), same()]),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::InvalidArgument(v) if v.iter().any(|v| v.reason == "DUPLICATE")),
        "{err:?}"
    );
}

/// A delegated sub-grant of the replaced grant, in tenant `b`.
async fn delegated(env: &crate::test_support::Env, a: Uuid, b: Uuid, id: GrantId) -> GrantId {
    env.tenants.link(a, b);
    env.grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![id]))
        .await
        .unwrap()
        .sub(0)
}

#[tokio::test]
async fn a_moved_chain_compares_subject_types_without_minor_versions() {
    use crate::infra::storage::entity::grant::Column;
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let old = issue_one(&env, a, delegable(1)).await;
    let sub = delegated(&env, a, b, old.id).await;
    // The sub-grant names a later minor version of the same subject type.
    crate::test_support::edit_row(
        &env,
        sub,
        Column::SubjectType,
        "gts.cf.core.lic.subj.v1~test.le.app.user.v1.1~".to_owned(),
    )
    .await;
    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(delegable(1), Some(old.id))]),
        )
        .await
        .unwrap();
    assert!(
        matches!(done.grants[0].binding, Binding::Delegated { grant_id, .. } if grant_id == sub)
    );
}

/// Renewal of an expired license: the chain that inherits the ended window of
/// the replaced root moves to the successor and takes its window.
#[tokio::test]
async fn an_inherited_chain_of_an_ended_root_moves_to_the_successor() {
    use crate::infra::storage::entity::grant::Column;
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let old = issue_one(&env, a, delegable(1)).await;
    let sub = delegated(&env, a, b, old.id).await;
    let past = now() - time::Duration::days(1);
    for id in [old.id, sub] {
        for column in [Column::ExpiresAt, Column::EffectiveEnd] {
            crate::test_support::edit_row(&env, id, column, past).await;
        }
        crate::test_support::edit_row(&env, id, Column::NotBefore, past - time::Duration::days(1))
            .await;
    }
    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(delegable(1), Some(old.id))]),
        )
        .await
        .unwrap();
    let successor = &done.grants[0];
    assert!(matches!(successor.binding, Binding::Delegated { grant_id, .. } if grant_id == sub));
    let conn = env.db.conn().unwrap();
    let moved = env
        .store
        .grant(&conn, &AccessScope::allow_all(), sub, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.state, GrantState::Active);
    assert_eq!(moved.parent_grant_id, Some(successor.id));
    assert_eq!(moved.validity.window(), successor.validity.window());
}

#[tokio::test]
async fn a_moved_chain_keeps_the_acceptance_step_of_its_new_parent() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let root = issue_one(&env, a, delegable(1)).await;
    let d = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![root.id]))
        .await
        .unwrap();
    let sub = crate::test_support::sub_grants(&env, &d).await.remove(0);
    assert!(super::subset_of(&sub, &root));
    // `fits` also refuses a successor that awaits acceptance; this holds the
    // rule for one that has been accepted.
    let mut gated = root;
    gated.permissions.requires_acceptance = true;
    assert!(!super::subset_of(&sub, &gated));
    // Issue refuses conditions under the default evaluator, so the rule is held
    // here: a parent with conditions admits only a sub-grant with the same.
    let mut conditioned = gated;
    conditioned.permissions.requires_acceptance = false;
    conditioned.conditions = Some(Conditions(serde_json::json!({ "region": "eu" })));
    assert!(!super::subset_of(&sub, &conditioned));
    let mut same = sub;
    same.conditions.clone_from(&conditioned.conditions);
    assert!(super::subset_of(&same, &conditioned));
}

/// A caller without `GRANT/replace` learns nothing about the members of the
/// pack and does not reach the plugin.
#[tokio::test]
async fn replace_pack_asks_the_members_first() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let called = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&called);
    let pdp = Arc::new(crate::test_support::RuleAuthZ::default());
    let env = crate::test_support::env_with_plugin(
        pdp.clone(),
        Arc::new(crate::test_support::TestPlugin::admitting(move |_, _| {
            seen.store(true, Ordering::SeqCst);
        })),
    )
    .await;
    let a = Uuid::now_v7();
    let issued = env
        .grants
        .issue(&ctx(a), a, vec![terms()], true, token())
        .await
        .unwrap();
    called.store(false, Ordering::SeqCst);
    // A refusal, and a decision that leaves the member out.
    for rule in [None, Some(vec![Uuid::now_v7()])] {
        pdp.set("license_enforcement.grant", "replace", rule);
        for named in [issued.grants[0].id, GrantId(Uuid::now_v7())] {
            let err = env
                .grants
                .replace(
                    &ctx(a),
                    Target::Pack(issued.pack.as_ref().unwrap().id),
                    successors(vec![Successor::new(terms(), Some(named))]),
                )
                .await
                .unwrap_err();
            assert!(matches!(err, DomainError::PermissionDenied), "{err:?}");
        }
    }
    assert!(!called.load(Ordering::SeqCst));
}

/// As `delegate`, a successor with recipient rules takes no delegation: the
/// rules are not evaluated yet.
#[tokio::test]
async fn a_successor_with_recipient_rules_takes_no_delegation() {
    use super::{Fit, fits};
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let old = issue_one(&env, a, delegable(1)).await;
    let sub = delegated(&env, a, b, old.id).await;
    let old = env.grants.get_grant(&ctx(a), old.id).await.unwrap();
    let mut successor = issue_one(&env, a, delegable(1)).await;
    let conn = env.db.conn().unwrap();
    let subtrees = env.store.live_subtrees(&conn, &[sub], false).await.unwrap();

    assert!(matches!(fits(&successor, &old, &subtrees, now()), Fit::Yes));
    successor.recipient_rules = Some(license_enforcement_sdk::models::Conditions(
        serde_json::json!({"op": "eq"}),
    ));
    assert!(matches!(
        fits(&successor, &old, &subtrees, now()),
        Fit::BlockedBy(Some(_))
    ));
}

/// I4 as in the check: a successor without conditions admits the conditions
/// of the moved chain.
#[tokio::test]
async fn a_chain_with_conditions_moves_to_a_successor_without_them() {
    use crate::infra::storage::entity::grant::Column;
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let old = issue_one(&env, a, delegable(1)).await;
    let sub = delegated(&env, a, b, old.id).await;
    for id in [old.id, sub] {
        crate::test_support::edit_row(
            &env,
            id,
            Column::Conditions,
            serde_json::json!({ "region": "eu" }),
        )
        .await;
    }
    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(delegable(1), Some(old.id))]),
        )
        .await
        .unwrap();
    assert!(
        matches!(done.grants[0].binding, Binding::Delegated { grant_id, .. } if grant_id == sub)
    );
}

/// Violations name the fields of the replace request.
#[tokio::test]
async fn violations_name_the_terms_of_the_successors() {
    let env = env().await;
    let a = Uuid::now_v7();
    let old = issue_one(&env, a, terms()).await;
    let mut long = terms();
    long.resource.id = Some("x".repeat(license_enforcement_sdk::limits::MAX_ID_LEN + 1));
    let violations = |successor: Successor| {
        let env = &env;
        async move {
            let err = env
                .grants
                .replace(&ctx(a), Target::Grant(old.id), successors(vec![successor]))
                .await
                .unwrap_err();
            let DomainError::InvalidArgument(v) = err else {
                panic!("{err:?}");
            };
            v.into_iter().map(|v| v.field).collect::<Vec<_>>()
        }
    };
    let fields = violations(Successor::new(long, Some(old.id))).await;
    assert_eq!(fields, ["successors[0].terms.resource.id"], "{fields:?}");

    // An ended window is refused before admission: its own call, since the
    // id above is refused first.
    let mut ended = terms();
    ended.window.expires_at = now() - time::Duration::hours(1);
    let fields = violations(Successor::new(ended, Some(old.id))).await;
    assert!(
        !fields.is_empty()
            && fields
                .iter()
                .all(|f| f.starts_with("successors[0].terms.window")),
        "{fields:?}"
    );
}

#[tokio::test]
async fn replace_checks_the_version_of_a_grant_and_of_a_pack() {
    let env = env().await;
    let a = Uuid::now_v7();
    let old = issue_one(&env, a, terms()).await;
    let mut req = successors(vec![Successor::new(terms(), None)]);
    req.expected_version = Some(old.version + 1);
    let err = env
        .grants
        .replace(&ctx(a), Target::Grant(old.id), req)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionMismatch), "{err:?}");

    let pack = env
        .grants
        .issue(&ctx(a), a, vec![terms()], true, token())
        .await
        .unwrap()
        .pack
        .unwrap();
    let mut req = successors(vec![Successor::new(terms(), None)]);
    req.expected_version = Some(pack.version + 1);
    let err = env
        .grants
        .replace(&ctx(a), Target::Pack(pack.id), req)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionMismatch), "{err:?}");
    assert_eq!(
        env.grants.get_pack(&ctx(a), pack.id).await.unwrap().version,
        pack.version
    );
}

#[tokio::test]
async fn replace_refuses_a_sub_grant_and_a_target_without_live_grants() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let root = issue_one(&env, a, delegable(1)).await;
    let sub = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![root.id]))
        .await
        .unwrap();
    let sub = crate::test_support::sub_grants(&env, &sub).await.remove(0);
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(sub.id),
            successors(vec![Successor::new(terms(), None)]),
        )
        .await
        .unwrap_err();
    crate::test_support::transition(&err);

    let ended = issue_one(&env, a, terms()).await;
    env.grants
        .grant_verb(&ctx(a), ended.id, Verb::Revoke, token(), None)
        .await
        .unwrap();
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(ended.id),
            successors(vec![Successor::new(terms(), None)]),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::InvalidTransition(m) if m.contains("nothing left to replace")),
        "{err:?}"
    );
}

#[tokio::test]
async fn replace_pack_refuses_successors_that_differ_in_acceptance() {
    let env = env().await;
    let a = Uuid::now_v7();
    let pack = env
        .grants
        .issue(&ctx(a), a, vec![terms(), terms()], true, token())
        .await
        .unwrap()
        .pack
        .unwrap();
    let mut waiting = terms();
    waiting.permissions.requires_acceptance = true;
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Pack(pack.id),
            successors(vec![
                Successor::new(terms(), None),
                Successor::new(waiting, None),
            ]),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::InvalidArgument(v)
            if v.iter().any(|v| v.reason == license_enforcement_sdk::error::reason::MIXED_ACCEPTANCE)),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_locked_rows_confirm_the_checks_before_admission() {
    let env = env().await;
    let a = Uuid::now_v7();
    let g = issue_one(&env, a, terms()).await;
    let internal = |r: Result<(), DomainError>| matches!(r, Err(DomainError::Internal(_)));
    assert!(super::still_replaced(Target::Grant(g.id), std::slice::from_ref(&g), &[g.id]).is_ok());

    let mut sub = g.clone();
    sub.parent_grant_id = Some(GrantId(Uuid::now_v7()));
    assert!(internal(super::still_replaced(
        Target::Grant(g.id),
        &[sub],
        &[g.id]
    )));
    let mut member = g.clone();
    member.pack_id = Some(license_enforcement_sdk::PackId(Uuid::now_v7()));
    assert!(internal(super::still_replaced(
        Target::Grant(g.id),
        &[member],
        &[g.id]
    )));

    // A pack whose members differ from the checked ones.
    let pack = license_enforcement_sdk::PackId(Uuid::now_v7());
    let other = issue_one(&env, a, terms()).await;
    assert!(internal(super::still_replaced(
        Target::Pack(pack),
        &[g.clone(), other],
        &[g.id]
    )));
}

#[tokio::test]
async fn a_replaced_grant_does_not_name_its_end_to_the_moved_subject() {
    let env = env().await;
    let a = Uuid::now_v7();
    let old = issue_one(&env, a, terms()).await;
    env.grants
        .change_binding(
            &ctx(a),
            old.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let done = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(old.id),
            successors(vec![Successor::new(terms(), Some(old.id))]),
        )
        .await
        .unwrap();
    let successor = done.grants[0].id;
    env.grants
        .change_binding(&ctx(a), successor, BindingChange::Unbind, token(), None)
        .await
        .unwrap();
    env.grants
        .grant_verb(&ctx(a), successor, Verb::Revoke, token(), None)
        .await
        .unwrap();
    let d = env
        .check
        .check(&request(USER, "alice", a), now())
        .await
        .unwrap();
    assert_eq!(
        d.reason,
        Some(license_enforcement_sdk::DenialReason::NoGrantForResource),
        "{d:?}"
    );
}

#[tokio::test]
async fn a_target_without_live_grants_does_not_reach_the_plugin() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let calls = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&calls);
    let plugin = crate::test_support::TestPlugin::admitting(move |_, _| {
        counted.fetch_add(1, Ordering::Relaxed);
    });
    let env = crate::test_support::env_with_plugin(
        Arc::new(crate::test_support::AllowAuthZ),
        Arc::new(plugin),
    )
    .await;
    let a = Uuid::now_v7();
    let ended = issue_one(&env, a, terms()).await;
    env.grants
        .grant_verb(&ctx(a), ended.id, Verb::Revoke, token(), None)
        .await
        .unwrap();
    let before = calls.load(Ordering::Relaxed);
    let err = env
        .grants
        .replace(
            &ctx(a),
            Target::Grant(ended.id),
            successors(vec![Successor::new(terms(), None)]),
        )
        .await
        .unwrap_err();
    crate::test_support::transition(&err);
    assert_eq!(calls.load(Ordering::Relaxed), before);
}

/// Each rule of a successor refuses alone: the binding is named without a
/// successor, with the delegation that blocks it.
#[tokio::test]
async fn a_successor_that_cannot_take_the_binding_names_it() {
    let env = env().await;
    let a = Uuid::now_v7();
    let bound = issue_one(&env, a, terms()).await;
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
    let mut other_resource = terms();
    other_resource.resource.id = Some("seat-1".to_owned());
    let mut narrower = terms();
    narrower.subject_type = gts::GtsTypeId::new(crate::test_support::ADMIN);
    for (case, t) in [
        ("other resource", other_resource),
        ("narrower subject type", narrower),
    ] {
        let err = env
            .grants
            .replace(
                &ctx(a),
                Target::Grant(bound.id),
                successors(vec![Successor::new(t, Some(bound.id))]),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::BindingsWithoutSuccessor(ids) if ids == &[(bound.id, None)]),
            "{case}: {err:?}"
        );
    }

    let b = Uuid::now_v7();
    let root = issue_one(&env, a, delegable(1)).await;
    let sub = delegated(&env, a, b, root.id).await;
    let conn = env.db.conn().unwrap();
    let delegation = env
        .store
        .grant(&conn, &AccessScope::allow_all(), sub, false)
        .await
        .unwrap()
        .unwrap()
        .delegation_id;
    for (case, t) in [("deeper", delegable(2))] {
        let err = env
            .grants
            .replace(
                &ctx(a),
                Target::Grant(root.id),
                successors(vec![Successor::new(t, Some(root.id))]),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::BindingsWithoutSuccessor(ids) if ids == &[(root.id, delegation)]),
            "{case}: {err:?}"
        );
    }
}
