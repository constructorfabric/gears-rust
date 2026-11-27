use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::models::{DenyReason, EvaluationResponseContext};

const SUBJECT_A: Uuid = Uuid::from_u128(0xA);
const SUBJECT_B: Uuid = Uuid::from_u128(0xB);
const ROW: Uuid = Uuid::from_u128(0x10);

fn subject(id: Uuid) -> Subject {
    Subject {
        id,
        subject_type: None,
        properties: HashMap::new(),
    }
}

fn action(name: &str) -> Action {
    Action {
        name: name.to_owned(),
    }
}

fn resource(id: Uuid) -> Resource {
    Resource {
        resource_type: "gts.cf.core.events.event.v1~".to_owned(),
        id: Some(id),
        properties: HashMap::new(),
    }
}

fn context(require_constraints: bool) -> EvaluationRequestContext {
    EvaluationRequestContext {
        tenant_context: None,
        token_scopes: vec!["*".to_owned()],
        require_constraints,
        capabilities: Vec::new(),
        supported_properties: vec!["owner_tenant_id".to_owned()],
        bearer_token: None,
    }
}

fn item() -> EvaluationBatchItem {
    EvaluationBatchItem::default()
}

fn batch(evaluations: Vec<EvaluationBatchItem>) -> EvaluationBatchRequest {
    EvaluationBatchRequest {
        subject: Some(subject(SUBJECT_A)),
        action: Some(action("read")),
        resource: Some(resource(ROW)),
        context: Some(context(true)),
        evaluations,
        options: EvaluationBatchOptions::default(),
    }
}

fn allow() -> EvaluationResponse {
    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext::default(),
    }
}

fn deny() -> EvaluationResponse {
    EvaluationResponse {
        decision: false,
        context: EvaluationResponseContext {
            constraints: Vec::new(),
            deny_reason: Some(DenyReason::new(DenyReason::INSUFFICIENT_PERMISSIONS)),
        },
    }
}

fn deny_without_code() -> EvaluationResponse {
    EvaluationResponse {
        decision: false,
        context: EvaluationResponseContext::default(),
    }
}

/// The single field violation of an `InvalidArgument`, as `(field, reason, description)`.
fn field_violation(err: impl Into<CanonicalError>) -> (String, String, String) {
    let err = err.into();
    let CanonicalError::InvalidArgument {
        ctx: toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations },
        ..
    } = &err
    else {
        panic!("expected InvalidArgument with field violations, got {err:?}");
    };
    assert_eq!(field_violations.len(), 1, "{err:?}");
    let v = &field_violations[0];
    (v.field.clone(), v.reason.clone(), v.description.clone())
}

// ── resolve ─────────────────────────────────────────────────────────────────

#[test]
fn resolve_fills_absent_keys_from_the_defaults() {
    let (requests, semantic) = batch(vec![item(), item()]).resolve(None).unwrap();
    assert_eq!(semantic, EvaluationsSemantic::ExecuteAll);
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.subject.id, SUBJECT_A);
        assert_eq!(request.action.name, "read");
        assert_eq!(request.resource.id, Some(ROW));
        assert!(request.context.require_constraints);
    }
}

#[test]
fn an_item_key_replaces_the_default_as_a_whole() {
    let mut overriding = item();
    overriding.subject = Some(subject(SUBJECT_B));
    overriding.action = Some(action("delete"));
    // A context that names only the tenant context gets the field defaults,
    // not the top-level context's fields (replacement, not merging).
    overriding.context =
        Some(serde_json::from_value(json!({ "tenant_context": { "mode": "root_only" } })).unwrap());

    let (requests, _) = batch(vec![item(), overriding]).resolve(None).unwrap();

    assert_eq!(requests[0].subject.id, SUBJECT_A);
    assert_eq!(requests[1].subject.id, SUBJECT_B);
    assert_eq!(requests[1].action.name, "delete");
    assert_eq!(requests[1].resource.id, Some(ROW));
    let replaced = &requests[1].context;
    assert!(!replaced.require_constraints);
    assert!(replaced.token_scopes.is_empty());
    assert!(replaced.supported_properties.is_empty());
    assert!(replaced.capabilities.is_empty());
    assert!(replaced.tenant_context.is_some());
}

