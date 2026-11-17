use std::sync::Arc;

use gts::GtsTypeId;
use license_enforcement_sdk::models::{GrantTerms, SubjectRef};
use time::Duration;
use uuid::Uuid;

use super::*;
use crate::domain::grants::BindingChange;
use crate::test_support::{
    ADMIN, Env, RESOURCE, SERVICE, Sides, TestPlugin, USER, ctx, env, input, now, request,
    tenant_subject, terms, token, user,
};

fn bound_to(subject: SubjectRef) -> GrantTerms {
    let mut t = terms();
    t.fixed_subject = Some(input(subject));
    t
}

async fn reason(env: &Env, req: &LicenseCheckRequest, at: OffsetDateTime) -> Option<DenialReason> {
    env.check.check(req, at).await.unwrap().reason
}

#[tokio::test]
async fn no_grant_and_unbound_grant() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let req = request(USER, "alice", tenant);
    assert_eq!(
        reason(&env, &req, now()).await,
        Some(DenialReason::NoGrantForResource)
    );

    let g = crate::test_support::issue_one(&env, tenant, terms()).await;
    assert_eq!(
        reason(&env, &req, now()).await,
        Some(DenialReason::SubjectNotBound)
    );

    env.grants
        .change_binding(
            &ctx(tenant),
            g.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let decision = env.check.check(&req, now()).await.unwrap();
    assert_eq!(decision, Decision::granted(g.id, false));
    assert_eq!(
        reason(&env, &request(USER, "bob", tenant), now()).await,
        Some(DenialReason::SubjectNotBound)
    );
}

#[tokio::test]
async fn binding_on_a_base_type_covers_a_derived_subject() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let g = crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    let decision = env
        .check
        .check(&request(ADMIN, "alice", tenant), now())
        .await
        .unwrap();
    assert_eq!(decision.grant_id, Some(g.id));
}

#[tokio::test]
async fn binding_on_a_derived_type_does_not_cover_a_base_subject() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    crate::test_support::issue_one(
        &env,
        tenant,
        bound_to(SubjectRef {
            gts_type: GtsTypeId::new(ADMIN),
            id: "alice".to_owned(),
        }),
    )
    .await;
    assert_eq!(
        reason(&env, &request(USER, "alice", tenant), now()).await,
        Some(DenialReason::SubjectNotBound)
    );
}

#[tokio::test]
async fn evidence_of_another_plugin_does_not_verify() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    let check = CheckService::new(
        Arc::clone(&env.db),
        env.store,
        license_enforcement_sdk::linked_evaluator().unwrap(),
        Arc::new(TestPlugin {
            info: Some(("another", false)),
            ..TestPlugin::default()
        }),
    );
    let decision = check
        .check(&request(USER, "alice", tenant), now())
        .await
        .unwrap();
    assert_eq!(decision.reason, Some(DenialReason::ChainInvalid));
}

#[tokio::test]
async fn evidence_bytes_reach_only_a_plugin_that_verifies_them() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    for verifies in [false, true] {
        // Records the length of the evidence bytes that `verify` gets.
        let seen = Arc::new(std::sync::Mutex::new(None));
        let probe = Arc::clone(&seen);
        let plugin = TestPlugin::verifying(verifies, move |_, evidence| {
            *probe.lock().unwrap() = Some(evidence.bytes.len());
            None
        });
        let check = CheckService::new(
            Arc::clone(&env.db),
            env.store,
            license_enforcement_sdk::linked_evaluator().unwrap(),
            Arc::new(plugin),
        );
        let decision = check.check(&request(USER, "alice", tenant), now()).await;
        // The issue plugin returns no root fields: only a plugin that does not
        // verify may do that.
        if verifies {
            assert!(
                matches!(decision, Err(DomainError::Internal(_))),
                "{decision:?}"
            );
        } else {
            assert!(decision.unwrap().granted);
        }
        let seen = seen.lock().unwrap().unwrap();
        assert_eq!(seen > 0, verifies, "verifies_evidence = {verifies}");
    }
}

#[tokio::test]
async fn binding_on_an_unrelated_type_does_not_match() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let mut t = bound_to(SubjectRef {
        gts_type: GtsTypeId::new(tenant_subject()),
        id: "alice".to_owned(),
    });
    t.subject_type = GtsTypeId::new(tenant_subject());
    crate::test_support::issue_one(&env, tenant, t).await;
    assert_eq!(
        reason(&env, &request(USER, "alice", tenant), now()).await,
        Some(DenialReason::SubjectNotBound)
    );
}

#[tokio::test]
async fn tenant_wide_grant_covers_every_subject() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let mut t = bound_to(SubjectRef {
        gts_type: GtsTypeId::new(tenant_subject()),
        id: tenant.to_string(),
    });
    t.subject_type = GtsTypeId::new(tenant_subject());
    let g = crate::test_support::issue_one(&env, tenant, t).await;
    let decision = env
        .check
        .check(&request(USER, "anyone", tenant), now())
        .await
        .unwrap();
    assert_eq!(decision.grant_id, Some(g.id));
}

