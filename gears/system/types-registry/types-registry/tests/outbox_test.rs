#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use types_registry::domain::selection::FieldSelection;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::outbox::{MessageResult, OutboxHandle, OutboxMessage};
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use uuid::Uuid;

use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums::{
    LifecycleStatus, OperationItemStatus, OperationKind, OperationStatus,
};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::Stores;
use types_registry::domain::ports::metrics::{AdmissionMetrics, DeliveryOutcome};
use types_registry::domain::registry_service::{EntityKey, RegistryService};
use types_registry::infra::outbox::{AdmissionHandler, OutboxDispatch};
use types_registry::infra::storage::repo::OperationRepo;

mod common;
use common::{await_delivery, metrics, stores, test_db_with_outbox};

const NOW: OffsetDateTime = datetime!(2026-09-14 12:00:00 UTC);

const TARGET: &str = gts_id!("cf.core.outbox.target.v1~");

fn schema(gts_id: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn registration(idempotency_key: &str, gts_id: &str) -> SubmitRequest {
    SubmitRequest {
        idempotency_key: Some(idempotency_key.to_owned()),
        kind: OperationKind::Registration,
        dry_run: false,
        candidates: vec![Candidate {
            gts_id: gts_id.to_owned(),
            content: Some(schema(gts_id)),
            expected_resource_version: None,
            force: false,
        }],
    }
}

fn service_with(
    db: &Arc<DBProvider<DbError>>,
    ports: Arc<dyn Stores>,
    dispatch: Arc<dyn OperationDispatch>,
) -> Arc<RegistryService> {
    Arc::new(RegistryService::new(
        db.db(),
        ports,
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        dispatch,
        metrics(),
    ))
}

fn service(
    db: &Arc<DBProvider<DbError>>,
    ports: Arc<dyn Stores>,
) -> (Arc<RegistryService>, Arc<OutboxDispatch>) {
    let dispatch = Arc::new(OutboxDispatch::new());
    let registry = service_with(
        db,
        ports,
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
    );
    (registry, dispatch)
}

const LAST_ATTEMPT: i16 = 2;

const MAX_ATTEMPTS: u32 = LAST_ATTEMPT as u32 + 1;

const PAST_BUDGET: i16 = LAST_ATTEMPT + 1;

const SENSITIVE_CAUSE: &str = "could not execute UPDATE on \
     postgres://registry:hunter2@db.internal:5432/app (authorization: Bearer \
     eyJhbGciOiJIUzI1NiJ9.super-secret): row was {\"ssn\": \"123-45-6789\"}";

const SENSITIVE_FRAGMENT: &str = "hunter2";

/// Serializes the tests that read captured output: they share one subscriber.
static LOG_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn capture() -> &'static common::CapturedLog {
    static CAPTURE: std::sync::OnceLock<common::CapturedLog> = std::sync::OnceLock::new();
    CAPTURE.get_or_init(common::CapturedLog::install_global)
}

fn service_with_operation_timeout(
    db: &Arc<DBProvider<DbError>>,
    ports: Arc<dyn Stores>,
    operation_timeout: std::time::Duration,
) -> Arc<RegistryService> {
    let mut config = TypesRegistryConfig::default();
    config.worker.operation_timeout = operation_timeout;
    Arc::new(RegistryService::new(
        db.db(),
        ports,
        RegistrationPolicy::default(),
        config,
        Arc::new(common::NoDispatch),
        metrics(),
    ))
}

fn service_recording_deliveries(
    db: &Arc<DBProvider<DbError>>,
    ports: Arc<dyn Stores>,
) -> (Arc<RegistryService>, Arc<common::RecordingDeliveryMetrics>) {
    service_recording_with_dispatch(db, ports, Arc::new(common::NoDispatch))
}

/// Recording instruments plus a real dispatch, for a test that needs both the
/// counted outcomes and a live pipeline.
fn service_recording_with_dispatch(
    db: &Arc<DBProvider<DbError>>,
    ports: Arc<dyn Stores>,
    dispatch: Arc<dyn OperationDispatch>,
) -> (Arc<RegistryService>, Arc<common::RecordingDeliveryMetrics>) {
    let recorded = Arc::new(common::RecordingDeliveryMetrics::default());
    let registry = Arc::new(RegistryService::new(
        db.db(),
        ports,
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        dispatch,
        Arc::clone(&recorded) as Arc<dyn AdmissionMetrics>,
    ));
    (registry, recorded)
}