#[test]
fn an_in_process_bearer_token_follows_the_replacement_rule() {
    let mut req = batch(vec![item(), item()]);
    req.context.as_mut().unwrap().bearer_token = Some(SecretString::from("default-token"));
    let mut own = context(false);
    own.bearer_token = Some(SecretString::from("item-token"));
    req.evaluations[1].context = Some(own);

    let (requests, _) = req.resolve(None).unwrap();

    let token = |i: usize| {
        requests[i]
            .context
            .bearer_token
            .as_ref()
            .unwrap()
            .expose_secret()
            .to_owned()
    };
    assert_eq!(token(0), "default-token");
    assert_eq!(token(1), "item-token");
}

#[test]
fn an_empty_batch_is_invalid_whatever_the_defaults() {
    let err = batch(Vec::new()).resolve(None).unwrap_err();
    assert_eq!(err, BatchRequestError::Empty);
    let (field, reason, _) = field_violation(err);
    assert_eq!(
        (field.as_str(), reason.as_str()),
        ("evaluations", "EMPTY_BATCH")
    );
}

#[test]
fn more_items_than_the_limit_is_invalid_naming_the_limit() {
    let err = batch(vec![item(); 3]).resolve(Some(2)).unwrap_err();
    assert_eq!(err, BatchRequestError::TooManyItems { max: 2, got: 3 });
    let (field, reason, description) = field_violation(err);
    assert_eq!(
        (field.as_str(), reason.as_str()),
        ("evaluations", "BATCH_TOO_LARGE")
    );
    assert!(description.contains("at most 2"), "{description}");
    batch(vec![item(); 2]).resolve(Some(2)).unwrap();
    batch(vec![item(); 500]).resolve(None).unwrap();
}

#[test]
fn an_item_without_a_resolvable_key_is_invalid_naming_the_index() {
    for key in ["subject", "action", "resource", "context"] {
        let mut req = batch(vec![item(), item()]);
        match key {
            "subject" => req.subject = None,
            "action" => req.action = None,
            "resource" => req.resource = None,
            _ => req.context = None,
        }
        req.evaluations[0] = EvaluationBatchItem {
            subject: Some(subject(SUBJECT_A)),
            action: Some(action("read")),
            resource: Some(resource(ROW)),
            context: Some(context(true)),
        };
        let err = req.resolve(None).unwrap_err();
        assert_eq!(err, BatchRequestError::MissingKey { index: 1, key });
        let (field, reason, _) = field_violation(err);
        assert_eq!(field, format!("evaluations[1].{key}"));
        assert_eq!(reason, "MISSING_EVALUATION_FIELD");
    }
}

// ── wire format ─────────────────────────────────────────────────────────────

#[test]
fn the_wire_request_omits_default_options_and_the_bearer_token() {
    let mut req = batch(vec![item()]);
    req.context.as_mut().unwrap().bearer_token = Some(SecretString::from("secret"));
    let wire = serde_json::to_value(&req).unwrap();
    assert!(wire.get("options").is_none(), "{wire}");
    assert!(!wire.to_string().contains("secret"));
    assert_eq!(wire["evaluations"], json!([{}]));

    req.options =
        EvaluationBatchOptions::default().with_semantic(EvaluationsSemantic::DenyOnFirstDeny);
    let wire = serde_json::to_value(&req).unwrap();
    assert_eq!(
        wire["options"],
        json!({ "evaluations_semantic": "deny_on_first_deny" })
    );
}

#[test]
fn the_wire_request_decodes_authzen_bodies() {
    let req: EvaluationBatchRequest = serde_json::from_value(json!({
        "subject": { "id": SUBJECT_A },
        "context": { "require_constraints": true },
        "evaluations": [
            { "action": { "name": "update" }, "resource": { "type": "t", "id": ROW } }
        ],
        "options": { "evaluations_semantic": "deny_on_first_deny", "future_option": 1 },
        "unknown_top_level": true
    }))
    .unwrap();
    assert_eq!(
        req.options.evaluations_semantic,
        EvaluationsSemantic::DenyOnFirstDeny
    );
    let (requests, semantic) = req.resolve(None).unwrap();
    assert_eq!(semantic, EvaluationsSemantic::DenyOnFirstDeny);
    assert_eq!(requests[0].action.name, "update");

    let absent: EvaluationBatchRequest =
        serde_json::from_value(json!({ "evaluations": [{}] })).unwrap();
    assert_eq!(
        absent.options.evaluations_semantic,
        EvaluationsSemantic::ExecuteAll
    );
}

