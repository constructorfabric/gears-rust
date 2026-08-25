//! Tests for the authentication-context extractor.
//!
//! **All four refusal arms, and a positive control.** Only the missing-extension
//! arm was covered until 2026-08-18: `grep -rn "is_nil()"` over `tests/` returned
//! nothing, and `ctx_for_principal` — the one builder every harness client uses —
//! always sets a real `subject_id`, `subject_tenant_id` and `subject_type`, so no
//! fixture in the crate could produce the other three shapes. Deleting either `if`
//! block left the whole suite green.
//!
//! The shapes are not hypothetical. `SecurityContext::anonymous()` is a live value
//! in this process: `infra::jobs` names it as the actor the three background
//! sweeps run under. A middleware ordering change, or a gateway that installs a
//! placeholder context on an unauthenticated route rather than omitting the
//! extension, delivers exactly it — and with the guard gone every downstream call
//! would carry `tenant_id = 00000000-…` into the audit stamp and the idempotency
//! key.

use axum::extract::Extension;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::require_authenticated;

/// A context of the shape a real `AuthN` resolver produces.
///
/// **No `token_scopes`, deliberately.** `require_authenticated` never reads them,
/// and `module_test`'s `no_source_reads_token_scopes` refuses any `src/` file that
/// names the field while every harness client is built with the wildcard `["*"]` —
/// the guard that keeps a scope-keyed refusal from being untestable and green at
/// the same time. Setting it here would have bought nothing and tripped it.
fn authed(subject: Uuid, tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .subject_type(gts_id!("cf.core.security.subject_user.v1~"))
        .build()
        .expect("authed SecurityContext must build")
}

/// **The positive control**, and the four refusals below mean nothing without it:
/// an extractor that returned 401 unconditionally would satisfy every one of them.
#[test]
fn a_fully_populated_context_is_accepted() {
    let ctx = authed(Uuid::from_u128(0x5b), Uuid::from_u128(0x7e));

    let out = require_authenticated(Some(Extension(ctx))).expect("a real context is accepted");

    assert_eq!(out.subject_id(), Uuid::from_u128(0x5b));
    assert_eq!(out.subject_tenant_id(), Uuid::from_u128(0x7e));
}

#[test]
fn a_request_without_a_context_is_refused() {
    let err = require_authenticated(None).expect_err("no context is a 401");

    assert_eq!(err.status_code(), 401);
}

#[test]
fn a_context_with_a_nil_subject_is_refused() {
    let ctx = authed(Uuid::nil(), Uuid::from_u128(0x7e));

    let err = require_authenticated(Some(Extension(ctx))).expect_err("a nil subject is a 401");

    assert_eq!(err.status_code(), 401);
}

/// The one that costs the most if it goes: a nil tenant is not refused anywhere
/// downstream — it is a *valid* uuid, so it reaches the audit stamp, the
/// idempotency key and every scope key as `00000000-…`.
#[test]
fn a_context_with_a_nil_tenant_is_refused() {
    let ctx = authed(Uuid::from_u128(0x5b), Uuid::nil());

    let err = require_authenticated(Some(Extension(ctx))).expect_err("a nil tenant is a 401");

    assert_eq!(err.status_code(), 401);
}

#[test]
fn a_context_without_a_subject_type_is_refused() {
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5b))
        .subject_tenant_id(Uuid::from_u128(0x7e))
        .build()
        .expect("a context with no subject_type still builds");

    let err =
        require_authenticated(Some(Extension(ctx))).expect_err("an absent subject_type is a 401");

    assert_eq!(err.status_code(), 401);
}

/// The live value, asserted as itself rather than as a reconstruction of it —
/// `infra::jobs` runs the background sweeps under exactly this context, so if its
/// shape ever stops being the all-zero placeholder this is the test that says so.
#[test]
fn the_anonymous_placeholder_is_refused() {
    let err = require_authenticated(Some(Extension(SecurityContext::anonymous())))
        .expect_err("the anonymous placeholder is a 401");

    assert_eq!(err.status_code(), 401);
}