#[tokio::test]
async fn grants_that_have_not_started_do_not_take_the_candidate_limit() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let mut not_yet = bound_to(user("alice"));
    not_yet.window.not_before = now() + Duration::days(1);
    crate::test_support::issue_one(&env, tenant, not_yet).await;
    let mut wide = bound_to(SubjectRef {
        gts_type: GtsTypeId::new(tenant_subject()),
        id: tenant.to_string(),
    });
    wide.subject_type = GtsTypeId::new(tenant_subject());
    let wide = crate::test_support::issue_one(&env, tenant, wide).await;
    let req = request(USER, "alice", tenant);

    assert_eq!(
        env.check.check(&req, now()).await.unwrap().grant_id,
        Some(wide.id)
    );
    let limited = CheckService::new(
        Arc::clone(&env.db),
        crate::infra::storage::Store::new(
            toolkit_db::odata::LimitCfg {
                default: 50,
                max: 1000,
            },
            1,
        ),
        license_enforcement_sdk::linked_evaluator().unwrap(),
        license_enforcement_sdk::linked_admission_plugin().unwrap(),
    );
    // `not_yet` matches the first filter, yet the only candidate read is `wide`.
    assert_eq!(
        limited.check(&req, now()).await.unwrap().grant_id,
        Some(wide.id)
    );
    // A grant that has not started still gives the reason when nothing else does.
    let other = Uuid::now_v7();
    let mut not_yet = bound_to(user("alice"));
    not_yet.window.not_before = now() + Duration::days(1);
    crate::test_support::issue_one(&env, other, not_yet).await;
    assert_eq!(
        limited
            .check(&request(USER, "alice", other), now())
            .await
            .unwrap()
            .reason,
        Some(DenialReason::NotYetValid)
    );
}

#[tokio::test]
async fn window_and_grace() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let mut t = bound_to(user("alice"));
    t.window.grace_until = Some(t.window.expires_at + Duration::days(7));
    let expires = t.window.expires_at;
    let not_before = t.window.not_before;
    crate::test_support::issue_one(&env, tenant, t).await;
    let req = request(USER, "alice", tenant);

    assert_eq!(
        reason(&env, &req, not_before - Duration::seconds(1)).await,
        Some(DenialReason::NotYetValid)
    );
    let in_grace = env
        .check
        .check(&req, expires + Duration::days(1))
        .await
        .unwrap();
    assert!(in_grace.granted && in_grace.grace);
    assert_eq!(
        reason(&env, &req, expires + Duration::days(8)).await,
        Some(DenialReason::GraceExpired)
    );
}

/// `SQLite` compares stored times as text; a window in another offset must
/// still be live by the instant, not by its string.
#[tokio::test]
async fn window_in_a_negative_offset_is_live_by_instant() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let minus_five = time::UtcOffset::from_hms(-5, 0, 0).unwrap();
    let mut t = bound_to(user("alice"));
    t.window.not_before = (now() - Duration::hours(1)).to_offset(minus_five);
    t.window.expires_at = (now() + Duration::hours(1)).to_offset(minus_five);
    crate::test_support::issue_one(&env, tenant, t).await;

    let decision = env
        .check
        .check(&request(USER, "alice", tenant), now())
        .await
        .unwrap();
    assert!(decision.granted, "{decision:?}");
}

#[tokio::test]
async fn expired_without_grace() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let t = bound_to(user("alice"));
    let expires = t.window.expires_at;
    crate::test_support::issue_one(&env, tenant, t).await;
    assert_eq!(
        reason(&env, &request(USER, "alice", tenant), expires).await,
        Some(DenialReason::Expired)
    );
}

#[tokio::test]
async fn awaiting_acceptance_denies() {
    use crate::infra::storage::entity::grant;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr};
    use toolkit_db::secure::SecureUpdateExt as _;
    let env = env().await;
    let tenant = Uuid::now_v7();
    // `issue` refuses `requires_acceptance` until `accept` exists.
    let g = crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    grant::Entity::update_many()
        .col_expr(grant::Column::State, Expr::value("awaiting_acceptance"))
        .filter(grant::Column::Id.eq(g.id.0))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&env.db.conn().unwrap())
        .await
        .unwrap();
    assert_eq!(
        reason(&env, &request(USER, "alice", tenant), now()).await,
        Some(DenialReason::AwaitingAcceptance)
    );
}

#[tokio::test]
async fn grants_of_another_tenant_do_not_count() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    crate::test_support::issue_one(&env, a, bound_to(user("alice"))).await;
    assert_eq!(
        reason(&env, &request(USER, "alice", b), now()).await,
        Some(DenialReason::NoGrantForResource)
    );
}

#[test]
fn decision_diagnostics() {
    let id = GrantId(Uuid::nil());
    let granted = Decision::granted(id, true).into_license_decision();
    assert!(granted.granted);
    assert_eq!(granted.diagnostics[DIAG_GRANT_ID], id.to_string());
    assert_eq!(granted.diagnostics[DIAG_GRACE], true);

    let denied = Decision::denied(DenialReason::Terminated, Some(TerminationReason::Revoked))
        .into_license_decision();
    assert!(!denied.granted);
    assert_eq!(denied.diagnostics[DIAG_REASON], "terminated");
    assert_eq!(denied.diagnostics[DIAG_TERMINATION_REASON], "revoked");
}

#[tokio::test]
async fn resolver_adapter_answers_with_a_decision() {
    use license_resolver_sdk::LicenseResolverPluginClient;

    use crate::domain::local_client::ResolverAdapter;

    let env = env().await;
    let tenant = Uuid::now_v7();
    let adapter = ResolverAdapter::new(Arc::clone(&env.check));
    let denied = adapter
        .is_licensed(request(USER, "alice", tenant))
        .await
        .unwrap();
    assert!(!denied.granted);
    assert_eq!(denied.diagnostics[DIAG_REASON], "no_grant_for_resource");

    let g = crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    let granted = adapter
        .is_licensed(request(USER, "alice", tenant))
        .await
        .unwrap();
    assert!(granted.granted);
    assert_eq!(granted.diagnostics[DIAG_GRANT_ID], g.id.to_string());
}

