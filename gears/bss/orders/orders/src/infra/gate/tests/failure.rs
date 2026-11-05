//! The submit/commit failure log (lib tier, ungated): identities and the cause are rendered and
//! nothing else is.
use super::super::failure::{Ids, submit_integration};
use uuid::Uuid;

#[tracing_test::traced_test]
#[test]
fn ids_are_rendered() {
    let (order, attempt, line) = (
        Uuid::from_u128(0xA1),
        Uuid::from_u128(0xB2),
        Uuid::from_u128(0xC3),
    );
    let ids = Ids::attempt(order, attempt).line(line);
    submit_integration(
        "a recorded receipt result does not decode",
        ids,
        Some(&"member `receipt result`"),
    );
    assert!(logs_contain("a recorded receipt result does not decode"));
    assert!(logs_contain(&order.to_string()));
    assert!(logs_contain(&attempt.to_string()));
    assert!(logs_contain(&line.to_string()));
    assert!(logs_contain("member `receipt result`"));
    assert!(logs_contain("orders.gate.submit"));
}

#[tracing_test::traced_test]
#[test]
fn unknown_ids_and_no_cause_are_logged_as_absent() {
    submit_integration("an attempt without lines", Ids::default(), None);
    assert!(logs_contain("an attempt without lines"));
    assert!(logs_contain("order_id=None"));
    assert!(logs_contain("attempt_id=None"));
    assert!(logs_contain("line_id=None"));
    assert!(!logs_contain("cause="));
}

#[tracing_test::traced_test]
#[test]
fn preview_run_failure_names_run_and_correlation_ids() {
    let (run, correlation) = (Uuid::from_u128(0xD4), Uuid::from_u128(0xE5));
    let _ = super::super::preview::run_failure(
        run,
        Some(correlation),
        &super::super::orchestrate::OrchestrateError::Cancelled,
    );
    assert!(logs_contain("orders.gate.preview"));
    assert!(logs_contain(&run.to_string()));
    assert!(logs_contain(&correlation.to_string()));
}
