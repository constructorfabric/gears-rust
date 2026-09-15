//! `ClientHub` adapter for the authoritative database-backed registry.
//!
//! This deliberately does not fall back to the legacy in-memory service: a
//! successful call through [`TypesRegistryEntities`] is a persistence claim.

use std::sync::Arc;
use std::time::Duration;

use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use types_registry_sdk::{
    CandidateError, CandidateStatus, EntityKind, EntitySnapshot, LifecycleStatus, OperationStatus,
    RegisterEntities, RegistrationItemResult, RegistrationOperation, TypesRegistryEntities,
};

use super::admission::{Candidate, SubmitRequest};
use super::enums::{
    EntityKind as DomainEntityKind, LifecycleStatus as DomainLifecycleStatus,
    OperationItemStatus as DomainCandidateStatus, OperationKind,
    OperationStatus as DomainOperationStatus,
};
use super::registry_service::{EntityKey, EntityRecord, OperationRecord, RegistryService};

const INITIAL_POLL_INTERVAL_MS: u64 = 25;
const MAX_POLL_INTERVAL_MS: u64 = 1_000;

/// Persistent implementation of the startup-reconciliation SDK subset.
#[domain_model]
pub struct TypesRegistryEntitiesClient {
    registry: Arc<RegistryService>,
}

impl TypesRegistryEntitiesClient {
    #[must_use]
    pub fn new(registry: Arc<RegistryService>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl TypesRegistryEntities for TypesRegistryEntitiesClient {
    async fn get_entity(&self, gts_id: &str) -> Result<Option<EntitySnapshot>, CanonicalError> {
        self.registry
            .entity(&EntityKey::GtsId(gts_id.to_owned()))
            .await
            .map(|record| record.map(map_entity))
            .map_err(CanonicalError::from)
    }

    async fn compare_and_swap_owning_gear(
        &self,
        gts_id: &str,
        expected_resource_version: i64,
        expected_owning_gear: Option<String>,
        owning_gear: String,
    ) -> Result<bool, CanonicalError> {
        validate_owning_gear(&owning_gear)?;
        self.registry
            .compare_and_swap_owning_gear(
                gts_id,
                expected_resource_version,
                expected_owning_gear.as_deref(),
                &owning_gear,
                time::OffsetDateTime::now_utc(),
            )
            .await
            .map_err(CanonicalError::from)
    }

    async fn register_and_await(
        &self,
        idempotency_key: String,
        request: RegisterEntities,
        deadline: Duration,
    ) -> Result<RegistrationOperation, CanonicalError> {
        validate_owning_gear(&request.owning_gear)?;
        let owning_gear = request.owning_gear.clone();
        let deadline_at = tokio::time::Instant::now()
            .checked_add(deadline)
            .ok_or_else(|| service_unavailable("registration deadline is out of range"))?;
        let submit = SubmitRequest {
            idempotency_key,
            kind: OperationKind::Registration,
            dry_run: request.dry_run,
            candidates: request
                .items
                .into_iter()
                .map(|item| Candidate {
                    gts_id: item.gts_id,
                    content: Some(item.content),
                    expected_resource_version: item.expected_resource_version,
                    force: item.force,
                })
                .collect(),
        };

        let accepted = tokio::time::timeout_at(
            deadline_at,
            self.registry
                .submit_for_gear(&submit, &owning_gear, time::OffsetDateTime::now_utc()),
        )
        .await
        .map_err(|_| service_unavailable("persistent registration submission timed out"))?
        .map_err(CanonicalError::from)?;

        let mut poll_attempt = 0_u32;
        loop {
            let operation = tokio::time::timeout_at(
                deadline_at,
                self.registry.operation(accepted.operation_id),
            )
            .await
            .map_err(|_| service_unavailable("persistent registration polling timed out"))?
            .map_err(CanonicalError::from)?
            .ok_or_else(|| {
                CanonicalError::internal("accepted registration operation was not found").create()
            })?;

            if operation.status == DomainOperationStatus::Completed {
                return Ok(map_operation(operation));
            }

            let delay = polling_delay(operation.operation_id, poll_attempt);
            poll_attempt = poll_attempt.saturating_add(1);
            let wake_at = (tokio::time::Instant::now() + delay).min(deadline_at);
            tokio::time::sleep_until(wake_at).await;
            if tokio::time::Instant::now() >= deadline_at {
                return Err(service_unavailable(
                    "persistent registration did not complete before its deadline",
                ));
            }
        }
    }
}

/// Bounded exponential backoff with stable per-operation jitter. Stable jitter
/// prevents replicas polling the same database row in lockstep without needing
/// process-global randomness on this startup path.
fn polling_delay(operation_id: uuid::Uuid, attempt: u32) -> Duration {
    let shift = attempt.min(16);
    let exponential_ms = INITIAL_POLL_INTERVAL_MS
        .saturating_mul(1_u64 << shift)
        .min(MAX_POLL_INTERVAL_MS);
    let jitter_window = exponential_ms >> 2;
    let mut high_bytes = [0_u8; 8];
    high_bytes.copy_from_slice(&operation_id.as_bytes()[..8]);
    let mut low_bytes = [0_u8; 8];
    low_bytes.copy_from_slice(&operation_id.as_bytes()[8..]);
    let operation_seed = u64::from_be_bytes(high_bytes) ^ u64::from_be_bytes(low_bytes);
    let mixed = operation_seed ^ u64::from(attempt).wrapping_mul(0x9e37_79b9_7f4a_7c15_u64);
    let jitter = mixed % (jitter_window + 1);
    Duration::from_millis(exponential_ms - jitter)
}

fn validate_owning_gear(owning_gear: &str) -> Result<(), CanonicalError> {
    let valid = !owning_gear.is_empty()
        && owning_gear.len() <= 128
        && owning_gear
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && owning_gear
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && owning_gear
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric);
    if valid {
        Ok(())
    } else {
        Err(CanonicalError::internal("trusted owning_gear attribution is malformed").create())
    }
}

fn service_unavailable(detail: &str) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(detail)
        .create()
}