fn check_with(
    env: &Env,
    v: license_enforcement_sdk::Verification,
) -> CheckService<crate::infra::storage::Store> {
    CheckService::new(
        Arc::clone(&env.db),
        env.store,
        license_enforcement_sdk::linked_evaluator().unwrap(),
        Arc::new(TestPlugin::verifying(true, move |_, _| Some(v.clone()))),
    )
}

#[tokio::test]
async fn verify_outcomes_map_to_decisions() {
    use license_enforcement_sdk::{PluginDenial, RootFields, Verification};
    let env = env().await;
    let tenant = Uuid::now_v7();
    crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    let req = request(USER, "alice", tenant);
    for (outcome, want) in [
        (
            VerifyOutcome::Revoked(RevocationState::Suspended),
            DenialReason::Suspended,
        ),
        (
            VerifyOutcome::Revoked(RevocationState::Terminated),
            DenialReason::Terminated,
        ),
        (
            VerifyOutcome::Denied(PluginDenial::Stale),
            DenialReason::Stale,
        ),
    ] {
        let decision = check_with(&env, Verification::new(outcome.clone()))
            .check(&req, now())
            .await
            .unwrap();
        assert_eq!(decision.reason, Some(want), "{outcome:?}");
        assert!(!decision.granted);
    }
    let root_fields = VerifyOutcome::Verified(Some(Box::new(RootFields::new(tenant, terms()))));
    let decision = check_with(&env, Verification::new(root_fields))
        .check(&req, now())
        .await
        .unwrap();
    assert!(decision.granted, "{decision:?}");
}

#[tokio::test]
async fn material_written_by_verify_is_stored_and_a_failed_write_keeps_the_decision() {
    use license_enforcement_sdk::{Expect, MaterialWrite, RootFields, Verification};
    let env = env().await;
    let tenant = Uuid::now_v7();
    crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    let req = request(USER, "alice", tenant);
    let id = le_issue_admission_plugin::IssueAdmissionPlugin.info().id;
    let verified = || {
        Verification::new(VerifyOutcome::Verified(Some(Box::new(RootFields::new(
            tenant,
            terms(),
        )))))
    };
    let stored = || async {
        let conn = env.db.conn().unwrap();
        env.store.material(&conn, id, "mark").await.unwrap()
    };

    let mut v = verified();
    v.material = vec![MaterialWrite::put("mark", vec![1])];
    assert!(
        check_with(&env, v)
            .check(&req, now())
            .await
            .unwrap()
            .granted
    );
    assert_eq!(stored().await, Some(vec![1]));

    let mut v = verified();
    v.material = vec![MaterialWrite::put("mark", vec![2]).expecting(Expect::Equals(vec![9]))];
    assert!(
        check_with(&env, v)
            .check(&req, now())
            .await
            .unwrap()
            .granted
    );
    assert_eq!(stored().await, Some(vec![1]));
}

#[tokio::test]
async fn a_terminated_grant_names_its_termination() {
    use crate::infra::storage::entity::grant;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr};
    use toolkit_db::secure::SecureUpdateExt as _;
    let env = env().await;
    let tenant = Uuid::now_v7();
    let g = crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    let conn = env.db.conn().unwrap();
    grant::Entity::update_many()
        .col_expr(grant::Column::State, Expr::value("terminated"))
        .col_expr(grant::Column::TerminationReason, Expr::value("revoked"))
        .filter(grant::Column::Id.eq(g.id.0))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .unwrap();
    let decision = env
        .check
        .check(&request(USER, "alice", tenant), now())
        .await
        .unwrap();
    assert_eq!(decision.reason, Some(DenialReason::Terminated));
    assert_eq!(
        decision.termination_reason,
        Some(license_enforcement_sdk::models::TerminationReason::Revoked)
    );
}

#[tokio::test]
async fn a_nul_in_the_request_denies_without_the_store() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    assert_eq!(
        reason(&env, &request(USER, "ali\0ce", tenant), now()).await,
        Some(DenialReason::NoGrantForResource)
    );
}