fn service_without_dispatch(
    db: &Arc<DBProvider<DbError>>,
    ports: Arc<dyn Stores>,
) -> Arc<RegistryService> {
    service_with(db, ports, Arc::new(common::NoDispatch))
}

async fn started(
    db: &Arc<DBProvider<DbError>>,
    ports: Arc<dyn Stores>,
) -> (Arc<RegistryService>, OutboxHandle) {
    let (registry, dispatch) = service(db, ports);
    let handle = types_registry::infra::outbox::start(db.db(), &registry, &dispatch)
        .await
        .expect("start the admission outbox");
    (registry, handle)
}

#[tokio::test]
async fn the_handler_admits_the_operation_its_payload_names() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, stores());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");
    assert_eq!(
        accepted.status,
        OperationStatus::Pending,
        "outbox mode must not admit in the caller's task",
    );

    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), 0)
        .await;
    assert!(matches!(result, MessageResult::Ok), "got: {result:?}");

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(operation.status, OperationStatus::Completed);
    assert_eq!(operation.items[0].status, OperationItemStatus::Succeeded);
}

#[tokio::test]
async fn a_duplicate_delivery_changes_nothing() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, stores());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");
    let payload = accepted.operation_id.to_string();

    let first = handler.admit_payload(payload.as_bytes(), 0).await;
    assert!(matches!(first, MessageResult::Ok), "got: {first:?}");
    let after_first = registry
        .entity(&EntityKey::GtsId(TARGET.to_owned()), FieldSelection::full())
        .await
        .expect("read")
        .expect("the entity exists");

    let second = handler.admit_payload(payload.as_bytes(), 0).await;
    assert!(
        matches!(second, MessageResult::Ok),
        "a redelivery is a no-op, not a failure: {second:?}",
    );

    let after_second = registry
        .entity(&EntityKey::GtsId(TARGET.to_owned()), FieldSelection::full())
        .await
        .expect("read")
        .expect("the entity exists");
    assert_eq!(
        after_second.origin.map(|o| o.resource_version),
        after_first.origin.map(|o| o.resource_version),
        "a redelivery must not advance resource_version",
    );
    assert_eq!(after_second.lifecycle_status, LifecycleStatus::Active);

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(operation.items.len(), 1);
    assert_eq!(operation.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(operation.items[0].resource_version, Some(1));
}

#[tokio::test]
async fn a_payload_that_is_not_an_operation_uuid_is_rejected() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, stores());
    let handler = AdmissionHandler::new(registry, MAX_ATTEMPTS);

    let result = handler.admit_payload(b"not-a-uuid", 0).await;
    assert!(
        matches!(result, MessageResult::Reject(_)),
        "got: {result:?}"
    );
}

#[tokio::test]
async fn a_message_naming_no_operation_is_rejected() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, stores());
    let handler = AdmissionHandler::new(registry, MAX_ATTEMPTS);

    let result = handler
        .admit_payload(Uuid::new_v4().to_string().as_bytes(), 0)
        .await;
    assert!(
        matches!(result, MessageResult::Reject(_)),
        "got: {result:?}"
    );
}

#[tokio::test]
async fn a_storage_failure_during_admission_is_retried() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, common::TestStores::failing_completion());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");

    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), 0)
        .await;
    assert!(matches!(result, MessageResult::Retry), "got: {result:?}");

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_ne!(
        operation.status,
        OperationStatus::Completed,
        "a retried message must leave the operation for the next delivery",
    );
}

#[tokio::test]
async fn a_transient_failure_on_the_last_attempt_is_terminalized_and_acked() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, common::TestStores::failing_item_success());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");

    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), LAST_ATTEMPT)
        .await;
    assert!(
        matches!(result, MessageResult::Ok),
        "the same failure that is retried on attempt 0 must terminalize and ack once \
         the budget is spent, or the partition never advances: {result:?}",
    );

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(
        operation.status,
        OperationStatus::Completed,
        "a failed operation must not stay non-terminal",
    );
    assert_eq!(operation.items[0].status, OperationItemStatus::Failed);
    let error = stored_error(&operation.items[0]);
    assert_eq!(
        error["reason"],
        json!("system_failure"),
        "the reason must say admission stopped trying, not that the candidate was refused",
    );
}

