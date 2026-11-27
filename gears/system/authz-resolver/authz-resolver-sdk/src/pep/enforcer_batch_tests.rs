//! `PolicyEnforcer` batch calls: equivalence with single checks,
//! request assembly, short-circuit results and whole-call failures.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use secrecy::ExposeSecret;
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, pep_properties};

use super::*;
use crate::batch::{EvaluationBatchResponse, evaluate_batch_sequentially};
use crate::constraints::{Constraint, EqPredicate, InPredicate, Predicate};
use crate::models::{DenyReason, EvaluationResponseContext};

const TENANT: Uuid = Uuid::from_u128(0x1111);
const SUBJECT: Uuid = Uuid::from_u128(0x2222);
const ROW: Uuid = Uuid::from_u128(0x3333);
const OTHER_ROW: Uuid = Uuid::from_u128(0x4444);

static USERS: ResourceType = ResourceType::from_static(
    gts_id!("cf.core.users.user.v1~"),
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);
static EVENTS: ResourceType = ResourceType::from_static(
    gts_id!("cf.core.events.event.v1~"),
    &[pep_properties::OWNER_TENANT_ID],
);

/// Decides by action: `delete` is denied with a reason, `nothing` permits an
/// empty scope, `unsupported` permits on a property the PEP did not
/// advertise, everything else permits the subject's tenant. Records the
/// batch requests it receives and how many items it evaluated.
#[derive(Default)]
struct ScriptedPdp {
    batches: Mutex<Vec<(serde_json::Value, Option<String>)>>,
    evaluated: AtomicUsize,
}

fn permit(predicates: Vec<Predicate>) -> EvaluationResponse {
    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint { predicates }],
            deny_reason: None,
        },
    }
}

#[async_trait]
impl AuthZResolverApi for ScriptedPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.evaluated.fetch_add(1, Ordering::SeqCst);
        Ok(match req.action.name.as_str() {
            "delete" => EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: Some(
                        DenyReason::new(DenyReason::INSUFFICIENT_PERMISSIONS)
                            .with_details("no delete"),
                    ),
                },
            },
            "nothing" => permit(vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                Vec::<Uuid>::new(),
            ))]),
            "unsupported" => permit(vec![Predicate::Eq(EqPredicate::new(
                "not_advertised",
                TENANT,
            ))]),
            _ => permit(vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [TENANT],
            ))]),
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: EvaluationBatchRequest,
    ) -> Result<EvaluationBatchResponse, CanonicalError> {
        let token = req
            .context
            .as_ref()
            .and_then(|c| c.bearer_token.as_ref())
            .map(|t| t.expose_secret().to_owned());
        self.batches
            .lock()
            .unwrap()
            .push((serde_json::to_value(&req).unwrap(), token));
        evaluate_batch_sequentially(self, ctx, req).await
    }
}

/// Batch and single outcomes agree: the same scope, or the same refusal.
fn assert_same_outcome(
    batch: &Result<AccessScope, AccessCheckError>,
    single: &Result<AccessScope, EnforcerError>,
    action: &str,
) {
    match (batch, single) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "check `{action}`"),
        (
            Err(AccessCheckError::AccessDenied(AccessDenial::Pdp { reason: a })),
            Err(EnforcerError::AccessDenied(AccessDenial::Pdp { reason: b })),
        ) => assert_eq!(
            a.as_ref().map(|r| &r.error_code),
            b.as_ref().map(|r| &r.error_code),
            "check `{action}`"
        ),
        (
            Err(AccessCheckError::AccessDenied(AccessDenial::ScopeMatchesNothing)),
            Err(EnforcerError::AccessDenied(AccessDenial::ScopeMatchesNothing)),
        )
        | (Err(AccessCheckError::CompileFailed(_)), Err(EnforcerError::CompileFailed(_))) => {}
        _ => panic!("check `{action}`: batch {batch:?} != single {single:?}"),
    }
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(TENANT)
        .bearer_token("bearer-secret".to_owned())
        .build()
        .unwrap()
}