#[tokio::test]
async fn root_fields_bound_a_sub_grant_and_its_chain() {
    use crate::infra::storage::entity::grant::Column;

    use license_enforcement_sdk::{RootFields, Verification};
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let root = crate::test_support::issue_one(&env, a, crate::test_support::delegable(1)).await;
    let sub = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![root.id]))
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
    let req = request(USER, "bob", b);
    let w = *root.validity.window();
    let with = |mut t: GrantTerms| {
        t.window = license_enforcement_sdk::models::WindowInput {
            not_before: w.not_before,
            expires_at: w.expires_at,
            grace_until: w.grace_until,
        };
        Verification::new(VerifyOutcome::Verified(Some(Box::new(RootFields::new(
            a, t,
        )))))
    };

    let ok = check_with(&env, with(crate::test_support::delegable(1)))
        .check(&req, now())
        .await
        .unwrap();
    assert!(ok.granted, "{ok:?}");

    // Root fields of another holder than the stored root.
    let mut t = crate::test_support::delegable(1);
    t.window = license_enforcement_sdk::models::WindowInput {
        not_before: w.not_before,
        expires_at: w.expires_at,
        grace_until: w.grace_until,
    };
    let stranger = Verification::new(VerifyOutcome::Verified(Some(Box::new(RootFields::new(
        Uuid::now_v7(),
        t,
    )))));
    let foreign = check_with(&env, stranger).check(&req, now()).await.unwrap();
    assert_eq!(foreign.reason, Some(DenialReason::ChainInvalid));

    // The evidence allows no delegation: the stored chain is wider.
    let flat = check_with(&env, with(terms()))
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(flat.reason, Some(DenialReason::ChainInvalid));

    // The evidence fixes the binding of the root: it cannot be delegated.
    let mut fixed = crate::test_support::delegable(1);
    fixed.fixed_subject = Some(input(user("alice")));
    let fixed = check_with(&env, with(fixed))
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(fixed.reason, Some(DenialReason::ChainInvalid));

    // The evidence permits no bind: the bound sub-grant is wider.
    let mut no_bind = crate::test_support::delegable(1);
    no_bind.permissions.may_bind = false;
    let no_bind = check_with(&env, with(no_bind))
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(no_bind.reason, Some(DenialReason::ChainInvalid));

    // The evidence requires acceptance; the stored sub-grant dropped it.
    let mut gated = crate::test_support::delegable(1);
    gated.permissions.requires_acceptance = true;
    let gated = check_with(&env, with(gated))
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(gated.reason, Some(DenialReason::ChainInvalid));

    // The sub-grant row names other evidence than its root.
    let other = crate::test_support::issue_one(&env, a, terms()).await;
    crate::test_support::edit_row(&env, sub.id, Column::EvidenceId, other.evidence_id.0).await;
    let foreign = check_with(&env, with(crate::test_support::delegable(1)))
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(foreign.reason, Some(DenialReason::ChainInvalid));
    crate::test_support::edit_row(&env, sub.id, Column::EvidenceId, root.evidence_id.0).await;

    // Only a key column of the sub-grant is edited: the request names a
    // resource type or a subject type that the root fields do not.
    let key = crate::domain::types::type_key;
    let other_type = "gts.cf.core.lic.res.v1~test.le.app.other.v1~";
    crate::test_support::edit_row(&env, sub.id, Column::ResourceTypeKey, key(other_type)).await;
    let mut other_req = request(USER, "bob", b);
    other_req.resource.gts_type = GtsTypeId::new(other_type);
    let edited_key = check_with(&env, with(crate::test_support::delegable(1)))
        .check(&other_req, now())
        .await
        .unwrap();
    assert_eq!(
        edited_key.reason,
        Some(DenialReason::ChainInvalid),
        "{edited_key:?}"
    );
    crate::test_support::edit_row(&env, sub.id, Column::ResourceTypeKey, key(RESOURCE)).await;
    crate::test_support::edit_row(&env, sub.id, Column::BindingSubjectTypeKey, key(SERVICE)).await;
    let edited_key = check_with(&env, with(crate::test_support::delegable(1)))
        .check(&request(SERVICE, "bob", b), now())
        .await
        .unwrap();
    assert_eq!(
        edited_key.reason,
        Some(DenialReason::ChainInvalid),
        "{edited_key:?}"
    );
    crate::test_support::edit_row(&env, sub.id, Column::BindingSubjectTypeKey, key(USER)).await;

    // A narrower window than the root: the sub-grant has the window of its root.
    let earlier = w.expires_at - Duration::days(1);
    crate::test_support::edit_row(&env, sub.id, Column::ExpiresAt, earlier).await;
    let narrowed = check_with(&env, with(crate::test_support::delegable(1)))
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(
        narrowed.reason,
        Some(DenialReason::ChainInvalid),
        "{narrowed:?}"
    );
    crate::test_support::edit_row(&env, sub.id, Column::ExpiresAt, w.expires_at).await;

    // An edited row binds the sub-grant to a subject type that the evidence
    // does not cover: the tenant as a whole.
    let conn = env.db.conn().unwrap();
    let mut edited = env
        .store
        .grant(
            &conn,
            &toolkit_security::AccessScope::allow_all(),
            sub.id,
            false,
        )
        .await
        .unwrap()
        .unwrap();
    edited.binding = Binding::subject(SubjectRef {
        gts_type: GtsTypeId::new(tenant_subject()),
        id: b.to_string(),
    });
    edited.version += 1;
    env.store
        .update_grant(
            &conn,
            &toolkit_security::AccessScope::for_tenant(b),
            &edited,
            now(),
        )
        .await
        .unwrap();
    let wide = check_with(&env, with(crate::test_support::delegable(1)))
        .check(&request(tenant_subject(), &b.to_string(), b), now())
        .await
        .unwrap();
    assert_eq!(wide.reason, Some(DenialReason::ChainInvalid), "{wide:?}");
}

#[tokio::test]
async fn root_fields_of_another_holder_do_not_grant_a_root() {
    use license_enforcement_sdk::{RootFields, Verification};
    let env = env().await;
    let tenant = Uuid::now_v7();
    crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    let fields = RootFields::new(Uuid::now_v7(), bound_to(user("alice")));
    let d = check_with(
        &env,
        Verification::new(VerifyOutcome::Verified(Some(Box::new(fields)))),
    )
    .check(&request(USER, "alice", tenant), now())
    .await
    .unwrap();
    assert_eq!(d.reason, Some(DenialReason::ChainInvalid));
}

