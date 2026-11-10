use license_enforcement_sdk::ids::{EvidenceId, GrantId};
use license_enforcement_sdk::linked_evaluator;
use license_enforcement_sdk::models::{
    Binding, Permissions, ResourceRef, SubjectRef, Validity, Window,
};
use license_resolver_sdk::{LicenseCheckContext, Resource, Subject};
use serde_json::json;
use time::macros::datetime;
use uuid::Uuid;

use super::*;

const USER: &str = "gts.cf.core.lic.subj.v1~acme.app.user.v1~";
const ADMIN: &str = "gts.cf.core.lic.subj.v1~acme.app.user.v1~acme.app.admin.v1~";
const TENANT: &str = "gts.cf.core.lic.subj.v1~cf.core.lic.tenant.v1~";

fn request(subject_type: &str, subject_id: Option<&str>, tenant: Uuid) -> LicenseCheckRequest {
    LicenseCheckRequest::new(
        Subject {
            gts_type: GtsTypeId::new(subject_type),
            id: subject_id.map(str::to_owned),
            metadata: serde_json::Map::new(),
        },
        Resource {
            gts_type: GtsTypeId::new("gts.cf.core.lic.res.v1~acme.app.seat.v1~"),
            id: None,
            metadata: serde_json::Map::new(),
        },
        LicenseCheckContext::builder()
            .tenant_id(tenant)
            .build()
            .unwrap(),
    )
}

fn grant(state: GrantState, grace_until: Option<OffsetDateTime>) -> Grant {
    Grant {
        id: GrantId(Uuid::nil()),
        holder_tenant_id: Uuid::nil(),
        resource: ResourceRef {
            gts_type: GtsTypeId::new("gts.cf.core.lic.res.v1~acme.app.seat.v1~"),
            id: None,
        },
        subject_type: GtsTypeId::new(USER),
        binding: Binding::Subject {
            subject: SubjectRef {
                gts_type: GtsTypeId::new(USER),
                id: "alice".to_owned(),
            },
        },
        binding_fixed: false,
        validity: Validity::Own {
            window: Window {
                not_before: datetime!(2026-01-01 00:00 UTC),
                expires_at: datetime!(2027-01-01 00:00 UTC),
                grace_until,
            },
        },
        conditions: None,
        permissions: Permissions {
            may_bind: true,
            max_depth: 0,
            requires_acceptance: false,
        },
        recipient_rules: None,
        parent_grant_id: None,
        delegation_id: None,
        pack_id: None,
        evidence_id: EvidenceId(Uuid::nil()),
        state,
        termination_reason: None,
        version: 1,
    }
}

#[test]
fn links_as_the_only_evaluator() {
    assert!(linked_evaluator().is_ok());
}

#[test]
fn filter_matches_subject_then_tenant() {
    let tenant = Uuid::now_v7();
    let filters = DefaultEvaluator.candidate_filter(&request(USER, Some("alice"), tenant));
    assert_eq!(
        filters,
        vec![
            BindingFilter::exact(GtsTypeId::new(USER), "alice"),
            BindingFilter::new(GtsTypeId::new(TENANT), tenant.to_string()),
        ]
    );
}

#[test]
fn filter_covers_base_types_of_the_subject_most_derived_first() {
    let tenant = Uuid::now_v7();
    let filters = DefaultEvaluator.candidate_filter(&request(ADMIN, Some("alice"), tenant));
    assert_eq!(
        filters,
        vec![
            BindingFilter::exact(GtsTypeId::new(ADMIN), "alice"),
            BindingFilter::exact(GtsTypeId::new(USER), "alice"),
            BindingFilter::new(GtsTypeId::new(TENANT), tenant.to_string()),
        ]
    );
}

#[test]
fn filter_without_subject_id_has_only_tenant() {
    let tenant = Uuid::now_v7();
    let filters = DefaultEvaluator.candidate_filter(&request(USER, None, tenant));
    assert_eq!(
        filters,
        vec![BindingFilter::new(
            GtsTypeId::new(TENANT),
            tenant.to_string()
        )]
    );
}

#[test]
fn filter_for_tenant_subject_is_not_duplicated() {
    let tenant = Uuid::now_v7();
    let filters =
        DefaultEvaluator.candidate_filter(&request(TENANT, Some(&tenant.to_string()), tenant));
    assert_eq!(filters.len(), 1);
}

