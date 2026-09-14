//! Tests for the REST-level error mapping.

use super::{authz_error_to_canonical, unauthenticated};
use crate::authz::{AuthzError, DeniedAttempt};
use toolkit_canonical_errors::CanonicalError;

/// A denial carrying the operand set the funnel now emits.
fn denial(reason: &str) -> AuthzError {
    AuthzError::Denied(Box::new(DeniedAttempt {
        subject_principal_id: uuid::Uuid::from_u128(0x_5e_eb),
        subject_tenant_id: uuid::Uuid::from_u128(0x_7e_11),
        resource_type: crate::authz::labels::PLAN.to_owned(),
        action: "write".to_owned(),
        resource_id: None,
        owner_tenant_id: None,
        reason: reason.to_owned(),
    }))
}

/// What the caller receives, rather than what the error prints.
///
/// Every claim below is about the wire, and `Debug` is not it: `CanonicalError`
/// carries fields that never serialize (`Internal` and `Unknown` mark their
/// descriptions `#[serde(skip)]`), so a `Debug` string is wider than the body on
/// the negative claims — and on the positive one it is evidence that the value
/// is somewhere in the error, not that a caller is told it.
/// This is the document `Problem::into_response` writes, taken one step short of
/// the bytes so the assertions can read members by name.
fn wire_body(err: &CanonicalError) -> serde_json::Value {
    serde_json::to_value(toolkit_canonical_errors::Problem::from(err.clone()))
        .expect("a problem document serializes")
}

#[test]
fn a_denial_is_403_and_keeps_the_deny_reason() {
    let err = authz_error_to_canonical(denial("no plan x write"));

    assert_eq!(err.status_code(), 403);
    assert!(
        wire_body(&err).to_string().contains("no plan x write"),
        "the PDP's reason is what the caller is told: {}",
        wire_body(&err)
    );
}

#[test]
fn a_denial_tells_the_caller_the_reason_and_nothing_about_the_subject() {
    // The operand set exists for the log, not for the wire: a 403 that echoed the
    // principal id back would hand an unauthenticated prober a way to confirm one.
    let err = authz_error_to_canonical(denial("no plan x write"));
    let body = wire_body(&err).to_string();

    assert!(
        !body.contains(&uuid::Uuid::from_u128(0x_5e_eb).to_string()),
        "{body}"
    );
    assert!(
        !body.contains(&uuid::Uuid::from_u128(0x_7e_11).to_string()),
        "{body}"
    );
}

#[test]
fn a_pdp_outage_fails_closed_as_503_and_leaks_no_diagnostic() {
    // Never an allow, and never an explanation of the policy engine's internals
    // to an unauthenticated-for-all-we-know caller.
    //
    // **The absence is structural, and the `!contains` is what guards it.**
    // `authz_error_to_canonical` consumes the detail into the `error` log and
    // builds the 503 from nothing, so a wave that passed the detail along would
    // do it through `ServiceUnavailableBuilder::with_detail` and land in
    // `detail`. The `context` equality below pins a different thing: that arm
    // takes no `retry_after_seconds` either, so a caller is told nothing about
    // when to come back and that is deliberate — the PDP's recovery is not this
    // gear's to estimate.

    let err = authz_error_to_canonical(AuthzError::Unavailable(
        "pdp connect timeout to 10.0.0.9".to_owned(),
    ));

    assert_eq!(err.status_code(), 503);
    let body = wire_body(&err);
    assert!(!body.to_string().contains("10.0.0.9"), "{body}");
    assert_eq!(
        body["context"],
        serde_json::json!({}),
        "the outage carries no operand at all: {body}"
    );
}

#[test]
fn a_missing_identity_is_401_not_403() {
    // 403 would tell an anonymous caller that the resource exists and that it
    // is merely not allowed to it.
    assert_eq!(unauthenticated().status_code(), 401);
}

/// One emitted event's fields, by name.
type DenyRecord = std::collections::HashMap<String, String>;