/// The check of `env` with a plugin that signed `roots` of `holder` with
/// `terms`, in the window that each root was stored with.
fn signed(
    env: &Env,
    holder: Uuid,
    roots: &[(&Grant, GrantTerms)],
) -> CheckService<crate::infra::storage::Store> {
    let fields: std::collections::HashMap<_, _> = roots
        .iter()
        .map(|(root, t)| {
            let w = *root.validity.window();
            let mut t = t.clone();
            t.window = license_enforcement_sdk::models::WindowInput {
                not_before: w.not_before,
                expires_at: w.expires_at,
                grace_until: w.grace_until,
            };
            (root.id, t)
        })
        .collect();
    CheckService::new(
        Arc::clone(&env.db),
        env.store,
        license_enforcement_sdk::linked_evaluator().unwrap(),
        // Verifies evidence: it knows the fields of the roots that its evidence
        // names, and no other root.
        Arc::new(TestPlugin::verifying(true, move |id, _| {
            use license_enforcement_sdk::{PluginDenial, RootFields, Verification};
            Some(Verification::new(match fields.get(&id) {
                Some(t) => {
                    VerifyOutcome::Verified(Some(Box::new(RootFields::new(holder, t.clone()))))
                }
                None => VerifyOutcome::Denied(PluginDenial::Duplicated),
            }))
        })),
    )
}

/// ADR-0002: an edit of a stored root row leaves the decision as it was or
/// denies.
#[tokio::test]
async fn edits_of_a_root_row_never_widen_the_decision() {
    use crate::infra::storage::entity::grant::Column;
    use crate::test_support::edit_row;
    let env = env().await;
    let tenant = Uuid::now_v7();
    let t = bound_to(user("alice"));
    let root = crate::test_support::issue_one(&env, tenant, t.clone()).await;
    let check = signed(&env, tenant, &[(&root, t)]);
    let granted = |req: LicenseCheckRequest, at: OffsetDateTime| {
        let check = &check;
        async move { check.check(&req, at).await.unwrap().granted }
    };
    assert!(granted(request(USER, "alice", tenant), now()).await);

    // Another holder.
    let other = Uuid::now_v7();
    edit_row(&env, root.id, Column::HolderTenantId, other).await;
    assert!(!granted(request(USER, "alice", other), now()).await);
    edit_row(&env, root.id, Column::HolderTenantId, tenant).await;

    // Another resource type.
    let other_type = "gts.cf.core.lic.res.v1~test.le.app.other.v1~";
    edit_row(&env, root.id, Column::ResourceType, other_type.to_owned()).await;
    edit_row(
        &env,
        root.id,
        Column::ResourceTypeKey,
        crate::domain::types::type_key(other_type),
    )
    .await;
    let mut req = request(USER, "alice", tenant);
    req.resource.gts_type = GtsTypeId::new(other_type);
    assert!(!granted(req, now()).await);
    edit_row(&env, root.id, Column::ResourceType, RESOURCE.to_owned()).await;
    edit_row(
        &env,
        root.id,
        Column::ResourceTypeKey,
        crate::domain::types::type_key(RESOURCE),
    )
    .await;
    assert!(granted(request(USER, "alice", tenant), now()).await);

    // A longer window.
    let later = root.validity.window().expires_at + Duration::days(365);
    edit_row(&env, root.id, Column::ExpiresAt, later).await;
    edit_row(&env, root.id, Column::EffectiveEnd, later).await;
    let after = root.validity.window().expires_at + Duration::days(1);
    assert!(!granted(request(USER, "alice", tenant), after).await);

    // Only the subject type key of the fixed binding.
    let key = crate::domain::types::type_key;
    edit_row(&env, root.id, Column::BindingSubjectTypeKey, key(SERVICE)).await;
    assert!(!granted(request(SERVICE, "alice", tenant), now()).await);
    edit_row(&env, root.id, Column::BindingSubjectTypeKey, key(USER)).await;
    assert!(granted(request(USER, "alice", tenant), now()).await);

    // Another subject in the fixed binding.
    edit_row(&env, root.id, Column::BindingSubjectId, "bob".to_owned()).await;
    assert!(!granted(request(USER, "bob", tenant), now()).await);
}

/// ADR-0002: a root row that no evidence names denies.
#[tokio::test]
async fn a_fabricated_root_row_denies() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let t = bound_to(user("alice"));
    let root = crate::test_support::issue_one(&env, tenant, t.clone()).await;
    let mut fake = root.clone();
    fake.id = license_enforcement_sdk::GrantId(Uuid::now_v7());
    fake.binding = Binding::subject(user("mallory"));
    crate::test_support::insert_sub_grant(
        &env,
        &toolkit_security::AccessScope::for_tenant(tenant),
        &fake,
        fake.id,
    )
    .await
    .unwrap();
    let check = signed(&env, tenant, &[(&root, t)]);
    let d = check
        .check(&request(USER, "mallory", tenant), now())
        .await
        .unwrap();
    assert!(!d.granted, "{d:?}");
}

/// ADR-0002: the store holds one row per grant id and one live child per
/// parent, so a second root row or a copied sub-grant row is refused.
#[tokio::test]
async fn the_store_refuses_a_second_root_row_and_a_copied_sub_grant() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let root = crate::test_support::issue_one(&env, a, crate::test_support::delegable(1)).await;
    let scope = toolkit_security::AccessScope::for_tenant(a);
    assert!(
        crate::test_support::insert_sub_grant(&env, &scope, &root, root.id)
            .await
            .is_err()
    );
    let sub = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![root.id]))
        .await
        .unwrap();
    let sub = crate::test_support::sub_grants(&env, &sub).await.remove(0);
    let mut copy = sub.clone();
    copy.id = license_enforcement_sdk::GrantId(Uuid::now_v7());
    assert!(
        crate::test_support::insert_sub_grant(
            &env,
            &toolkit_security::AccessScope::for_tenant(b),
            &copy,
            root.id
        )
        .await
        .is_err()
    );
}