#[tokio::test]
async fn an_admission_past_the_delivery_budget_is_terminalized_without_being_admitted() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, stores());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");
    assert_eq!(accepted.status, OperationStatus::Pending);

    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), PAST_BUDGET)
        .await;
    assert!(
        matches!(result, MessageResult::Ok),
        "a delivery past the budget must terminalize and leave the queue rather than \
         admit again: {result:?}",
    );

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(
        operation.status,
        OperationStatus::Completed,
        "the operation must be terminal, not left running for a delivery that will not come",
    );
    assert_eq!(operation.items[0].status, OperationItemStatus::Failed);
    let error = stored_error(&operation.items[0]);
    assert_eq!(error["reason"], json!("system_failure"));

    assert!(
        registry
            .entity(&EntityKey::GtsId(TARGET.to_owned()), FieldSelection::full())
            .await
            .expect("read")
            .is_none(),
        "the handler must not have run admission for a message past its budget",
    );
}

#[tokio::test]
async fn a_delivery_past_the_budget_acks_an_operation_a_prior_pass_completed() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, stores());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");
    registry
        .admit(accepted.operation_id, NOW)
        .await
        .expect("a prior delivery admitted the operation");

    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), PAST_BUDGET)
        .await;
    assert!(
        matches!(result, MessageResult::Ok),
        "a completed operation must be acked without re-admission past the budget: {result:?}",
    );

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(operation.status, OperationStatus::Completed);
    assert_eq!(
        operation.items[0].status,
        OperationItemStatus::Succeeded,
        "the prior pass's outcome must survive the budget check unchanged",
    );
}

#[tokio::test]
async fn a_status_read_that_fails_past_the_budget_terminalizes_rather_than_retries() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, common::TestStores::failing_operation_read());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");

    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), PAST_BUDGET)
        .await;
    assert!(
        matches!(result, MessageResult::Ok),
        "an unreadable status past the budget must terminalize and leave the queue, \
         not come back to the same branch: {result:?}",
    );

    let unhooked = service_without_dispatch(&db, stores());
    let operation = unhooked
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(
        operation.status,
        OperationStatus::Completed,
        "acking the message is only allowed because the operation was terminalized",
    );
    assert_eq!(operation.items[0].status, OperationItemStatus::Failed);
    let error = stored_error(&operation.items[0]);
    assert_eq!(
        error["reason"],
        json!("system_failure"),
        "the item must say admission stopped, not that the candidate was refused",
    );
}

#[tokio::test]
async fn a_stalled_status_path_is_bounded_by_the_handler_as_a_whole() {
    const STALL: std::time::Duration = std::time::Duration::from_mins(1);
    const LEASE: std::time::Duration = std::time::Duration::from_secs(2);

    let db = test_db_with_outbox().await;
    let registry =
        service_with_operation_timeout(&db, common::TestStores::stalling_status_path(STALL), LEASE);
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");

    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), PAST_BUDGET)
        .await;
    let elapsed = started.elapsed();

    assert!(
        matches!(result, MessageResult::Retry),
        "a stall that terminalizes nothing must keep the message, past the budget or \
         not: the operation is the only thing that can end it: {result:?}",
    );
    assert!(
        elapsed < LEASE,
        "the handler took {elapsed:?} of a {LEASE:?} lease on a path stalled for {STALL:?}; \
         the read and the failure write after it must share one deadline, or two individually \
         safe budgets spend the lease twice and `timeout_at` decides instead",
    );
}

#[tokio::test]
async fn a_failure_write_after_a_slow_admission_stays_inside_the_delivery_deadline() {
    const LEASE: std::time::Duration = std::time::Duration::from_secs(1);
    const SPENT_ADMITTING: std::time::Duration = std::time::Duration::from_millis(700);
    const WRITE_STALL: std::time::Duration = std::time::Duration::from_millis(500);

    let db = test_db_with_outbox().await;
    let registry = service_with_operation_timeout(
        &db,
        common::TestStores::slow_admission_then_stalled_failure_write(SPENT_ADMITTING, WRITE_STALL),
        LEASE,
    );
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");

    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), LAST_ATTEMPT)
        .await;
    assert!(
        matches!(result, MessageResult::Retry),
        "a failure write cut off by the deadline leaves the operation non-terminal, so \
         the message must stay deliverable: {result:?}",
    );

    let unhooked = service_without_dispatch(&db, stores());
    let operation = unhooked
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(
        operation.status,
        OperationStatus::Pending,
        "the failure write must have been cut off by the deadline the delivery started \
         with; reaching a terminal status here means it ran on a budget measured from when \
         admission gave up, which is lease the delivery no longer had",
    );
}

