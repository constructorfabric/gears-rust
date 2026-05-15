use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use modkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use usage_collector_sdk::models::{Subject, UsageKind, UsageRecord};
use usage_collector_sdk::{UsageCollectorClientV1, UsageCollectorError, UsageRecordError};
use uuid::Uuid;

use super::DeliveryHandler;

enum CollectorOutcome {
    Ok,
    Transient,
    Permanent,
    Unavailable,
    ResourceExhausted,
    Internal,
    Unknown,
    DataLoss,
    Cancelled,
    Aborted,
    InvalidArgument,
    PermissionDenied,
    NotFound,
}

struct MockCollector {
    outcome: CollectorOutcome,
}

impl MockCollector {
    fn ok() -> Arc<Self> {
        Arc::new(Self {
            outcome: CollectorOutcome::Ok,
        })
    }

    fn transient() -> Arc<Self> {
        Arc::new(Self {
            outcome: CollectorOutcome::Transient,
        })
    }

    fn permanent() -> Arc<Self> {
        Arc::new(Self {
            outcome: CollectorOutcome::Permanent,
        })
    }

    fn unavailable() -> Arc<Self> {
        Arc::new(Self {
            outcome: CollectorOutcome::Unavailable,
        })
    }

    fn resource_exhausted() -> Arc<Self> {
        Arc::new(Self {
            outcome: CollectorOutcome::ResourceExhausted,
        })
    }

    fn with_outcome(outcome: CollectorOutcome) -> Arc<Self> {
        Arc::new(Self { outcome })
    }
}

#[async_trait]
impl UsageCollectorClientV1 for MockCollector {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageCollectorError> {
        match self.outcome {
            CollectorOutcome::Ok => Ok(()),
            CollectorOutcome::Transient => {
                Err(UsageRecordError::deadline_exceeded("plugin timed out").create())
            }
            CollectorOutcome::Permanent => Err(UsageCollectorError::unauthenticated()
                .with_reason("permanent")
                .create()),
            CollectorOutcome::Unavailable => {
                Err(UsageCollectorError::service_unavailable().create())
            }
            CollectorOutcome::ResourceExhausted => Err(UsageRecordError::resource_exhausted(
                "rate limited by gateway",
            )
            .with_quota_violation("requests", "rate limit exceeded")
            .create()),
            CollectorOutcome::Internal => Err(UsageCollectorError::internal("boom").create()),
            CollectorOutcome::Unknown => Err(UsageRecordError::unknown("???").create()),
            CollectorOutcome::DataLoss => Err(UsageRecordError::data_loss("data loss detected")
                .with_resource("rec-1")
                .create()),
            CollectorOutcome::Cancelled => Err(UsageRecordError::cancelled().create()),
            CollectorOutcome::Aborted => Err(UsageRecordError::aborted("concurrency conflict")
                .with_reason("concurrency conflict on receiver")
                .create()),
            CollectorOutcome::InvalidArgument => Err(UsageRecordError::invalid_argument()
                .with_constraint("bad input")
                .create()),
            CollectorOutcome::PermissionDenied => Err(UsageRecordError::permission_denied()
                .with_reason("denied")
                .create()),
            CollectorOutcome::NotFound => Err(UsageRecordError::not_found("missing")
                .with_resource("rec-1")
                .create()),
        }
    }

    async fn get_module_config(
        &self,
        _module_name: &str,
    ) -> Result<usage_collector_sdk::ModuleConfig, UsageCollectorError> {
        Ok(usage_collector_sdk::ModuleConfig {
            allowed_metrics: vec![],
            max_metadata_bytes: 8192,
        })
    }
}

fn valid_usage_record() -> UsageRecord {
    UsageRecord {
        tenant_id: Uuid::new_v4(),
        module: "test-module".to_owned(),
        metric: "test.metric".to_owned(),
        kind: UsageKind::Gauge,
        value: 1.0,
        resource_id: Uuid::new_v4(),
        resource_type: "test.resource".to_owned(),
        subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
        idempotency_key: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        metadata: None,
    }
}

fn make_msg(payload_bytes: Vec<u8>) -> OutboxMessage {
    OutboxMessage {
        partition_id: 0,
        seq: 1,
        payload: payload_bytes,
        payload_type: "application/json".to_owned(),
        created_at: Utc::now(),
        attempts: 0,
    }
}

fn handler(collector: Arc<dyn UsageCollectorClientV1>) -> DeliveryHandler {
    DeliveryHandler::new(collector)
}

#[tokio::test]
async fn handle_invalid_json_payload_is_rejected() {
    let h = handler(MockCollector::ok());
    let msg = make_msg(b"not-json".to_vec());
    let result = h.handle(&msg).await;
    assert!(matches!(result, MessageResult::Reject(_)));
}

#[tokio::test]
async fn handle_collector_transient_error_returns_retry() {
    let h = handler(MockCollector::transient());
    let payload = serde_json::to_vec(&valid_usage_record()).unwrap();
    let msg = make_msg(payload);
    let result = h.handle(&msg).await;
    assert!(matches!(result, MessageResult::Retry));
}

#[tokio::test]
async fn handle_collector_permanent_error_returns_reject() {
    let h = handler(MockCollector::permanent());
    let payload = serde_json::to_vec(&valid_usage_record()).unwrap();
    let msg = make_msg(payload);
    let result = h.handle(&msg).await;
    assert!(matches!(result, MessageResult::Reject(_)));
}