/// A link whose parent does not name it is a copy: the check denies it as
/// duplicated. Root fields bound the depth of a chain two links deep.
#[tokio::test]
async fn root_fields_check_a_deep_chain_link_by_link() {
    use crate::infra::storage::entity::grant::Column;

    let env = env().await;
    let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    env.tenants.link(b, c);
    let terms2 = crate::test_support::delegable(2);
    let root = crate::test_support::issue_one(&env, a, terms2.clone()).await;
    let delegate = |grantor, recipient, id| {
        let env = &env;
        async move {
            env.grants
                .delegate(
                    &ctx(grantor),
                    crate::test_support::by_ids(recipient, vec![id]),
                )
                .await
                .unwrap()
                .sub(0)
        }
    };
    let s1 = delegate(a, b, root.id).await;
    let s2 = delegate(b, c, s1).await;
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
    let req = request(USER, "carol", c);

    let ok = signed(&env, a, &[(&root, terms2)])
        .check(&req, now())
        .await
        .unwrap();
    assert!(ok.granted, "{ok:?}");
    // The evidence allows one level: the chain is too deep.
    let shallow = signed(&env, a, &[(&root, crate::test_support::delegable(1))])
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(shallow.reason, Some(DenialReason::ChainInvalid));

    let deep = || signed(&env, a, &[(&root, crate::test_support::delegable(2))]);
    // A terminated link in the middle of the chain.
    crate::test_support::edit_row(&env, s1, Column::State, "terminated".to_owned()).await;
    let ended = deep().check(&req, now()).await.unwrap();
    assert_eq!(ended.reason, Some(DenialReason::ChainInvalid));
    crate::test_support::edit_row(&env, s1, Column::State, "active".to_owned()).await;
    // S1 no longer leads to the root.
    crate::test_support::edit_row(&env, s1, Column::ParentGrantId, Uuid::now_v7()).await;
    let unrooted = deep().check(&req, now()).await.unwrap();
    assert_eq!(unrooted.reason, Some(DenialReason::ChainInvalid));
    crate::test_support::edit_row(&env, s1, Column::ParentGrantId, root.id.0).await;
    assert!(deep().check(&req, now()).await.unwrap().granted);

    // S1 no longer names S2: S2 is a copy.
    crate::test_support::edit_row(&env, s1, Column::BindingGrantId, Uuid::now_v7()).await;
    let copied = signed(&env, a, &[(&root, crate::test_support::delegable(2))])
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(copied.reason, Some(DenialReason::Duplicated));
}

/// Step 3 reads the evidence and trees of every passing candidate at once;
/// each candidate is held against its own tree.
#[tokio::test]
async fn root_fields_check_each_of_several_candidates_against_its_own_tree() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let delegable = crate::test_support::delegable(1);
    let r1 = crate::test_support::issue_one(&env, a, delegable.clone()).await;
    let r2 = crate::test_support::issue_one(&env, a, delegable.clone()).await;
    let d = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![r1.id, r2.id]))
        .await
        .unwrap();
    let subs = crate::test_support::sub_grants(&env, &d).await;
    for s in &subs {
        env.grants
            .change_binding(
                &ctx(b),
                s.id,
                BindingChange::Bind(user("bob")),
                token(),
                None,
            )
            .await
            .unwrap();
    }
    let sub_of = |root: &Grant| {
        subs.iter()
            .find(|s| s.parent_grant_id == Some(root.id))
            .map(|s| s.id)
    };
    let req = request(USER, "bob", b);
    for (signed_root, other) in [(&r1, &r2), (&r2, &r1)] {
        let decision = signed(&env, a, &[(signed_root, delegable.clone())])
            .check(&req, now())
            .await
            .unwrap();
        assert!(decision.granted, "{decision:?}");
        assert_ne!(decision.grant_id, sub_of(other));
        assert_eq!(decision.grant_id, sub_of(signed_root));
    }
}

/// ADR-0002 without the indexes that hold I3: a second live child of a link
/// is a copy, and the check denies it as duplicated.
#[tokio::test]
async fn the_check_denies_a_second_live_child_that_the_store_let_in() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let delegable = crate::test_support::delegable(1);
    let root = crate::test_support::issue_one(&env, a, delegable.clone()).await;
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
    assert!(
        signed(&env, a, &[(&root, delegable.clone())])
            .check(&req, now())
            .await
            .unwrap()
            .granted
    );

    crate::test_support::operator_sql(
        &env,
        "DROP INDEX uq_license_enforcement__grants_one_live_child",
        "DROP INDEX uq_license_enforcement__grants_one_live_child",
    )
    .await;
    let conn = env.db.conn().unwrap();
    let mut copy = env
        .store
        .grant(
            &conn,
            &toolkit_security::AccessScope::allow_all(),
            s1,
            false,
        )
        .await
        .unwrap()
        .unwrap();
    copy.id = license_enforcement_sdk::GrantId(Uuid::now_v7());
    crate::test_support::insert_sub_grant(
        &env,
        &toolkit_security::AccessScope::for_tenant(b),
        &copy,
        root.id,
    )
    .await
    .unwrap();
    let decision = signed(&env, a, &[(&root, delegable)])
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(
        decision.reason,
        Some(DenialReason::Duplicated),
        "{decision:?}"
    );
}