#[tokio::test]
async fn an_overrunning_admission_is_cut_off_with_lease_left_to_fail_it() {
    const LEASE: std::time::Duration = std::time::Duration::from_secs(2);
    const SPENT_ADMITTING: std::time::Duration = std::time::Duration::from_millis(1600);
    const WRITE_STALL: std::time::Duration = std::time::Duration::from_millis(300);

    let db = test_db_with_outbox().await;
    let registry = service_with_operation_timeout(
        &db,
        common::TestStores::slow_admission_then_stalled_failure_write(SPENT_ADMITTING, WRITE_STALL),
        LEASE,
    );
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");

    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), LAST_ATTEMPT)
        .await;
    assert!(
        matches!(result, MessageResult::Ok),
        "an overrun on the last attempt terminalizes and acks: {result:?}",
    );

    let unhooked = service_without_dispatch(&db, stores());
    let operation = unhooked
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(
        operation.status,
        OperationStatus::Completed,
        "the reserve exists so this write lands: acking a message whose operation \
         stays non-terminal would leave no queued work to resume it",
    );
    let error = stored_error(&operation.items[0]);
    assert_eq!(
        error["error_code"], "admission_deadline_exceeded",
        "an overrun is its own diagnostic, not a failure code borrowed from admission",
    );
}

#[tokio::test]
async fn failing_an_operation_that_never_ran_still_terminalizes_it() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, common::TestStores::failing_running());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");
    assert_eq!(accepted.status, OperationStatus::Pending);

    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), LAST_ATTEMPT)
        .await;
    assert!(matches!(result, MessageResult::Ok), "got: {result:?}");

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(
        operation.status,
        OperationStatus::Completed,
        "an operation failed before its pass started must still reach a terminal status",
    );
    assert_eq!(operation.items[0].status, OperationItemStatus::Failed);
}

#[tokio::test]
async fn a_system_failure_records_the_cause_kind_and_never_the_drivers_own_text() {
    let _serial = LOG_TESTS.lock().await;
    let captured = capture();
    captured.clear();
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(
        &db,
        common::TestStores::failing_item_success_saying(SENSITIVE_CAUSE),
    );
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");
    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), LAST_ATTEMPT)
        .await;
    let log = captured.lines_for(accepted.operation_id);
    // The whole buffer: a leak anywhere is a leak.
    assert!(
        !captured.contains(SENSITIVE_CAUSE) && !captured.contains(SENSITIVE_FRAGMENT),
        "no part of the driver's own text may reach the operator log: {log}",
    );
    assert!(
        log.contains("cause_kind=\"worker\""),
        "the diagnostic the cause is replaced by must still be there, or the four \
         failures behind one error_code stay indistinguishable: {log}",
    );
    assert!(
        matches!(result, MessageResult::Ok),
        "an exhausted system failure terminalizes and acks: {result:?}",
    );

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    // The raw column: a parsed read would drop an unknown field carrying the cause.
    let conn = db.conn().expect("conn");
    let raw = OperationRepo::find_items(&conn, &common::allow_all(), accepted.operation_id)
        .await
        .expect("read items");
    let stored = raw[0]
        .error_payload
        .as_deref()
        .expect("a failed item carries a stored error payload");
    assert!(
        !stored.contains(SENSITIVE_CAUSE) && !stored.contains(SENSITIVE_FRAGMENT),
        "the injected cause must not reach the client-visible payload: {stored}",
    );
    let error: Value = serde_json::from_str(stored).expect("the stored payload is JSON");
    assert_eq!(
        stored_error(&operation.items[0]),
        error,
        "the poll reads it whole"
    );
    assert_eq!(error["reason"], json!("system_failure"));
    assert_eq!(error["error_code"], json!("storage_failure"));
    assert_eq!(
        error["operation_id"],
        json!(accepted.operation_id.to_string())
    );
}

