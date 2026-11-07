use std::sync::Arc;

use gts::GtsTypeId;
use license_enforcement_sdk::models::{GrantTerms, SubjectRef};
use time::Duration;
use uuid::Uuid;

use super::*;
use crate::domain::grants::BindingChange;
use crate::test_support::{
    ADMIN, Env, USER, ctx, env, input, now, request, tenant_subject, terms, token, user,
};

async fn issue(env: &Env, tenant: Uuid, t: GrantTerms) -> Grant {
    env.grants
        .issue(&ctx(tenant), tenant, vec![t], false, token())
        .await
        .unwrap()
        .grants
        .remove(0)
}

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

    let g = issue(&env, tenant, terms()).await;
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
    let g = issue(&env, tenant, bound_to(user("alice"))).await;
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
    issue(
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

struct AnotherPlugin;

#[async_trait::async_trait]
impl GrantAdmissionPluginV1 for AnotherPlugin {
    fn info(&self) -> license_enforcement_sdk::PluginInfo {
        license_enforcement_sdk::PluginInfo::new("another", false, None)
    }

    fn intakes(&self) -> license_enforcement_sdk::Intakes {
        le_issue_admission_plugin::IssueAdmissionPlugin.intakes()
    }

    async fn admit(
        &self,
        input: license_enforcement_sdk::AdmissionInput,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Admission, license_enforcement_sdk::AdmissionError> {
        le_issue_admission_plugin::IssueAdmissionPlugin
            .admit(input, instant, material)
            .await
    }

    async fn verify(
        &self,
        root_id: license_enforcement_sdk::ids::GrantId,
        evidence: &license_enforcement_sdk::StoredEvidence,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Verification, license_enforcement_sdk::PluginFault> {
        le_issue_admission_plugin::IssueAdmissionPlugin
            .verify(root_id, evidence, instant, material)
            .await
    }

    async fn health(&self) -> Result<(), license_enforcement_sdk::PluginFault> {
        Ok(())
    }
}

#[tokio::test]
async fn evidence_of_another_plugin_does_not_verify() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    issue(&env, tenant, bound_to(user("alice"))).await;
    let check = CheckService::new(
        Arc::clone(&env.db),
        env.store,
        license_enforcement_sdk::linked_evaluator().unwrap(),
        Arc::new(AnotherPlugin),
    );
    let decision = check
        .check(&request(USER, "alice", tenant), now())
        .await
        .unwrap();
    assert_eq!(decision.reason, Some(DenialReason::ChainInvalid));
}

/// The issue plugin under its own id, with `verifies_evidence` set by the test.
/// Records the length of the evidence bytes that `verify` gets.
struct BytesProbe {
    verifies: bool,
    seen: std::sync::Mutex<Option<usize>>,
}

#[async_trait::async_trait]
impl GrantAdmissionPluginV1 for BytesProbe {
    fn info(&self) -> license_enforcement_sdk::PluginInfo {
        let id = le_issue_admission_plugin::IssueAdmissionPlugin.info().id;
        license_enforcement_sdk::PluginInfo::new(id, self.verifies, None)
    }

    fn intakes(&self) -> license_enforcement_sdk::Intakes {
        le_issue_admission_plugin::IssueAdmissionPlugin.intakes()
    }

    async fn admit(
        &self,
        input: license_enforcement_sdk::AdmissionInput,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Admission, license_enforcement_sdk::AdmissionError> {
        le_issue_admission_plugin::IssueAdmissionPlugin
            .admit(input, instant, material)
            .await
    }

    async fn verify(
        &self,
        root_id: license_enforcement_sdk::ids::GrantId,
        evidence: &license_enforcement_sdk::StoredEvidence,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Verification, license_enforcement_sdk::PluginFault> {
        *self.seen.lock().unwrap() = Some(evidence.bytes.len());
        le_issue_admission_plugin::IssueAdmissionPlugin
            .verify(root_id, evidence, instant, material)
            .await
    }

    async fn health(&self) -> Result<(), license_enforcement_sdk::PluginFault> {
        Ok(())
    }
}

#[tokio::test]
async fn evidence_bytes_reach_only_a_plugin_that_verifies_them() {
    let env = env().await;
    let tenant = Uuid::now_v7();
    issue(&env, tenant, bound_to(user("alice"))).await;
    for verifies in [false, true] {
        let probe = Arc::new(BytesProbe {
            verifies,
            seen: std::sync::Mutex::default(),
        });
        let check = CheckService::new(
            Arc::clone(&env.db),
            env.store,
            license_enforcement_sdk::linked_evaluator().unwrap(),
            Arc::clone(&probe) as Arc<dyn GrantAdmissionPluginV1>,
        );
        let decision = check
            .check(&request(USER, "alice", tenant), now())
            .await
            .unwrap();
        assert!(decision.granted, "{decision:?}");
        let seen = probe.seen.lock().unwrap().unwrap();
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
    issue(&env, tenant, t).await;
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
    let g = issue(&env, tenant, t).await;
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
    issue(&env, tenant, not_yet).await;
    let mut wide = bound_to(SubjectRef {
        gts_type: GtsTypeId::new(tenant_subject()),
        id: tenant.to_string(),
    });
    wide.subject_type = GtsTypeId::new(tenant_subject());
    let wide = issue(&env, tenant, wide).await;
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
    issue(&env, other, not_yet).await;
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
    issue(&env, tenant, t).await;
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
    issue(&env, tenant, t).await;

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
    issue(&env, tenant, t).await;
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
    let g = issue(&env, tenant, bound_to(user("alice"))).await;
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
    issue(&env, a, bound_to(user("alice"))).await;
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

    let g = issue(&env, tenant, bound_to(user("alice"))).await;
    let granted = adapter
        .is_licensed(request(USER, "alice", tenant))
        .await
        .unwrap();
    assert!(granted.granted);
    assert_eq!(granted.diagnostics[DIAG_GRANT_ID], g.id.to_string());
}

/// The `issue` plugin whose `verify` returns a set verification.
struct VerifyAs(license_enforcement_sdk::Verification);

#[async_trait::async_trait]
impl GrantAdmissionPluginV1 for VerifyAs {
    fn info(&self) -> license_enforcement_sdk::PluginInfo {
        le_issue_admission_plugin::IssueAdmissionPlugin.info()
    }

    fn intakes(&self) -> license_enforcement_sdk::Intakes {
        le_issue_admission_plugin::IssueAdmissionPlugin.intakes()
    }

    async fn admit(
        &self,
        input: license_enforcement_sdk::AdmissionInput,
        instant: OffsetDateTime,
        material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Admission, license_enforcement_sdk::AdmissionError> {
        le_issue_admission_plugin::IssueAdmissionPlugin
            .admit(input, instant, material)
            .await
    }

    async fn verify(
        &self,
        _root_id: license_enforcement_sdk::ids::GrantId,
        _evidence: &license_enforcement_sdk::StoredEvidence,
        _instant: OffsetDateTime,
        _material: &dyn license_enforcement_sdk::PluginMaterial,
    ) -> Result<license_enforcement_sdk::Verification, license_enforcement_sdk::PluginFault> {
        Ok(self.0.clone())
    }

    async fn health(&self) -> Result<(), license_enforcement_sdk::PluginFault> {
        Ok(())
    }
}

fn check_with(
    env: &Env,
    v: license_enforcement_sdk::Verification,
) -> CheckService<crate::infra::storage::Store> {
    CheckService::new(
        Arc::clone(&env.db),
        env.store,
        license_enforcement_sdk::linked_evaluator().unwrap(),
        Arc::new(VerifyAs(v)),
    )
}

#[tokio::test]
async fn verify_outcomes_map_to_decisions() {
    use license_enforcement_sdk::{PluginDenial, RootFields, Verification};
    let env = env().await;
    let tenant = Uuid::now_v7();
    issue(&env, tenant, bound_to(user("alice"))).await;
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
    let err = check_with(&env, Verification::new(root_fields))
        .check(&req, now())
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Internal(_)), "{err:?}");
}

#[tokio::test]
async fn material_written_by_verify_is_stored_and_a_failed_write_keeps_the_decision() {
    use license_enforcement_sdk::{Expect, MaterialWrite, Verification};
    let env = env().await;
    let tenant = Uuid::now_v7();
    issue(&env, tenant, bound_to(user("alice"))).await;
    let req = request(USER, "alice", tenant);
    let id = le_issue_admission_plugin::IssueAdmissionPlugin.info().id;
    let stored = || async {
        let conn = env.db.conn().unwrap();
        env.store.material(&conn, id, "mark").await.unwrap()
    };

    let mut v = Verification::new(VerifyOutcome::Verified(None));
    v.material = vec![MaterialWrite::put("mark", vec![1])];
    assert!(
        check_with(&env, v)
            .check(&req, now())
            .await
            .unwrap()
            .granted
    );
    assert_eq!(stored().await, Some(vec![1]));

    let mut v = Verification::new(VerifyOutcome::Verified(None));
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
    let g = issue(&env, tenant, bound_to(user("alice"))).await;
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
    issue(&env, tenant, bound_to(user("alice"))).await;
    assert_eq!(
        reason(&env, &request(USER, "ali\0ce", tenant), now()).await,
        Some(DenialReason::NoGrantForResource)
    );
}