#[tokio::test]
async fn handle_collector_unavailable_error_returns_retry() {
    let h = handler(MockCollector::unavailable());
    let payload = serde_json::to_vec(&valid_usage_record()).unwrap();
    let msg = make_msg(payload);
    let result = h.handle(&msg).await;
    assert!(matches!(result, MessageResult::Retry));
}

#[tokio::test]
async fn handle_collector_resource_exhausted_error_returns_retry() {
    let h = handler(MockCollector::resource_exhausted());
    let payload = serde_json::to_vec(&valid_usage_record()).unwrap();
    let msg = make_msg(payload);
    let result = h.handle(&msg).await;
    assert!(matches!(result, MessageResult::Retry));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedDisposition {
    Ok,
    Retry,
    Reject,
}

async fn assert_outcome(outcome: CollectorOutcome, expected: ExpectedDisposition) {
    let h = handler(MockCollector::with_outcome(outcome));
    let payload = serde_json::to_vec(&valid_usage_record()).unwrap();
    let msg = make_msg(payload);
    let got = h.handle(&msg).await;
    let got_disposition = match got {
        MessageResult::Ok => ExpectedDisposition::Ok,
        MessageResult::Retry => ExpectedDisposition::Retry,
        MessageResult::Reject(_) => ExpectedDisposition::Reject,
    };
    assert_eq!(got_disposition, expected, "delivery disposition mismatch");
}

// Canonically-retryable errors must retry: DeadlineExceeded, ResourceExhausted,
// ServiceUnavailable (covered above), plus Cancelled and Aborted.

#[tokio::test]
async fn handle_collector_cancelled_error_returns_retry() {
    assert_outcome(CollectorOutcome::Cancelled, ExpectedDisposition::Retry).await;
}

#[tokio::test]
async fn handle_collector_aborted_error_returns_retry() {
    assert_outcome(CollectorOutcome::Aborted, ExpectedDisposition::Retry).await;
}

// Internal / Unknown / DataLoss are canonically permanent and must dead-letter:
// serious defects, unrecognized error space, or unrecoverable corruption never
// improve with retry.

#[tokio::test]
async fn handle_collector_internal_error_is_rejected() {
    assert_outcome(CollectorOutcome::Internal, ExpectedDisposition::Reject).await;
}

#[tokio::test]
async fn handle_collector_unknown_error_is_rejected() {
    assert_outcome(CollectorOutcome::Unknown, ExpectedDisposition::Reject).await;
}

#[tokio::test]
async fn handle_collector_data_loss_error_is_rejected() {
    assert_outcome(CollectorOutcome::DataLoss, ExpectedDisposition::Reject).await;
}

// Caller-induced errors must dead-letter (retrying would loop forever).

#[tokio::test]
async fn handle_collector_invalid_argument_error_is_rejected() {
    assert_outcome(
        CollectorOutcome::InvalidArgument,
        ExpectedDisposition::Reject,
    )
    .await;
}

#[tokio::test]
async fn handle_collector_permission_denied_error_is_rejected() {
    assert_outcome(
        CollectorOutcome::PermissionDenied,
        ExpectedDisposition::Reject,
    )
    .await;
}

#[tokio::test]
async fn handle_collector_not_found_error_is_rejected() {
    assert_outcome(CollectorOutcome::NotFound, ExpectedDisposition::Reject).await;
}

#[tokio::test]
async fn handle_success_returns_ok() {
    let h = handler(MockCollector::ok());
    let payload = serde_json::to_vec(&valid_usage_record()).unwrap();
    let msg = make_msg(payload);
    let result = h.handle(&msg).await;
    assert!(matches!(result, MessageResult::Ok));
}

// ── Subject fields deserialization ────────────────────────────────────────────

struct CapturingCollector {
    captured: Arc<Mutex<Option<UsageRecord>>>,
}

impl CapturingCollector {
    fn new(captured: Arc<Mutex<Option<UsageRecord>>>) -> Arc<Self> {
        Arc::new(Self { captured })
    }
}

#[async_trait]
impl UsageCollectorClientV1 for CapturingCollector {
    async fn create_usage_record(&self, record: UsageRecord) -> Result<(), UsageCollectorError> {
        *self.captured.lock().unwrap() = Some(record);
        Ok(())
    }

    async fn get_module_config(
        &self,
        _module_name: &str,
    ) -> Result<usage_collector_sdk::ModuleConfig, UsageCollectorError> {
        Ok(usage_collector_sdk::ModuleConfig {
            allowed_metrics: vec![],
            max_metadata_bytes: 8192,
        })
    }
}

#[tokio::test]
async fn handle_delivery_preserves_subject_fields_through_deserialization() {
    let known_subject = Subject::with_type(Uuid::new_v4(), "real.subject");
    let record = UsageRecord {
        subject: Some(known_subject.clone()),
        ..valid_usage_record()
    };

    let captured: Arc<Mutex<Option<UsageRecord>>> = Arc::new(Mutex::new(None));
    let collector = CapturingCollector::new(Arc::clone(&captured));
    let h = handler(collector);
    let payload = serde_json::to_vec(&record).unwrap();
    let msg = make_msg(payload);
    let result = h.handle(&msg).await;
    assert!(matches!(result, MessageResult::Ok));

    let received = captured
        .lock()
        .unwrap()
        .take()
        .expect("collector must have received the record");
    assert_eq!(received.subject, Some(known_subject));
}