#[tokio::test]
async fn invalid_scope_is_terminalized_on_the_first_delivery_with_a_system_diagnostic() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, common::TestStores::failing_running());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);
    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");
    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), 0)
        .await;
    assert!(
        matches!(result, MessageResult::Ok),
        "invalid scope cannot be repaired by redelivery, so the first delivery \
         terminalizes it rather than spending the budget: {result:?}",
    );
    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("exists");
    assert_eq!(operation.status, OperationStatus::Completed);
    assert_eq!(operation.items[0].status, OperationItemStatus::Failed);
    let error = stored_error(&operation.items[0]);
    assert_eq!(error["error_code"], json!("storage_failure"));
    assert_eq!(
        error["operation_id"],
        json!(accepted.operation_id.to_string())
    );
}

#[tokio::test]
async fn a_foreign_payload_type_is_rejected_by_the_handler() {
    let db = test_db_with_outbox().await;
    let registry = service_without_dispatch(&db, stores());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");

    let msg = OutboxMessage {
        partition_id: 0,
        seq: 1,
        payload: types_registry::infra::outbox::payload(accepted.operation_id),
        payload_type: "someone_else.message".to_owned(),
        created_at: chrono::DateTime::default(),
        attempts: 0,
    };

    let result = handler
        .handle_message(&msg, std::time::Duration::from_secs(30))
        .await;
    assert!(
        matches!(result, MessageResult::Reject(_)),
        "got: {result:?}"
    );

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(
        operation.status,
        OperationStatus::Pending,
        "refusing the envelope must not admit or terminalize the operation it names",
    );
}

#[test]
fn the_payload_is_the_operation_uuid_and_nothing_else() {
    let operation_id = Uuid::new_v4();
    let payload = types_registry::infra::outbox::payload(operation_id);

    assert_eq!(
        String::from_utf8(payload.clone()).expect("the payload is UTF-8"),
        operation_id.to_string(),
        "the payload is the canonical UUID text, which is what an operator reads \
         out of a dead-letter row",
    );
    assert_eq!(
        types_registry::infra::outbox::parse_payload(&payload).expect("round trip"),
        operation_id,
    );
    assert!(types_registry::infra::outbox::parse_payload(b"{}").is_err());
}

async fn seed_partition_operation(
    db: &Arc<DBProvider<DbError>>,
    id: Uuid,
    gts_id: &'static str,
    dispatch: Arc<dyn OperationDispatch>,
) {
    use types_registry::domain::admission::Precondition;
    use types_registry::domain::admission::fingerprint::{RequestFingerprint, ScopeHash};
    use types_registry::domain::enums::Plane;
    use types_registry::domain::ports::{NewOperation, NewOperationItem};

    let enqueue_dispatch = Arc::clone(&dispatch);
    let wake = db
        .db()
        .transaction_ref(|tx| {
            let dispatch = enqueue_dispatch;
            Box::pin(async move {
                let ports = stores();
                let scope = common::allow_all();
                let operation = ports
                    .insert_operation(
                        tx,
                        &scope,
                        NewOperation {
                            id,
                            kind: OperationKind::Registration,
                            dry_run: false,
                            plane: Plane::Platform,
                            tenant_id: None,
                            principal_id: Uuid::from_u128(1),
                            idempotency_key: id.to_string(),
                            idempotency_scope_hash: ScopeHash::from_stored(vec![1; 32]).unwrap(),
                            request_fingerprint: RequestFingerprint::from_stored(vec![2; 32])
                                .unwrap(),
                            now: NOW,
                        },
                    )
                    .await?;
                ports
                    .insert_items(
                        tx,
                        &scope,
                        &operation,
                        &[NewOperationItem {
                            item_no: 0,
                            gts_id: gts_id.to_owned(),
                            precondition: Precondition::MustNotExist,
                            compat_forced: false,
                            request_payload: schema(gts_id).to_string(),
                        }],
                    )
                    .await?;
                let wake = dispatch
                    .enqueue(tx, id)
                    .await
                    .map_err(|e| DbError::Other(e.into()))?;
                Ok(wake)
            })
        })
        .await
        .expect("seed operation and dispatch atomically");
    // Enqueue defers the sequencer wake to the post-commit signal; fire it now
    // that the seed transaction has committed.
    wake.fire();
}