/// ADR-0002 without the primary key: a second live row of a signed root is a
/// copy, and the check denies it as duplicated.
#[tokio::test]
async fn the_check_denies_a_second_row_of_a_root() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let t = crate::test_support::terms();
    let root = crate::test_support::issue_one(&env, tenant, t.clone()).await;
    env.grants
        .change_binding(
            &ctx(tenant),
            root.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let req = request(USER, "alice", tenant);
    assert!(
        signed(&env, tenant, &[(&root, t.clone())])
            .check(&req, now())
            .await
            .unwrap()
            .granted
    );

    // The database of this test holds this one root.
    crate::test_support::operator_sql(
        &env,
        "ALTER TABLE license_enforcement__grants DROP CONSTRAINT license_enforcement__grants_pkey; \
         INSERT INTO license_enforcement__grants SELECT * FROM license_enforcement__grants",
        "ALTER TABLE license_enforcement__grants RENAME TO grants_with_key; \
         CREATE TABLE license_enforcement__grants AS SELECT * FROM grants_with_key; \
         DROP TABLE grants_with_key; \
         INSERT INTO license_enforcement__grants SELECT * FROM license_enforcement__grants",
    )
    .await;
    let decision = signed(&env, tenant, &[(&root, t)])
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(
        decision.reason,
        Some(DenialReason::Duplicated),
        "{decision:?}"
    );
}

/// ADR-0002 without the primary key: a copy of a sub-grant row with the same id,
/// without a parent and bound to another subject, denies as duplicated; so does
/// the original.
#[tokio::test]
async fn the_check_denies_a_copy_of_a_sub_grant_row() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let delegable = crate::test_support::delegable(1);
    let root = crate::test_support::issue_one(&env, a, delegable.clone()).await;
    let d = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![root.id]))
        .await
        .unwrap();
    let sub = crate::test_support::sub_grants(&env, &d).await.remove(0);
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
    let check = signed(&env, a, &[(&root, delegable)]);
    assert!(
        check
            .check(&request(USER, "bob", b), now())
            .await
            .unwrap()
            .granted
    );

    // The copy keeps the id and the root; the original is written again after
    // it, so a read by id finds the original last.
    crate::test_support::operator_sql(
        &env,
        "ALTER TABLE license_enforcement__grants DROP CONSTRAINT license_enforcement__grants_pkey; \
         CREATE TABLE le_original AS SELECT * FROM license_enforcement__grants WHERE binding_subject_id = 'bob'; \
         UPDATE license_enforcement__grants SET binding_subject_id = 'mallory', parent_grant_id = NULL \
         WHERE binding_subject_id = 'bob'; \
         INSERT INTO license_enforcement__grants SELECT * FROM le_original; \
         DROP TABLE le_original",
        "ALTER TABLE license_enforcement__grants RENAME TO grants_with_key; \
         CREATE TABLE license_enforcement__grants AS SELECT * FROM grants_with_key; \
         DROP TABLE grants_with_key; \
         CREATE TABLE le_original AS SELECT * FROM license_enforcement__grants WHERE binding_subject_id = 'bob'; \
         UPDATE license_enforcement__grants SET binding_subject_id = 'mallory', parent_grant_id = NULL \
         WHERE binding_subject_id = 'bob'; \
         INSERT INTO license_enforcement__grants SELECT * FROM le_original; \
         DROP TABLE le_original",
    )
    .await;
    // Whichever copy the store reads, both subjects get the same reason.
    for subject in ["mallory", "bob"] {
        let decision = check
            .check(&request(USER, subject, b), now())
            .await
            .unwrap();
        assert_eq!(
            decision.reason,
            Some(DenialReason::Duplicated),
            "{subject}: {decision:?}"
        );
    }
}

/// A root that its holder bound: the signed fields must permit bind and cover
/// the subject type of the binding.
#[tokio::test]
async fn root_fields_check_a_binding_made_by_the_holder() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let t = crate::test_support::terms();
    let root = crate::test_support::issue_one(&env, tenant, t.clone()).await;
    env.grants
        .change_binding(
            &ctx(tenant),
            root.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let req = request(USER, "alice", tenant);
    let decide = |terms: GrantTerms| {
        let (env, req, root) = (&env, &req, &root);
        async move {
            signed(env, tenant, &[(root, terms)])
                .check(req, now())
                .await
                .unwrap()
        }
    };
    assert!(decide(t.clone()).await.granted);

    let mut no_bind = t.clone();
    no_bind.permissions.may_bind = false;
    assert_eq!(
        decide(no_bind).await.reason,
        Some(DenialReason::ChainInvalid)
    );

    let mut other_type = t;
    other_type.subject_type = GtsTypeId::new(crate::test_support::SERVICE);
    assert_eq!(
        decide(other_type).await.reason,
        Some(DenialReason::ChainInvalid)
    );
}

/// A missing evidence row denies its own candidate, not the whole check.
#[tokio::test]
async fn missing_evidence_denies_only_its_candidate() {
    use crate::infra::storage::entity::evidence;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureDeleteExt as _;
    let env = env().await;
    let tenant = Uuid::now_v7();
    let lost = crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    let conn = env.db.conn().unwrap();
    evidence::Entity::delete_many()
        .filter(evidence::Column::Id.eq(lost.evidence_id.0))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .unwrap();
    let req = request(USER, "alice", tenant);
    let alone = env.check.check(&req, now()).await.unwrap();
    assert_eq!(alone.reason, Some(DenialReason::ChainInvalid));

    crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    assert!(env.check.check(&req, now()).await.unwrap().granted);
}

/// A candidate whose root row is missing denies alone; another candidate
/// still grants.
#[tokio::test]
async fn a_missing_root_denies_only_its_candidate() {
    use crate::infra::storage::entity::grant::Column;
    let env = env().await;
    let tenant = Uuid::now_v7();
    let lost = crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    crate::test_support::edit_row(&env, lost.id, Column::RootGrantId, Uuid::now_v7()).await;
    let req = request(USER, "alice", tenant);
    let alone = env.check.check(&req, now()).await.unwrap();
    assert_eq!(alone.reason, Some(DenialReason::ChainInvalid));

    crate::test_support::issue_one(&env, tenant, bound_to(user("alice"))).await;
    assert!(env.check.check(&req, now()).await.unwrap().granted);
}