/// The `pricing.authz.deny` records emitted while `f` runs, as field maps.
///
/// The deny record is `inst-rb-audit` / `dod-rbac`, both `p1`, and it is a pure
/// side effect: `authz_error_to_canonical` returns the same `CanonicalError`
/// whether or not it emitted one. Nothing in the crate read it back, so the
/// operand set could have been dropped a field at a time with every test here
/// green. Installing a subscriber is the only way to make the emission a fact a
/// case can assert.
fn deny_records(f: impl FnOnce()) -> Vec<DenyRecord> {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<DenyRecord>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() != "pricing.authz.deny" {
                return;
            }
            let mut fields = DenyRecord::new();
            let mut visitor = Visitor(&mut fields);
            event.record(&mut visitor);
            self.0
                .lock()
                .expect("the capture is not poisoned")
                .push(fields);
        }
    }

    struct Visitor<'a>(&'a mut DenyRecord);

    impl tracing::field::Visit for Visitor<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.insert(field.name().to_owned(), value.to_owned());
        }
    }

    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::with_default(subscriber, f);
    let records = capture.0.lock().expect("the capture is not poisoned");
    records.clone()
}

#[test]
fn a_denial_leaves_the_operand_set_in_the_log_and_an_outage_leaves_none() {
    let records = deny_records(|| {
        let _ = authz_error_to_canonical(denial("no plan x write"));
    });

    assert_eq!(records.len(), 1, "one denial, one record: {records:?}");
    let record = &records[0];
    // The whole operand set, by name. A missing field here is an operator who
    // cannot tell which principal was refused what.
    for field in [
        "subject_principal_id",
        "subject_tenant_id",
        "resource_type",
        "action",
        "resource_id",
        "owner_tenant_id",
        "reason",
    ] {
        assert!(
            record.contains_key(field),
            "the deny record carries `{field}`: {record:?}"
        );
    }
    assert_eq!(
        record.get("subject_principal_id").map(String::as_str),
        Some(uuid::Uuid::from_u128(0x_5e_eb).to_string().as_str()),
        "and the values are the attempt's, not a placeholder: {record:?}"
    );
    assert_eq!(record.get("action").map(String::as_str), Some("write"));
    assert_eq!(
        record.get("reason").map(String::as_str),
        Some("no plan x write")
    );

    // **The operands that carry an id, on a denial that has them.** `denial()`
    // leaves `resource_id` and `owner_tenant_id` `None`, so presence alone is all
    // the case above can say about them - and presence survives a mis-wiring that
    // stamps one operand's value under another's name. Those two are exactly the
    // pair an operator needs to name the object a `p1` record is about.
    let identified = deny_records(|| {
        let _ = authz_error_to_canonical(AuthzError::Denied(Box::new(DeniedAttempt {
            subject_principal_id: uuid::Uuid::from_u128(0x_5e_eb),
            subject_tenant_id: uuid::Uuid::from_u128(0x_7e_11),
            resource_type: crate::authz::labels::PLAN.to_owned(),
            action: "write".to_owned(),
            resource_id: Some(uuid::Uuid::from_u128(0x_a5_5e)),
            owner_tenant_id: Some(uuid::Uuid::from_u128(0x_7e_22)),
            reason: "no plan x write".to_owned(),
        })));
    });
    assert_eq!(identified.len(), 1, "{identified:?}");
    let identified = &identified[0];
    assert_eq!(
        identified.get("subject_tenant_id").map(String::as_str),
        Some(uuid::Uuid::from_u128(0x_7e_11).to_string().as_str())
    );
    assert_eq!(
        identified.get("resource_type").map(String::as_str),
        Some(crate::authz::labels::PLAN)
    );
    // Both ride `?`, so they render as the `Option`'s `Debug`.
    assert_eq!(
        identified.get("resource_id").map(String::as_str),
        Some(format!("Some({})", uuid::Uuid::from_u128(0x_a5_5e)).as_str()),
        "the record names the object, not merely that a field is present: {identified:?}"
    );
    assert_eq!(
        identified.get("owner_tenant_id").map(String::as_str),
        Some(format!("Some({})", uuid::Uuid::from_u128(0x_7e_22)).as_str()),
        "and whose it is: {identified:?}"
    );

    // The `Unavailable` arm is not a denial and must not be filed as one: an
    // outage that logged here would put a PDP failure in the operator's list of
    // refused principals.
    let outage = deny_records(|| {
        let _ = authz_error_to_canonical(AuthzError::Unavailable("pdp down".to_owned()));
    });
    assert!(outage.is_empty(), "an outage is not a denial: {outage:?}");
}