#[tokio::test]
async fn enqueue_routes_independent_operations_to_different_partitions() {
    const SECOND: &str = gts_id!("cf.core.outbox.independent.v1~");

    let dsn = format!(
        "sqlite:file:tr-partitions-{}?mode=memory&cache=shared",
        Uuid::new_v4()
    );
    let db = common::provider_for_with_outbox(&dsn, 2).await;
    let first_id = Uuid::from_u128(1);
    let second_id = Uuid::from_u128(2);

    let (ports, reached, resume) = common::TestStores::pausing(common::PausePoint::OperationRead);
    let (registry, dispatch) = service(&db, ports);
    let handle = types_registry::infra::outbox::start(db.db(), &registry, &dispatch)
        .await
        .expect("start production pipeline");
    seed_partition_operation(
        &db,
        first_id,
        TARGET,
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
    )
    .await;
    let reached = std::sync::Mutex::new(reached);
    await_delivery("first admission enters its handler", || async {
        match reached.lock().unwrap().try_recv() {
            Ok(()) => Some(()),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
            Err(error) => panic!("admission pause dropped: {error}"),
        }
    })
    .await;

    seed_partition_operation(&db, second_id, SECOND, dispatch).await;
    let operation = await_delivery(
        "another partition completes while the first is paused",
        || async {
            let record = registry.operation(second_id).await.unwrap().unwrap();
            (record.status == OperationStatus::Completed).then_some(record)
        },
    )
    .await;
    assert_eq!(operation.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(
        registry
            .entity(&EntityKey::GtsId(SECOND.to_owned()), FieldSelection::full())
            .await
            .unwrap()
            .unwrap()
            .origin
            .map(|o| o.resource_version),
        Some(1)
    );
    assert_eq!(
        registry.operation(first_id).await.unwrap().unwrap().status,
        OperationStatus::Pending
    );

    resume.send(()).expect("release the first admission");
    let first = await_delivery("first partition resumes", || async {
        let record = registry.operation(first_id).await.unwrap().unwrap();
        (record.status == OperationStatus::Completed).then_some(record)
    })
    .await;
    assert_eq!(first.items[0].status, OperationItemStatus::Succeeded);
    handle.stop().await;
}

#[tokio::test]
async fn an_accepted_operation_is_admitted_by_the_outbox() {
    let db = test_db_with_outbox().await;
    let (registry, handle) = started(&db, stores()).await;

    let accepted = registry
        .submit(&registration("key", TARGET), NOW)
        .await
        .expect("accept");
    assert_eq!(accepted.status, OperationStatus::Pending);

    let operation = await_delivery("registration through the outbox", || async {
        let record = registry
            .operation(accepted.operation_id)
            .await
            .expect("read the operation")
            .expect("the operation exists");
        match record.status {
            OperationStatus::Completed => Some(record),
            OperationStatus::Pending | OperationStatus::Running => None,
        }
    })
    .await;

    assert_eq!(operation.items[0].status, OperationItemStatus::Succeeded);
    let entity = registry
        .entity(&EntityKey::GtsId(TARGET.to_owned()), FieldSelection::full())
        .await
        .expect("read")
        .expect("the entity the outbox admitted is readable");
    assert_eq!(entity.origin.map(|o| o.resource_version), Some(1));

    handle.stop().await;
}

#[tokio::test]
async fn stopping_the_pipeline_leaves_no_silent_enqueue() {
    let db = test_db_with_outbox().await;
    let (registry, dispatch) = service(&db, stores());
    let handle = types_registry::infra::outbox::start(db.db(), &registry, &dispatch)
        .await
        .expect("start");

    handle.stop().await;

    let refused = registry.submit(&registration("key", TARGET), NOW).await;
    assert!(
        refused.is_err(),
        "with the pipeline stopped the acceptance must refuse, not commit an \
         operation nothing will admit",
    );
    assert!(
        registry
            .entity(&EntityKey::GtsId(TARGET.to_owned()), FieldSelection::full())
            .await
            .expect("read")
            .is_none(),
        "and the refused acceptance must have rolled back",
    );
}

#[tokio::test]
async fn a_second_pipeline_refuses_to_bind_rather_than_starting_unreachable() {
    let db = test_db_with_outbox().await;
    let (registry, dispatch) = service(&db, stores());

    let first = types_registry::infra::outbox::start(db.db(), &registry, &dispatch)
        .await
        .expect("the first pipeline binds");

    let Err(refused) = types_registry::infra::outbox::start(db.db(), &registry, &dispatch).await
    else {
        panic!("the second pipeline must not bind");
    };
    assert!(
        matches!(
            refused,
            types_registry::domain::admission::OutboxError::AlreadyBound
        ),
        "a second bind is its own failure, not a generic outbox error: {refused}",
    );

    let accepted = registry
        .submit(&registration("after-refused-bind", TARGET), NOW)
        .await
        .expect("accept");
    let operation = await_delivery("the first pipeline still delivers", || async {
        let record = registry
            .operation(accepted.operation_id)
            .await
            .unwrap()
            .unwrap();
        (record.status == OperationStatus::Completed).then_some(record)
    })
    .await;
    assert_eq!(operation.status, OperationStatus::Completed);

    first.stop().await;
}

#[tokio::test]
async fn a_temporary_failure_is_redelivered_by_the_pipeline_until_it_clears() {
    const FAILURES: usize = 1;

    let db = test_db_with_outbox().await;
    let ports = common::TestStores::failing_running_transiently(FAILURES);
    let (registry, dispatch) = service(&db, Arc::clone(&ports) as Arc<dyn Stores>);
    let handle = types_registry::infra::outbox::start(db.db(), &registry, &dispatch)
        .await
        .expect("start the admission outbox");

    let accepted = registry
        .submit(&registration("retried-key", TARGET), NOW)
        .await
        .expect("accept");

    let operation = await_delivery("a redelivered admission completes", || async {
        let record = registry.operation(accepted.operation_id).await.unwrap()?;
        (record.status == OperationStatus::Completed).then_some(record)
    })
    .await;

    assert_eq!(
        ports.transient_failures_issued(),
        FAILURES,
        "the first delivery must have failed temporarily, or this test proves nothing",
    );
    assert_eq!(
        operation.items[0].status,
        OperationItemStatus::Succeeded,
        "the redelivery admits the candidate the failed delivery did not: {:?}",
        operation.items,
    );
    assert_eq!(
        operation.items.len(),
        1,
        "a redelivery resumes the operation rather than adding to it: {:?}",
        operation.items,
    );

    handle.stop().await;
}

#[tokio::test]
async fn a_rejected_message_lands_in_a_dead_letter_row_that_names_the_reason() {
    use toolkit_db::outbox::{DeadLetterFilter, DeadLetterScope, Record};

    let db = test_db_with_outbox().await;
    let (registry, dispatch) = service(&db, stores());
    let handle = types_registry::infra::outbox::start(db.db(), &registry, &dispatch)
        .await
        .expect("start the admission outbox");

    let provider: DBProvider<DbError> = DBProvider::new(db.db());
    let outbox = Arc::clone(handle.outbox());
    // Enqueue does not wake the sequencer; thread the flush handle out of the
    // transaction and flush it once the foreign record is durable.
    let wake = provider
        .transaction(move |tx| {
            let outbox = Arc::clone(&outbox);
            Box::pin(async move {
                let foreign = Record::to(types_registry::infra::outbox::QUEUE, 0)
                    .payload(
                        b"not-an-admission-message".to_vec(),
                        "some.other.gear.event",
                    )
                    .build()
                    .expect("build the foreign record");
                let wake = outbox.enqueue(tx, foreign).await.expect("enqueue");
                Ok::<_, DbError>(wake)
            })
        })
        .await
        .expect("commit the foreign message");
    wake.fire();

    let dead_letters = await_delivery("the foreign message is dead-lettered", || async {
        let conn = db.conn().expect("conn");
        let rows = handle
            .outbox()
            .dead_letter_list(
                &conn,
                &DeadLetterFilter::from_scope(DeadLetterScope::default()),
            )
            .await
            .expect("read the dead letters");
        (!rows.is_empty()).then_some(rows)
    })
    .await;

    assert_eq!(dead_letters.len(), 1, "one message, one row");
    let row = &dead_letters[0];
    assert_eq!(
        row.payload, b"not-an-admission-message",
        "the row keeps the bytes an operator has to look at",
    );
    assert_eq!(row.payload_type, "some.other.gear.event");
    let reason: Value = serde_json::from_str(
        row.last_error
            .as_deref()
            .expect("a rejected message records why"),
    )
    .expect("the reason is the handler's structured diagnostic");
    assert_eq!(
        reason["error_code"], "unexpected_payload_type",
        "the stored reason names the refusal, not a generic failure: {reason}",
    );
    assert_eq!(
        reason["operation_id"],
        Value::Null,
        "a foreign envelope names no operation",
    );

    handle.stop().await;
}

#[tokio::test]
async fn a_system_failure_whose_write_lands_is_acked_without_a_failed_delivery() {
    let db = test_db_with_outbox().await;
    let (registry, counted) =
        service_recording_deliveries(&db, common::TestStores::failing_running());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("terminalized", TARGET), NOW)
        .await
        .expect("accept");
    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), 0)
        .await;

    assert!(
        matches!(result, MessageResult::Ok),
        "a permanent failure whose operation was terminalized ends the message: {result:?}",
    );
    assert!(
        counted.outcomes().is_empty(),
        "a delivery that terminalized its operation succeeded as a transport, so the \
         failed-delivery series must not move: {:?}",
        counted.outcomes(),
    );

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(operation.status, OperationStatus::Completed);
    assert_eq!(operation.items[0].status, OperationItemStatus::Failed);
}