#[test]
fn undecodable_bodies_are_rejected() {
    // A missing `evaluations` key and the deferred `permit_on_first_permit`
    // fail decoding.
    assert!(serde_json::from_value::<EvaluationBatchRequest>(json!({})).is_err());
    assert!(
        serde_json::from_value::<EvaluationBatchRequest>(json!({
            "evaluations": [{}],
            "options": { "evaluations_semantic": "permit_on_first_permit" }
        }))
        .is_err()
    );
}

#[test]
fn the_openapi_schema_requires_at_least_one_item() {
    use utoipa::PartialSchema;
    let schema = serde_json::to_value(EvaluationBatchRequest::schema()).unwrap();
    assert_eq!(schema["properties"]["evaluations"]["minItems"], json!(1));
    assert_eq!(schema["required"], json!(["evaluations"]));
}

// ── result shape ────────────────────────────────────────────────────────────

#[test]
fn execute_all_requires_one_result_per_item() {
    let all = EvaluationsSemantic::ExecuteAll;
    check_batch_results(&[allow(), deny(), allow()], 3, all).unwrap();
    assert_eq!(
        check_batch_results(&[allow(), deny()], 3, all),
        Err(BatchResultsError::CountMismatch {
            expected: 3,
            actual: 2
        })
    );
    assert_eq!(
        check_batch_results(&vec![allow(); 4], 3, all),
        Err(BatchResultsError::CountMismatch {
            expected: 3,
            actual: 4
        })
    );
}

#[test]
fn deny_on_first_deny_accepts_only_an_allowed_prefix_ending_in_a_denial() {
    let gate = EvaluationsSemantic::DenyOnFirstDeny;
    check_batch_results(&[allow(), allow(), allow()], 3, gate).unwrap();
    check_batch_results(&[allow(), allow(), deny()], 3, gate).unwrap();
    check_batch_results(&[allow(), deny()], 3, gate).unwrap();
    check_batch_results(&[deny()], 3, gate).unwrap();

    assert_eq!(
        check_batch_results(&[], 3, gate),
        Err(BatchResultsError::ResultCountOutOfRange {
            items: 3,
            results: 0
        })
    );
    assert_eq!(
        check_batch_results(&vec![allow(); 4], 3, gate),
        Err(BatchResultsError::ResultCountOutOfRange {
            items: 3,
            results: 4
        })
    );
    assert_eq!(
        check_batch_results(&[deny(), allow()], 3, gate),
        Err(BatchResultsError::ContinuedAfterDeny { index: 0 })
    );
    assert_eq!(
        check_batch_results(&[allow(), allow()], 3, gate),
        Err(BatchResultsError::StoppedWithoutDeny { index: 1 })
    );
}

#[test]
fn every_denial_must_carry_an_error_code() {
    let mut empty_code = deny();
    empty_code.context.deny_reason = Some(DenyReason::new(""));
    for semantic in [
        EvaluationsSemantic::ExecuteAll,
        EvaluationsSemantic::DenyOnFirstDeny,
    ] {
        assert_eq!(
            check_batch_results(&[allow(), deny_without_code()], 2, semantic),
            Err(BatchResultsError::DenialWithoutErrorCode { index: 1 })
        );
        assert_eq!(
            check_batch_results(&[allow(), empty_code.clone()], 2, semantic),
            Err(BatchResultsError::DenialWithoutErrorCode { index: 1 })
        );
    }
}

#[test]
fn into_results_returns_only_the_evaluated_prefix() {
    let response = EvaluationBatchResponse {
        evaluations: vec![allow(), deny()],
    };
    let results = response
        .into_results(4, EvaluationsSemantic::DenyOnFirstDeny)
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].decision);
    assert!(!results[1].decision);
}

