//! D-226 (PR #5259 #188): one actor class for every authorization decision.
//!
//! `authz::decision_class` is what the PEP's user-only (D-182 break-glass) and finite-ID rules
//! and the engine's declared row actor rule all read. The configured identities decide; the
//! authenticated subject type can only tighten an unconfigured subject from `User` to `Service`.
#![expect(
    clippy::expect_used,
    reason = "a malformed fixture must fail the test at once"
)]

use bss_orders::authz::decision_class;
use bss_orders::domain::audit::{ActorIdentities, AuditActorClass, Principal, ServiceRole};
use toolkit_security::SecurityContext;
use uuid::Uuid;

const USER: &str = "gts.cf.core.security.subject_user.v1~";
const SERVICE: &str = "gts.cf.core.security.subject_service.v1~";

const SYSTEM: (u128, u128) = (900, 901);
const WORKFLOW: (u128, u128) = (902, 901);
const BILLING: (u128, u128) = (903, 901);
const UNCONFIGURED: (u128, u128) = (1, 10);

fn principal((subject, tenant): (u128, u128)) -> Principal {
    Principal {
        subject_id: Uuid::from_u128(subject),
        subject_tenant_id: Uuid::from_u128(tenant),
    }
}

fn identities() -> ActorIdentities {
    ActorIdentities::new(
        Some(principal(SYSTEM)),
        [
            (ServiceRole::Workflow, principal(WORKFLOW)),
            (ServiceRole::Billing, principal(BILLING)),
        ],
    )
    .expect("valid identities")
}

fn ctx((subject, tenant): (u128, u128), subject_type: Option<&str>) -> SecurityContext {
    let builder = SecurityContext::builder()
        .subject_id(Uuid::from_u128(subject))
        .subject_tenant_id(Uuid::from_u128(tenant));
    match subject_type {
        Some(kind) => builder.subject_type(kind),
        None => builder,
    }
    .build()
    .expect("valid context")
}

/// Every (configured identity × subject type) combination.
#[test]
fn configured_identities_decide_and_the_subject_type_only_tightens() {
    use AuditActorClass::{Service, System, User};
    let identities = identities();
    let other = "gts.cf.core.security.subject_other.v1~";
    let cases = [
        // Correctly configured callers: unchanged from the subject-type rule.
        (UNCONFIGURED, Some(USER), User),
        (WORKFLOW, Some(SERVICE), Service),
        (BILLING, Some(SERVICE), Service),
        (SYSTEM, Some(SERVICE), System),
        // Unconfigured non-user tokens keep the stricter service rules.
        (UNCONFIGURED, Some(SERVICE), Service),
        (UNCONFIGURED, Some(other), Service),
        (UNCONFIGURED, None, Service),
        // Configured principals keep their configured class whatever the token says.
        (WORKFLOW, Some(USER), Service),
        (BILLING, Some(USER), Service),
        (WORKFLOW, None, Service),
        (SYSTEM, Some(USER), System),
        (SYSTEM, None, System),
        // A configured subject id under another tenant is not the configured principal.
        ((902, 10), Some(USER), User),
        ((902, 10), Some(SERVICE), Service),
    ];
    for (subject, kind, expected) in cases {
        assert_eq!(
            decision_class(&identities, &ctx(subject, kind)),
            expected,
            "{subject:?} typed {kind:?}"
        );
    }
}

/// The audit row's class comes from the same configured identities; the decision class differs
/// from it only by tightening an unconfigured non-user token to `Service`.
#[test]
fn decision_class_agrees_with_the_audit_class_except_the_tightening() {
    let identities = identities();
    for subject in [UNCONFIGURED, WORKFLOW, BILLING, SYSTEM] {
        for kind in [Some(USER), Some(SERVICE), None] {
            let ctx = ctx(subject, kind);
            let audit = identities.classify(&ctx).expect("stable actor").class();
            let decision = decision_class(&identities, &ctx);
            if audit == AuditActorClass::User && kind != Some(USER) {
                assert_eq!(decision, AuditActorClass::Service);
            } else {
                assert_eq!(decision, audit, "{subject:?} typed {kind:?}");
            }
        }
    }
}

/// A user-typed subject configured as a non-user principal is a misconfiguration: logged with
/// the class only, never the principal (D-182).
#[test]
#[tracing_test::traced_test]
fn a_misconfigured_user_typed_principal_is_logged_without_the_principal() {
    let identities = identities();
    assert_eq!(
        decision_class(&identities, &ctx(WORKFLOW, Some(USER))),
        AuditActorClass::Service
    );
    assert!(logs_contain("configured as a non-user principal"));
    assert!(logs_contain("actor_class=\"service\""));
    let workflow = principal(WORKFLOW);
    assert!(!logs_contain(&workflow.subject_id.to_string()));
    assert!(!logs_contain(&workflow.subject_tenant_id.to_string()));
    // A correctly configured caller logs nothing.
    assert_eq!(
        decision_class(&identities, &ctx(WORKFLOW, Some(SERVICE))),
        AuditActorClass::Service
    );
    assert_eq!(
        decision_class(&identities, &ctx(UNCONFIGURED, Some(USER))),
        AuditActorClass::User
    );
    logs_assert(|lines: &[&str]| {
        match lines
            .iter()
            .filter(|l| l.contains("configured as a non-user principal"))
            .count()
        {
            1 => Ok(()),
            n => Err(format!("expected one misconfiguration line, got {n}")),
        }
    });
}