fn check<'a>(resource: &'a ResourceType, action: &'a str, id: Option<Uuid>) -> AccessCheck<'a> {
    AccessCheck {
        resource,
        action,
        resource_id: id,
        request: AccessRequest::default(),
    }
}

fn pdp() -> (PolicyEnforcer, Arc<ScriptedPdp>) {
    let pdp = Arc::new(ScriptedPdp::default());
    (PolicyEnforcer::new(Arc::clone(&pdp) as _), pdp)
}

#[tokio::test]
async fn each_result_equals_the_single_check_on_the_same_inputs() {
    let (enforcer, _) = pdp();
    let checks = [
        check(&USERS, "read", Some(ROW)),
        check(&USERS, "delete", Some(ROW)),
        check(&USERS, "nothing", None),
        AccessCheck {
            request: AccessRequest::new().allow_empty_scope(true),
            ..check(&USERS, "nothing", None)
        },
        check(&EVENTS, "unsupported", None),
        check(&EVENTS, "read", Some(OTHER_ROW)),
    ];

    let batch = enforcer
        .access_scope_execute_all(&ctx(), &checks)
        .await
        .unwrap();

    assert_eq!(batch.len(), checks.len());
    for (check, result) in checks.iter().zip(&batch) {
        let single = enforcer
            .access_scope_with(
                &ctx(),
                check.resource,
                check.action,
                check.resource_id,
                &check.request,
            )
            .await;
        assert_same_outcome(result, &single, check.action);
    }
    assert!(batch[0].is_ok());
    assert!(matches!(
        &batch[1],
        Err(AccessCheckError::AccessDenied(AccessDenial::Pdp { reason: Some(r) }))
            if r.error_code == DenyReason::INSUFFICIENT_PERMISSIONS
    ));
    assert!(matches!(
        &batch[2],
        Err(AccessCheckError::AccessDenied(
            AccessDenial::ScopeMatchesNothing
        ))
    ));
    assert!(batch[3].is_ok());
    assert!(matches!(&batch[4], Err(AccessCheckError::CompileFailed(_))));
}