#[test]
fn into_results_rejects_a_malformed_response_as_internal() {
    let short = EvaluationBatchResponse {
        evaluations: vec![allow()],
    };
    let err = short
        .into_results(2, EvaluationsSemantic::ExecuteAll)
        .unwrap_err();
    assert!(matches!(err, CanonicalError::Internal { .. }), "{err:?}");

    let uncoded = EvaluationBatchResponse {
        evaluations: vec![deny_without_code()],
    };
    let err = uncoded
        .into_results(1, EvaluationsSemantic::ExecuteAll)
        .unwrap_err();
    assert!(matches!(err, CanonicalError::Internal { .. }), "{err:?}");
}

// ── sequential helper ───────────────────────────────────────────────────────

/// Denies `delete`, allows everything else, fails `explode`; counts calls.
#[derive(Default)]
struct Scripted {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl AuthZResolverApi for Scripted {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match req.action.name.as_str() {
            "delete" => Ok(deny()),
            "explode" => Err(CanonicalError::service_unavailable().create()),
            _ => Ok(allow()),
        }
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: EvaluationBatchRequest,
    ) -> Result<EvaluationBatchResponse, CanonicalError> {
        evaluate_batch_sequentially(self, ctx, req).await
    }
}

fn with_action(name: &str) -> EvaluationBatchItem {
    EvaluationBatchItem {
        action: Some(action(name)),
        ..item()
    }
}