/// ADR-0002: edits of the links and the instance of a root row leave the
/// decision as it was or deny.
#[tokio::test]
async fn edits_of_root_links_never_widen_the_decision() {
    use crate::infra::storage::entity::grant::Column;
    use crate::test_support::edit_row;
    let env = env().await;
    let tenant = Uuid::now_v7();
    let mut t = bound_to(user("alice"));
    t.resource.id = Some("seat-1".to_owned());
    let root = crate::test_support::issue_one(&env, tenant, t.clone()).await;
    let check = signed(&env, tenant, &[(&root, t)]);
    let on = |seat: &str| {
        let mut req = request(USER, "alice", tenant);
        req.resource.id = Some(seat.to_owned());
        req
    };
    let granted = |req: LicenseCheckRequest| {
        let check = &check;
        async move { check.check(&req, now()).await.is_ok_and(|d| d.granted) }
    };
    assert!(granted(on("seat-1")).await);
    assert!(!granted(on("seat-2")).await);

    // Any instance of the resource.
    edit_row(&env, root.id, Column::ResourceId, Option::<String>::None).await;
    assert!(!granted(on("seat-2")).await);
    edit_row(&env, root.id, Column::ResourceId, "seat-1".to_owned()).await;

    // Another evidence, or another root.
    for column in [Column::EvidenceId, Column::RootGrantId] {
        edit_row(&env, root.id, column, Uuid::now_v7()).await;
        assert!(!granted(on("seat-1")).await, "{column:?}");
        let back = match column {
            Column::EvidenceId => root.evidence_id.0,
            _ => root.id.0,
        };
        edit_row(&env, root.id, column, back).await;
    }
    assert!(granted(on("seat-1")).await);
}

/// ADR-0002: wider permissions in a root row give a sub-grant or a binding
/// nothing that the evidence does not permit.
#[tokio::test]
async fn edits_of_root_permissions_never_widen_the_decision() {
    use crate::infra::storage::entity::grant::Column;
    use crate::test_support::edit_row;
    let env = env().await;

    // A deeper delegation than the evidence permits.
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let flat = terms();
    let root = crate::test_support::issue_one(&env, a, flat.clone()).await;
    edit_row(&env, root.id, Column::MaxDepth, 1).await;
    let sub = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);
    env.grants
        .change_binding(
            &ctx(b),
            sub,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let d = signed(&env, a, &[(&root, flat)])
        .check(&request(USER, "alice", b), now())
        .await
        .unwrap();
    assert!(!d.granted, "{d:?}");

    // A bind that the evidence does not permit.
    let c = Uuid::now_v7();
    let mut no_bind = terms();
    no_bind.permissions.may_bind = false;
    let root = crate::test_support::issue_one(&env, c, no_bind.clone()).await;
    edit_row(&env, root.id, Column::MayBind, true).await;
    env.grants
        .change_binding(
            &ctx(c),
            root.id,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let d = signed(&env, c, &[(&root, no_bind)])
        .check(&request(USER, "alice", c), now())
        .await
        .unwrap();
    assert!(!d.granted, "{d:?}");
}

/// Evidence whose root carries recipient rules admits no sub-grant: the rules
/// are not evaluated, as `delegate` and `replace` refuse them.
#[tokio::test]
async fn evidence_with_recipient_rules_admits_no_sub_grant() {
    let env = env().await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    env.tenants.link(a, b);
    let t = crate::test_support::delegable(1);
    let root = crate::test_support::issue_one(&env, a, t.clone()).await;
    let sub = env
        .grants
        .delegate(&ctx(a), crate::test_support::by_ids(b, vec![root.id]))
        .await
        .unwrap()
        .sub(0);
    env.grants
        .change_binding(
            &ctx(b),
            sub,
            BindingChange::Bind(user("alice")),
            token(),
            None,
        )
        .await
        .unwrap();
    let req = request(USER, "alice", b);
    assert!(
        signed(&env, a, &[(&root, t.clone())])
            .check(&req, now())
            .await
            .unwrap()
            .granted
    );

    let mut ruled = t;
    ruled.recipient_rules = Some(license_enforcement_sdk::models::Conditions(
        serde_json::json!({"op": "eq"}),
    ));
    let d = signed(&env, a, &[(&root, ruled)])
        .check(&req, now())
        .await
        .unwrap();
    assert_eq!(d.reason, Some(DenialReason::ChainInvalid), "{d:?}");
}

/// A root without a live row denies as a broken chain, a copied row as a
/// duplicate; both before anything else is held against the fields.
#[tokio::test]
async fn the_count_of_live_rows_names_the_denial() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    let g = crate::test_support::issue_one(&env, tenant, terms()).await;
    let req = request(USER, "alice", tenant);
    let fields = license_enforcement_sdk::RootFields::new(tenant, terms());
    for (live_rows, expected) in [
        (0, DenialReason::ChainInvalid),
        (2, DenialReason::Duplicated),
    ] {
        let shape = Shape {
            chain: vec![g.clone()],
            live_children: Vec::new(),
            live_rows,
        };
        assert_eq!(
            env.check
                .against_fields(&req, &[], &g, &g, &shape, &fields, now()),
            Err(expected)
        );
    }
}