#[tokio::test]
async fn an_empty_list_of_checks_makes_no_call() {
    let (enforcer, pdp) = pdp();
    assert!(
        enforcer
            .access_scope_execute_all(&ctx(), &[])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(pdp.batches.lock().unwrap().is_empty());
}

#[tokio::test]
async fn shared_action_and_context_are_sent_once_with_the_subject() {
    let (enforcer, pdp) = pdp();
    let checks = [
        check(&USERS, "read", Some(ROW)),
        check(&USERS, "read", Some(OTHER_ROW)),
    ];
    enforcer
        .access_scope_execute_all(&ctx(), &checks)
        .await
        .unwrap();

    let batches = pdp.batches.lock().unwrap();
    assert_eq!(batches.len(), 1, "one PDP call per batch");
    let (wire, token) = &batches[0];
    assert_eq!(wire["subject"]["id"], serde_json::json!(SUBJECT));
    assert_eq!(wire["action"]["name"], "read");
    assert!(wire["context"].is_object());
    assert!(wire.get("resource").is_none());
    assert!(wire.get("options").is_none(), "execute_all is the default");
    for (item, id) in wire["evaluations"]
        .as_array()
        .unwrap()
        .iter()
        .zip([ROW, OTHER_ROW])
    {
        assert!(item.get("subject").is_none());
        assert!(item.get("action").is_none());
        assert!(item.get("context").is_none());
        assert_eq!(item["resource"]["id"], serde_json::json!(id));
    }
    // Not serialized, but carried to the in-process PDP on the hoisted context.
    assert!(!wire.to_string().contains("bearer-secret"));
    assert_eq!(token.as_deref(), Some("bearer-secret"));
}

#[tokio::test]
async fn differing_action_and_context_are_sent_per_item() {
    let (enforcer, pdp) = pdp();
    let checks = [
        check(&USERS, "read", Some(ROW)),
        check(&EVENTS, "update", None),
    ];
    enforcer
        .access_scope_execute_all(&ctx(), &checks)
        .await
        .unwrap();

    let batches = pdp.batches.lock().unwrap();
    let (wire, _) = &batches[0];
    assert!(wire.get("action").is_none());
    assert!(wire.get("context").is_none());
    assert_eq!(wire["subject"]["id"], serde_json::json!(SUBJECT));
    let items = wire["evaluations"].as_array().unwrap();
    assert_eq!(items[0]["action"]["name"], "read");
    assert_eq!(items[1]["action"]["name"], "update");
    // Supported properties differ per resource type, so the contexts differ.
    assert_eq!(
        items[0]["context"]["supported_properties"],
        serde_json::json!(["owner_tenant_id", "id"])
    );
    assert_eq!(
        items[1]["context"]["supported_properties"],
        serde_json::json!(["owner_tenant_id"])
    );
}

#[tokio::test]
async fn deny_on_first_deny_refuses_at_the_first_denial_and_stops() {
    let (enforcer, pdp) = pdp();
    let checks = [
        check(&USERS, "read", Some(ROW)),
        check(&USERS, "delete", Some(ROW)),
        check(&USERS, "read", Some(OTHER_ROW)),
    ];

    let err = enforcer
        .access_scope_deny_on_first_deny(&ctx(), &checks)
        .await
        .unwrap_err();

    assert!(
        matches!(
            &err,
            EnforcerError::AccessDenied(AccessDenial::Pdp { reason: Some(r) })
                if r.error_code == DenyReason::INSUFFICIENT_PERMISSIONS
        ),
        "{err:?}"
    );
    assert_eq!(pdp.evaluated.load(Ordering::SeqCst), 2);
    let batches = pdp.batches.lock().unwrap();
    assert_eq!(
        batches[0].0["options"],
        serde_json::json!({ "evaluations_semantic": "deny_on_first_deny" })
    );
}

#[tokio::test]
async fn deny_on_first_deny_returns_one_scope_per_check_when_all_are_permitted() {
    let (enforcer, pdp) = pdp();
    let checks = [
        check(&USERS, "delete_soft", Some(ROW)),
        check(&USERS, "delete_soft", Some(OTHER_ROW)),
        check(&EVENTS, "read", None),
    ];

    let scopes = enforcer
        .access_scope_deny_on_first_deny(&ctx(), &checks)
        .await
        .unwrap();

    assert_eq!(scopes.len(), checks.len());
    for (check, scope) in checks.iter().zip(&scopes) {
        let single = enforcer
            .access_scope_with(
                &ctx(),
                check.resource,
                check.action,
                check.resource_id,
                &check.request,
            )
            .await
            .unwrap();
        assert_eq!(scope, &single, "check `{}`", check.action);
    }
    assert_eq!(pdp.evaluated.load(Ordering::SeqCst), 3 + checks.len());
}

#[tokio::test]
async fn deny_on_first_deny_refuses_a_permit_that_does_not_compile() {
    let (enforcer, _) = pdp();
    let checks = [
        check(&USERS, "read", Some(ROW)),
        check(&EVENTS, "unsupported", None),
        check(&USERS, "nothing", None),
    ];

    let err = enforcer
        .access_scope_deny_on_first_deny(&ctx(), &checks)
        .await
        .unwrap_err();

    // The first failing check in order wins, not the later empty scope.
    assert!(matches!(err, EnforcerError::CompileFailed(_)), "{err:?}");
}

#[tokio::test]
async fn deny_on_first_deny_with_no_checks_makes_no_call() {
    let (enforcer, pdp) = pdp();
    assert!(
        enforcer
            .access_scope_deny_on_first_deny(&ctx(), &[])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(pdp.batches.lock().unwrap().is_empty());
}

/// Answers every batch with `answer`, or never answers when `None`.
struct FixedBatch(Option<Result<EvaluationBatchResponse, CanonicalError>>);

#[async_trait]
impl AuthZResolverApi for FixedBatch {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        unreachable!("the batch helper makes one batch call")
    }

    async fn evaluate_batch(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationBatchRequest,
    ) -> Result<EvaluationBatchResponse, CanonicalError> {
        match &self.0 {
            Some(answer) => answer.clone(),
            None => std::future::pending().await,
        }
    }
}

fn two_checks() -> [AccessCheck<'static>; 2] {
    [
        check(&USERS, "read", Some(ROW)),
        check(&USERS, "read", Some(OTHER_ROW)),
    ]
}

#[tokio::test]
async fn a_response_of_the_wrong_shape_fails_the_whole_call() {
    let one_result = EvaluationBatchResponse {
        evaluations: vec![permit(vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            [TENANT],
        ))])],
    };
    let enforcer = PolicyEnforcer::new(Arc::new(FixedBatch(Some(Ok(one_result)))));
    let err = enforcer
        .access_scope_execute_all(&ctx(), &two_checks())
        .await
        .unwrap_err();
    assert!(matches!(&err, CanonicalError::Internal { .. }), "{err:?}");
}