#[tokio::test]
async fn sequential_execute_all_evaluates_every_item_in_order() {
    let pdp = Scripted::default();
    let req = batch(vec![
        with_action("read"),
        with_action("delete"),
        with_action("read"),
    ]);
    let response = pdp
        .evaluate_batch(PlatformSecurityContext::outbound_marker(), req)
        .await
        .unwrap();
    let decisions: Vec<bool> = response.evaluations.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [true, false, true]);
    assert_eq!(pdp.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn sequential_deny_on_first_deny_stops_after_the_denial() {
    let pdp = Scripted::default();
    let mut req = batch(vec![
        with_action("read"),
        with_action("delete"),
        with_action("read"),
    ]);
    req.options =
        EvaluationBatchOptions::default().with_semantic(EvaluationsSemantic::DenyOnFirstDeny);
    let response = pdp
        .evaluate_batch(PlatformSecurityContext::outbound_marker(), req)
        .await
        .unwrap();
    assert_eq!(response.evaluations.len(), 2);
    assert_eq!(pdp.calls.load(Ordering::SeqCst), 2);
    let results = response
        .into_results(3, EvaluationsSemantic::DenyOnFirstDeny)
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(!results[1].decision);
}

#[tokio::test]
async fn sequential_fails_the_whole_batch_on_one_failing_item() {
    let pdp = Scripted::default();
    let req = batch(vec![
        with_action("read"),
        with_action("explode"),
        with_action("read"),
    ]);
    let err = pdp
        .evaluate_batch(PlatformSecurityContext::outbound_marker(), req)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn sequential_rejects_an_invalid_batch_before_evaluating() {
    let pdp = Scripted::default();
    let err = pdp
        .evaluate_batch(
            PlatformSecurityContext::outbound_marker(),
            batch(Vec::new()),
        )
        .await
        .unwrap_err();
    assert_eq!(field_violation(err).1, "EMPTY_BATCH");
    assert_eq!(pdp.calls.load(Ordering::SeqCst), 0);
}

// ── concurrent fan-out helper ───────────────────────────────────────────────

fn requests(actions: &[&str]) -> Vec<EvaluationRequest> {
    batch(actions.iter().map(|a| with_action(a)).collect())
        .resolve(None)
        .unwrap()
        .0
}

/// Evaluates `read*` as allow and `delete` as deny after a delay that makes
/// earlier items finish last; tracks started and in-flight counts.
#[derive(Default)]
struct Probe {
    started: AtomicUsize,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl Probe {
    async fn evaluate(
        self: Arc<Self>,
        request: EvaluationRequest,
        delay: Duration,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        // Decrements on completion and on cancellation alike.
        let _in_flight = InFlight(&self.in_flight);
        tokio::time::sleep(delay).await;
        match request.action.name.as_str() {
            "delete" => Ok(deny()),
            "explode" => Err(AuthZResolverError::ServiceUnavailable("down".to_owned())),
            _ => Ok(allow()),
        }
    }
}

struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn fan_out_preserves_request_order_and_bounds_concurrency() {
    let probe = Arc::new(Probe::default());
    let actions = ["read", "delete", "read", "read", "delete", "read"];
    let total = actions.len();
    let mut delays = (0..total)
        .rev()
        .map(|i| Duration::from_millis(5 * i as u64));
    let results = evaluate_batch_concurrently(
        requests(&actions),
        EvaluationsSemantic::ExecuteAll,
        NonZeroUsize::new(2).unwrap(),
        &Semaphore::new(64),
        |request| Arc::clone(&probe).evaluate(request, delays.next().unwrap()),
    )
    .await
    .unwrap();
    let decisions: Vec<bool> = results.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [true, false, true, true, false, true]);
    assert_eq!(probe.started.load(Ordering::SeqCst), total);
    assert_eq!(probe.max_in_flight.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn fan_out_deny_on_first_deny_stops_and_cancels_later_items() {
    let probe = Arc::new(Probe::default());
    let results = evaluate_batch_concurrently(
        requests(&["read", "delete", "read", "read", "read"]),
        EvaluationsSemantic::DenyOnFirstDeny,
        NonZeroUsize::new(2).unwrap(),
        &Semaphore::new(64),
        |request| Arc::clone(&probe).evaluate(request, Duration::from_millis(5)),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 2);
    assert!(!results[1].decision);
    // At most `max_concurrency - 1` items past the stopping item had started.
    assert!(probe.started.load(Ordering::SeqCst) <= 3);
    // Dropping the stream cancelled the evaluation still in flight.
    assert_eq!(probe.in_flight.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fan_out_fails_the_whole_batch_on_one_failing_item() {
    let probe = Arc::new(Probe::default());
    let err = evaluate_batch_concurrently(
        requests(&["read", "explode", "read"]),
        EvaluationsSemantic::ExecuteAll,
        NonZeroUsize::new(3).unwrap(),
        &Semaphore::new(64),
        |request| Arc::clone(&probe).evaluate(request, Duration::from_millis(1)),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, AuthZResolverError::ServiceUnavailable(msg) if msg.starts_with("batch item 1:")),
        "the error keeps its variant and names the failing item: {err:?}"
    );
}

#[tokio::test]
async fn fan_out_holds_an_in_flight_permit_per_item_only_while_it_runs() {
    let probe = Arc::new(Probe::default());
    let in_flight = Semaphore::new(1);
    let results = evaluate_batch_concurrently(
        requests(&["read", "read", "read", "read"]),
        EvaluationsSemantic::ExecuteAll,
        NonZeroUsize::new(3).unwrap(),
        &in_flight,
        |request| Arc::clone(&probe).evaluate(request, Duration::from_millis(2)),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 4);
    // One permit process-wide: never more than one item runs, whatever the
    // batch's own bound.
    assert_eq!(probe.max_in_flight.load(Ordering::SeqCst), 1);
    assert_eq!(in_flight.available_permits(), 1, "every permit is returned");
}

#[tokio::test]
async fn fan_out_takes_up_permits_as_other_batches_free_them() {
    let probe = Arc::new(Probe::default());
    let in_flight = Semaphore::new(2);
    // Another batch holds one of the two permits when this batch starts.
    let other = in_flight.try_acquire().unwrap();
    let release = async {
        tokio::time::sleep(Duration::from_millis(15)).await;
        drop(other);
    };
    let batch = evaluate_batch_concurrently(
        requests(&["read"; 8]),
        EvaluationsSemantic::ExecuteAll,
        NonZeroUsize::new(2).unwrap(),
        &in_flight,
        |request| Arc::clone(&probe).evaluate(request, Duration::from_millis(10)),
    );
    let ((), results) = tokio::join!(release, batch);
    assert_eq!(results.unwrap().len(), 8);
    // Started with one free permit, then ran two items at once after the
    // other batch released its permit.
    assert_eq!(probe.max_in_flight.load(Ordering::SeqCst), 2);
    assert_eq!(in_flight.available_permits(), 2);
}

#[tokio::test]
async fn fan_out_returns_the_permits_of_items_cancelled_by_a_short_circuit() {
    let probe = Arc::new(Probe::default());
    let in_flight = Semaphore::new(4);
    let results = evaluate_batch_concurrently(
        requests(&["delete", "read", "read", "read", "read"]),
        EvaluationsSemantic::DenyOnFirstDeny,
        NonZeroUsize::new(4).unwrap(),
        &in_flight,
        |request| Arc::clone(&probe).evaluate(request, Duration::from_millis(5)),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(in_flight.available_permits(), 4);
}

// ── decoding cap ────────────────────────────────────────────────────────────

#[test]
fn decoding_stops_past_the_hard_item_cap() {
    let body = |n: usize| json!({ "evaluations": vec![json!({}); n] });
    let at_cap: EvaluationBatchRequest = serde_json::from_value(body(MAX_BATCH_ITEMS)).unwrap();
    assert_eq!(at_cap.evaluations.len(), MAX_BATCH_ITEMS);
    let err = serde_json::from_value::<EvaluationBatchRequest>(body(MAX_BATCH_ITEMS + 1))
        .unwrap_err()
        .to_string();
    assert!(err.contains("at most 1000"), "{err}");
}

#[test]
fn the_openapi_schema_caps_the_items() {
    use utoipa::PartialSchema;
    let schema = serde_json::to_value(EvaluationBatchRequest::schema()).unwrap();
    assert_eq!(
        schema["properties"]["evaluations"]["maxItems"],
        json!(MAX_BATCH_ITEMS)
    );
}

// ── result shape: property ──────────────────────────────────────────────────

/// What `check_batch_results` must accept, stated directly: a full list for
/// `execute_all`; for `deny_on_first_deny` a non-empty prefix of allows that,
/// when shorter than the request, ends in a denial; and in both, every denial
/// carries an error code.
fn valid_shape(
    results: &[EvaluationResponse],
    expected: usize,
    semantic: EvaluationsSemantic,
) -> bool {
    let coded = results.iter().all(|r| {
        r.decision
            || r.context
                .deny_reason
                .as_ref()
                .is_some_and(|d| !d.error_code.is_empty())
    });
    let shape = match semantic {
        EvaluationsSemantic::ExecuteAll => results.len() == expected,
        EvaluationsSemantic::DenyOnFirstDeny => {
            let n = results.len();
            let in_range = if expected == 0 {
                n == 0
            } else {
                (1..=expected).contains(&n)
            };
            let allows_before_last = results.iter().take(n.saturating_sub(1)).all(|r| r.decision);
            let stops_on_deny = n == expected || results.last().is_some_and(|r| !r.decision);
            in_range && allows_before_last && stops_on_deny
        }
    };
    coded && shape
}

proptest::proptest! {
    #[test]
    fn check_batch_results_accepts_exactly_the_valid_shapes(
        kinds in proptest::collection::vec(0u8..3, 0..7),
        expected in 0usize..7,
        deny_on_first_deny: bool,
    ) {
        let results: Vec<EvaluationResponse> = kinds
            .iter()
            .map(|k| match k {
                0 => allow(),
                1 => deny(),
                _ => deny_without_code(),
            })
            .collect();
        let semantic = if deny_on_first_deny {
            EvaluationsSemantic::DenyOnFirstDeny
        } else {
            EvaluationsSemantic::ExecuteAll
        };
        let accepted = check_batch_results(&results, expected, semantic).is_ok();
        proptest::prop_assert_eq!(accepted, valid_shape(&results, expected, semantic));
        // Whatever is accepted converts unchanged, and a short prefix always
        // ends in a denial, so no unevaluated item follows an allow.
        if accepted {
            let converted = EvaluationBatchResponse { evaluations: results.clone() }
                .into_results(expected, semantic)
                .unwrap();
            let decisions = |r: &[EvaluationResponse]| r.iter().map(|e| e.decision).collect::<Vec<_>>();
            proptest::prop_assert_eq!(decisions(&converted), decisions(&results));
            if converted.len() < expected {
                proptest::prop_assert!(converted.last().is_some_and(|r| !r.decision));
            }
        }
    }
}