#[test]
fn evaluate_states_and_window() {
    let req = request(USER, Some("alice"), Uuid::nil());
    let active = grant(GrantState::Active, None);
    let in_window = datetime!(2026-06-01 00:00 UTC);
    let cases = [
        (
            GrantState::AwaitingAcceptance,
            GrantState::Active,
            in_window,
            Evaluation::Deny(EvaluatorDenial::AwaitingAcceptance),
        ),
        (
            GrantState::Suspended,
            GrantState::Active,
            in_window,
            Evaluation::Deny(EvaluatorDenial::Suspended),
        ),
        (
            GrantState::Terminated,
            GrantState::Active,
            in_window,
            Evaluation::Deny(EvaluatorDenial::Terminated),
        ),
        (
            GrantState::Active,
            GrantState::Suspended,
            in_window,
            Evaluation::Deny(EvaluatorDenial::Suspended),
        ),
        (
            GrantState::Active,
            GrantState::Active,
            datetime!(2025-12-31 23:59 UTC),
            Evaluation::Deny(EvaluatorDenial::NotYetValid),
        ),
        (
            GrantState::Active,
            GrantState::Active,
            datetime!(2026-01-01 00:00 UTC),
            Evaluation::Pass,
        ),
        (
            GrantState::Active,
            GrantState::Active,
            datetime!(2027-01-01 00:00 UTC),
            Evaluation::Deny(EvaluatorDenial::Expired),
        ),
    ];
    for (root_state, candidate_state, instant, expected) in cases {
        let root = grant(root_state, None);
        let candidate = grant(candidate_state, None);
        assert_eq!(
            DefaultEvaluator.evaluate(&req, &candidate, &root, instant),
            expected,
            "root {root_state:?}, candidate {candidate_state:?}, at {instant}"
        );
    }
    assert_eq!(
        DefaultEvaluator.evaluate(&req, &active, &active, in_window),
        Evaluation::Pass
    );
}

#[test]
fn evaluate_grace() {
    let req = request(USER, Some("alice"), Uuid::nil());
    let g = grant(GrantState::Active, Some(datetime!(2027-02-01 00:00 UTC)));
    assert_eq!(
        DefaultEvaluator.evaluate(&req, &g, &g, datetime!(2027-01-15 00:00 UTC)),
        Evaluation::PassInGrace
    );
    assert_eq!(
        DefaultEvaluator.evaluate(&req, &g, &g, datetime!(2027-02-01 00:00 UTC)),
        Evaluation::Deny(EvaluatorDenial::GraceExpired)
    );
}

#[test]
fn evaluate_denies_stored_conditions() {
    let req = request(USER, Some("alice"), Uuid::nil());
    let mut g = grant(GrantState::Active, None);
    g.conditions = Some(Conditions(json!({"plan": "pro"})));
    assert_eq!(
        DefaultEvaluator.evaluate(&req, &g, &g, datetime!(2026-06-01 00:00 UTC)),
        Evaluation::Deny(EvaluatorDenial::ConditionUnmet)
    );
    // The window comes before conditions (DESIGN §3.1 step 2).
    assert_eq!(
        DefaultEvaluator.evaluate(&req, &g, &g, datetime!(2030-01-01 00:00 UTC)),
        Evaluation::Deny(EvaluatorDenial::Expired)
    );
    g.conditions = Some(Conditions(json!({})));
    assert_eq!(
        DefaultEvaluator.evaluate(&req, &g, &g, datetime!(2026-06-01 00:00 UTC)),
        Evaluation::Pass
    );
}

#[test]
fn validate_and_match_accept_only_empty_rules() {
    for empty in [json!(null), json!({})] {
        assert!(
            DefaultEvaluator
                .validate(&Conditions(empty.clone()), &json!({}))
                .is_empty()
        );
        assert_eq!(
            DefaultEvaluator.matches(&Conditions(empty), &json!({})),
            Ok(true)
        );
    }
    let rules = Conditions(json!({"plan": "pro"}));
    let violations = DefaultEvaluator.validate(&rules, &json!({}));
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].reason, RULES_INVALID);
    assert!(DefaultEvaluator.matches(&rules, &json!({})).is_err());
}

#[test]
fn names_the_tenant_subject_type() {
    assert_eq!(DefaultEvaluator.named_types(), vec![GtsTypeId::new(TENANT)]);
}

#[test]
fn filter_never_names_the_subject_base_type() {
    let tenant = Uuid::now_v7();
    let minor = "gts.cf.core.lic.subj.v1.0~acme.app.user.v1~";
    let filters = DefaultEvaluator.candidate_filter(&request(minor, Some("alice"), tenant));
    assert_eq!(
        filters,
        vec![
            BindingFilter::exact(GtsTypeId::new(minor), "alice"),
            BindingFilter::new(GtsTypeId::new(TENANT), tenant.to_string()),
        ]
    );
}