#[tokio::test]
async fn a_failed_terminalization_keeps_the_message_instead_of_acking_it() {
    let db = test_db_with_outbox().await;
    let (registry, counted) =
        service_recording_deliveries(&db, common::TestStores::failing_running_and_failure_write());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("unterminalized", TARGET), NOW)
        .await
        .expect("accept");
    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), 0)
        .await;

    assert!(
        matches!(result, MessageResult::Retry),
        "the operation is still non-terminal, so the message must stay deliverable: {result:?}",
    );
    assert_eq!(
        counted.outcomes(),
        vec![DeliveryOutcome::Retried],
        "the series an operator alerts on must say redelivered: a silent delivery \
         here would report work as finished while it is still queued",
    );

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(
        operation.status,
        OperationStatus::Pending,
        "the refused terminalization must have rolled back whole: {:?}",
        operation.items,
    );
    assert_eq!(operation.items[0].status, OperationItemStatus::Pending);
}

#[tokio::test]
async fn an_unwritable_system_failure_is_redelivered_past_the_budget_too() {
    let db = test_db_with_outbox().await;
    let (registry, counted) =
        service_recording_deliveries(&db, common::TestStores::failing_running_and_failure_write());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("unterminalizable", TARGET), NOW)
        .await
        .expect("accept");
    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), PAST_BUDGET)
        .await;

    assert!(
        matches!(result, MessageResult::Retry),
        "the attempt budget bounds admission, not terminalization: removing this \
         message would strand a non-terminal operation nothing else re-drives: {result:?}",
    );
    assert_eq!(counted.outcomes(), vec![DeliveryOutcome::Retried]);

    let operation = registry
        .operation(accepted.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(operation.status, OperationStatus::Pending);
}