#[tokio::test]
async fn a_denial_without_an_error_code_fails_the_whole_call() {
    let uncoded = EvaluationResponse {
        decision: false,
        context: EvaluationResponseContext::default(),
    };
    let enforcer = PolicyEnforcer::new(Arc::new(FixedBatch(Some(Ok(EvaluationBatchResponse {
        evaluations: vec![uncoded.clone(), uncoded],
    })))));
    let err = enforcer
        .access_scope_execute_all(&ctx(), &two_checks())
        .await
        .unwrap_err();
    assert!(matches!(err, CanonicalError::Internal { .. }), "{err:?}");
}

#[tokio::test]
async fn a_whole_request_error_fails_the_whole_call() {
    let enforcer = PolicyEnforcer::new(Arc::new(FixedBatch(Some(Err(
        CanonicalError::service_unavailable().create(),
    )))));
    let err = enforcer
        .access_scope_execute_all(&ctx(), &two_checks())
        .await
        .unwrap_err();
    // `?` into `EnforcerError` keeps the PDP-unavailable classification.
    assert!(EnforcerError::from(err).is_pdp_unavailable());

    let err = enforcer
        .access_scope_deny_on_first_deny(&ctx(), &two_checks())
        .await
        .unwrap_err();
    assert!(err.is_pdp_unavailable(), "{err:?}");
}

#[tokio::test]
async fn the_evaluation_deadline_bounds_the_batch_call() {
    let enforcer = PolicyEnforcer::new(Arc::new(FixedBatch(None)))
        .with_deadline(std::time::Duration::from_millis(50));
    let err = enforcer
        .access_scope_execute_all(&ctx(), &two_checks())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_shared_action_with_differing_contexts_hoists_only_the_action() {
    let (enforcer, pdp) = pdp();
    let checks = [
        check(&USERS, "read", Some(ROW)),
        check(&EVENTS, "read", None),
    ];
    let results = enforcer
        .access_scope_execute_all(&ctx(), &checks)
        .await
        .unwrap();
    assert!(results.iter().all(Result::is_ok), "{results:?}");

    let batches = pdp.batches.lock().unwrap();
    let (wire, _) = &batches[0];
    assert_eq!(wire["action"]["name"], "read");
    assert!(wire.get("context").is_none());
    for item in wire["evaluations"].as_array().unwrap() {
        assert!(item.get("action").is_none());
        assert!(item["context"].is_object());
    }
}

#[tokio::test]
async fn differing_actions_with_a_shared_context_hoist_only_the_context() {
    let (enforcer, pdp) = pdp();
    let checks = [
        check(&USERS, "read", Some(ROW)),
        check(&USERS, "update", Some(OTHER_ROW)),
    ];
    let results = enforcer
        .access_scope_execute_all(&ctx(), &checks)
        .await
        .unwrap();
    assert!(results.iter().all(Result::is_ok), "{results:?}");

    let batches = pdp.batches.lock().unwrap();
    let (wire, token) = &batches[0];
    assert!(wire.get("action").is_none());
    assert!(wire["context"].is_object());
    assert_eq!(token.as_deref(), Some("bearer-secret"));
    let items = wire["evaluations"].as_array().unwrap();
    assert_eq!(items[0]["action"]["name"], "read");
    assert_eq!(items[1]["action"]["name"], "update");
    for item in items {
        assert!(item.get("context").is_none());
    }
}