fn map_entity(record: EntityRecord) -> EntitySnapshot {
    EntitySnapshot {
        gts_id: record.gts_id,
        gts_uuid: record.gts_uuid,
        kind: match record.kind {
            DomainEntityKind::TypeSchema => EntityKind::TypeSchema,
            DomainEntityKind::Instance => EntityKind::Instance,
        },
        lifecycle_status: match record.lifecycle_status {
            DomainLifecycleStatus::Active => LifecycleStatus::Active,
            DomainLifecycleStatus::Deleted => LifecycleStatus::Deleted,
        },
        resource_version: record.resource_version,
        owning_gear: record.owning_gear,
        content: record.content,
        resolved_schema: record.resolved_schema,
        effective_traits: record.effective_traits,
        effective_traits_schema: record.effective_traits_schema,
    }
}

fn map_operation(record: OperationRecord) -> RegistrationOperation {
    RegistrationOperation {
        operation_id: record.operation_id,
        status: match record.status {
            DomainOperationStatus::Pending => OperationStatus::Pending,
            DomainOperationStatus::Running => OperationStatus::Running,
            DomainOperationStatus::Completed => OperationStatus::Completed,
        },
        items: record
            .items
            .into_iter()
            .map(|item| RegistrationItemResult {
                gts_id: item.gts_id,
                status: match item.status {
                    DomainCandidateStatus::Pending => CandidateStatus::Pending,
                    DomainCandidateStatus::Running => CandidateStatus::Running,
                    DomainCandidateStatus::Succeeded => CandidateStatus::Succeeded,
                    DomainCandidateStatus::Unchanged => CandidateStatus::Unchanged,
                    DomainCandidateStatus::Failed => CandidateStatus::Failed,
                },
                resource_version: item.resource_version,
                error: item.error.map(|payload| parse_candidate_error(&payload)),
            })
            .collect(),
    }
}

fn parse_candidate_error(payload: &str) -> CandidateError {
    let parsed = serde_json::from_str::<serde_json::Value>(payload).ok();
    CandidateError {
        reason: parsed
            .as_ref()
            .and_then(|value| value.get("reason"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        message: parsed
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(payload)
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_POLL_INTERVAL_MS, parse_candidate_error, polling_delay};

    #[test]
    fn polling_backoff_is_bounded_and_jittered_per_operation() {
        let first = uuid::Uuid::from_u128(1);
        let second = uuid::Uuid::from_u128(2);
        let first_delays: Vec<_> = (0..20)
            .map(|attempt| polling_delay(first, attempt))
            .collect();
        let second_delays: Vec<_> = (0..20)
            .map(|attempt| polling_delay(second, attempt))
            .collect();

        assert!(
            first_delays
                .iter()
                .all(|delay| { *delay <= std::time::Duration::from_millis(MAX_POLL_INTERVAL_MS) })
        );
        assert!(first_delays.iter().all(|delay| !delay.is_zero()));
        assert_ne!(first_delays, second_delays);
        assert!(first_delays[5] > first_delays[0]);
    }

    #[test]
    fn candidate_error_preserves_structured_reason() {
        let error = parse_candidate_error(
            r#"{"reason":"precondition_failed","message":"entity appeared"}"#,
        );
        assert_eq!(error.reason, "precondition_failed");
        assert_eq!(error.message, "entity appeared");
    }

    #[test]
    fn candidate_error_keeps_unstructured_payload() {
        let error = parse_candidate_error("legacy failure");
        assert_eq!(error.reason, "unknown");
        assert_eq!(error.message, "legacy failure");
    }
}