#[tokio::test]
async fn a_retried_terminalization_failure_says_so_in_the_log() {
    let _serial = LOG_TESTS.lock().await;
    let captured = capture();
    captured.clear();
    let db = test_db_with_outbox().await;
    let registry =
        service_without_dispatch(&db, common::TestStores::failing_running_and_failure_write());
    let handler = AdmissionHandler::new(Arc::clone(&registry), MAX_ATTEMPTS);

    let accepted = registry
        .submit(&registration("logged", TARGET), NOW)
        .await
        .expect("accept");
    let result = handler
        .admit_payload(accepted.operation_id.to_string().as_bytes(), 0)
        .await;
    assert!(matches!(result, MessageResult::Retry), "got: {result:?}");

    let log = captured.lines_for(accepted.operation_id);
    assert!(
        log.contains("the message will be redelivered while the operation is non-terminal"),
        "the event must name the decision it made: {log}",
    );
    assert!(
        !log.contains("the message is acknowledged"),
        "and must not also claim the opposite: {log}",
    );
    assert!(
        log.contains("write=\"write_failed\""),
        "with the reason the terminalization did not land: {log}",
    );
}

/// A failed item's stored payload, read back through the domain parser.
fn stored_error(item: &types_registry::domain::registry_service::OperationItemRecord) -> Value {
    let stored = item
        .error
        .clone()
        .expect("a failed item carries a stored error payload")
        .expect("the stored payload is readable");
    serde_json::to_value(stored).expect("serialize")
}
