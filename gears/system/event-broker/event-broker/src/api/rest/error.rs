//! `DomainError` → `CanonicalError`/`Problem` mapping (`domain/error.rs`'s
//! reserved path). Category choice matches `docs/DESIGN.md`'s Hard-Error
//! Catalog (`cpt-cf-evbk-interface-error-codes`). `SequenceViolation`/
//! `BatchTooLarge` keep their canonical `FailedPrecondition`/`InvalidArgument`
//! category (dispatch-relevant fields - `type`/`title` - stay put) but carry a
//! `TransportOverride` so the wire `status` matches `docs/openapi.yaml`'s
//! literal `412`/`413` instead of the category default of `400`
//! (`gears-rust#4465`/`canonical-error-transport-overrides`, which added the
//! escape hatch this needed).

use toolkit::api::canonical_prelude::*;
use toolkit_canonical_errors::{Http, ResourceErrorBuilder};

use event_broker_sdk::error::reasons;

use crate::domain::error::{DomainError, ErrorCode};

/// The default `permission_denied` detail the `resource_error` macro stamps.
/// Replicated here because the code-driven `forbidden`/`not_found` builders
/// select their resource type from [`ErrorCode::resource_type`] at runtime and
/// so call the resource-agnostic builder constructors directly rather than one
/// per-entity macro type.
const PERMISSION_DENIED_DETAIL: &str = "You do not have permission to perform this operation";
/// The default `failed_precondition` detail the `resource_error` macro stamps.
/// Replicated for the same reason as [`PERMISSION_DENIED_DETAIL`].
const FAILED_PRECONDITION_DETAIL: &str = "Operation precondition not met";

// These per-entity resource-error types name one fixed resource; they remain
// for the `From` arms below that always concern the same resource
// (`SchemaViolation`/`SequenceViolation` -> the topic, the
// `Unimplemented` capability -> the consumer group; an unknown producer is a
// code-driven `NotFound` naming the producer, below). The code-driven
// `not_found`/`forbidden`/`conflict` builders instead pick their resource from
// `ErrorCode::resource_type`, which is why no type is declared here for the
// subscription, event type or producer.
#[resource_error(gts_id!("cf.core.events.topic.v1~"))]
struct TopicResourceError;
#[resource_error(gts_id!("cf.core.events.consumer_group.v1~"))]
struct ConsumerGroupResourceError;
/// Fallback for codes not yet tied to one specific entity type (validation
/// on the request itself, rate limiting) - not every `DomainError` is about
/// one addressable resource instance. `pub(crate)` (not module-private):
/// `infra::dispatcher::forward` reuses `invalid_argument()` directly for the
/// dispatcher's oversized-proxied-body `413`, which isn't a `DomainError` at
/// all (see that call site).
#[resource_error(gts_id!("cf.core.events.request.v1~"))]
pub(crate) struct EventBrokerResourceError;

// The `not_found`/`forbidden`/`conflict` builders below select their resource
// type from `code.resource_type()` (a shared `event_broker_sdk` constant) at
// runtime, so they call the resource-agnostic `ResourceErrorBuilder`
// constructors rather than one per-entity `#[resource_error]` type. That is
// what closes the former open code string set: an unknown resource or a
// misspelled reason is a compile error on `ErrorCode`, not a silent
// mis-stamped body. The per-entity types above remain for the `From` arms that
// name one fixed resource.

fn not_found(code: ErrorCode, message: String, resource: String) -> CanonicalError {
    ResourceErrorBuilder::__not_found(code.resource_type(), message)
        .with_resource(resource)
        .create()
}

fn forbidden(code: ErrorCode, message: String) -> CanonicalError {
    // `permission_denied`'s context is `reason: <detail>`, so the descriptive
    // message rides in `with_reason`; its top-level detail keeps the macro's
    // generic default so the wire body is unchanged.
    ResourceErrorBuilder::__permission_denied(code.resource_type(), PERMISSION_DENIED_DETAIL)
        .with_reason(message)
        .create()
}

fn conflict(code: ErrorCode, message: String, resource: String) -> CanonicalError {
    let reason = code.precondition_reason();
    let builder = match code {
        // The unseeded partitions arrive as one `subject`-per-partition list in
        // `message`; each becomes its own precondition violation so a caller
        // sees every partition still needing a seek.
        ErrorCode::PositionsNotSet => {
            let description =
                format!("call POST /v1/subscriptions/{resource}:seek before re-opening the stream");
            let mut iter = message
                .strip_prefix("unseeded assigned partitions: ")
                .unwrap_or(&message)
                .split(", ")
                .map(String::from);
            let first = iter.next().unwrap_or_default();
            let mut builder = ResourceErrorBuilder::__failed_precondition(
                code.resource_type(),
                FAILED_PRECONDITION_DETAIL,
            )
            .with_precondition_violation(first, description.clone(), reason);
            for subject in iter {
                builder = builder.with_precondition_violation(subject, description.clone(), reason);
            }
            builder
        }
        ErrorCode::StreamingInProgress => ResourceErrorBuilder::__failed_precondition(
            code.resource_type(),
            FAILED_PRECONDITION_DETAIL,
        )
        .with_precondition_violation(
            resource,
            "one stream per subscription; DELETE is the only permitted concurrent call",
            reason,
        ),
        // Minimal body for the topology-version mismatch: the subscription
        // resource + reason only, no topology_version and no assignment - the
        // caller re-reads the subscription for the fresh state.
        ErrorCode::ConsumerGroupHasActiveMembers
        | ErrorCode::PartitionNotAssigned
        | ErrorCode::TopologyVersionMismatch => ResourceErrorBuilder::__failed_precondition(
            code.resource_type(),
            FAILED_PRECONDITION_DETAIL,
        )
        .with_precondition_violation(resource, message, reason),
        // Any other conflict code maps to `Aborted` with the code as its
        // reason, matching the former string-table fallback.
        _ => {
            return ResourceErrorBuilder::__aborted(code.resource_type(), message)
                .with_resource(resource)
                .with_reason(code.as_str())
                .create();
        }
    };
    match code.wire_override() {
        Some(status) => builder.with_override(Http::status_code(status)).create(),
        None => builder.create(),
    }
}

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            // Payload-schema validation is the one `Validation` that is `422
            // PayloadValidationFailed` (`DESIGN.md:586`), not `400`: the body
            // parsed fine but its `data` does not satisfy the event type's
            // schema. Category stays `InvalidArgument`; a `TransportOverride`
            // surfaces the literal `422` (same mechanism as `SequenceViolation`
            // -> 412). Every other `Validation` (envelope, subject-type, mixed
            // topics, unknown producer, ...) is a `400`.
            // The payload failed its type's `data_schema`. The `(payload)`/
            // `schema_validation` violation says what is wrong without
            // reproducing anything the caller sent (`no user input in error
            // bodies`), and the targeted stream is named in
            // `context.resource_name` - the common resource of every publish
            // error.
            DomainError::SchemaViolation { topic } => TopicResourceError::invalid_argument()
                .with_field_violation(
                    "(payload)",
                    "event data does not satisfy the event type's data schema",
                    "schema_validation",
                )
                .with_resource(topic)
                .with_override(Http::status_code(422))
                .create(),

            DomainError::Validation { code, message } => {
                // `invalid_argument()` takes no detail argument - `.with_format`
                // is what actually sets `Problem.detail` here. Also carries
                // `InvalidSubjectType`/`SubjectTypeNotAllowed`/
                // `InvalidSubjectTypePattern` (`eb-event-type-enforcement`).
                EventBrokerResourceError::invalid_argument()
                    .with_format(format!("{code}: {message}"))
                    .create()
            }

            DomainError::TextField {
                field,
                detail,
                reason,
            } => EventBrokerResourceError::invalid_argument()
                .with_field_violation(field, detail, reason)
                .create(),

            // Each violation renders itself, in the SDK, so the gear and the
            // reference mock reject the same request with the same body.
            DomainError::SeekOutOfRange { violations } => {
                let mut rendered = violations
                    .iter()
                    .map(event_broker_sdk::PositionViolation::as_field_violation);
                let Some(first) = rendered.next() else {
                    return EventBrokerResourceError::invalid_argument()
                        .with_format("seek rejected with no violation recorded")
                        .create();
                };
                let mut builder = EventBrokerResourceError::invalid_argument()
                    .with_field_violation(first.field, first.description, first.reason);
                for violation in rendered {
                    builder = builder.with_field_violation(
                        violation.field,
                        violation.description,
                        violation.reason,
                    );
                }
                builder.create()
            }

            DomainError::Forbidden {
                code,
                message,
                resource: _,
            } => forbidden(code, message),

            DomainError::NotFound {
                code,
                message,
                resource,
            } => not_found(code, message, resource),

            DomainError::Conflict {
                code,
                message,
                resource,
            } => conflict(code, message, resource),

            DomainError::SequenceViolation {
                topic,
                partition,
                last_sequence,
            } => {
                // `failed_precondition()` takes no detail argument and its
                // only context option is `.with_precondition_violation` (no
                // `.with_format`-equivalent for this context type) - the
                // descriptive text lives in the violation's `description`
                // only, matching `DESIGN.md`'s documented shape exactly
                // (`violations: [{type: "sequence_mismatch", subject:
                // "(producer)", description: "expected_previous=<n>"}]`).
                // `docs/openapi.yaml` documents this as a literal `412`; the
                // override only changes the wire `status`, not the category.
                TopicResourceError::failed_precondition()
                    .with_precondition_violation(
                        "(producer)",
                        format!(
                            "topic={topic} partition={partition} expected_previous={last_sequence}"
                        ),
                        reasons::SEQUENCE_MISMATCH,
                    )
                    .with_resource(topic)
                    .with_override(Http::status_code(412))
                    .create()
            }

            DomainError::BatchTooLarge { count, max } => {
                let message = format!("batch too large: {count} events (max {max})");
                // `docs/openapi.yaml` documents this as a literal `413`.
                EventBrokerResourceError::invalid_argument()
                    .with_format(message)
                    .with_override(Http::status_code(413))
                    .create()
            }

            DomainError::BatchPayloadTooLarge { bytes, max } => {
                let message = format!("batch payload too large: {bytes} bytes (max {max})");
                // `docs/openapi.yaml` documents this as a literal `413`.
                EventBrokerResourceError::invalid_argument()
                    .with_format(message)
                    .with_override(Http::status_code(413))
                    .create()
            }

            DomainError::RateLimited {
                code,
                message,
                retry_after_secs,
            } => EventBrokerResourceError::resource_exhausted(message.clone())
                .with_quota_violation(code.as_str(), message)
                .with_quota_violation_retry_after_seconds(u64::from(retry_after_secs))
                .create(),

            DomainError::StorageUnavailable { reason, .. } => CanonicalError::service_unavailable()
                .with_detail(reason)
                .create(),

            DomainError::Internal(detail) => CanonicalError::internal(detail).create(),
            DomainError::Unimplemented(detail) => {
                ConsumerGroupResourceError::unimplemented(detail).create()
            }
        }
    }
}

/// Round-trip guard for the six `#[resource_error(gts_id!(...))]` literals
/// above against `event-broker-sdk::gts`'s constants - the proc-macro cannot
/// reference the const directly (`eb-gts-type-registration`'s design.md
/// "cannot be centralized"), so a drift between the two is only caught here,
/// matching `resource-group-sdk`'s own `gts_resource_type_round_trip`
/// precedent for the identical limitation.
#[cfg(test)]
mod resource_type_round_trip_tests {
    use event_broker_sdk::error::resources;
    use event_broker_sdk::gts::{CONSUMER_GROUP_RESOURCE_TYPE, REQUEST_RESOURCE_TYPE, TopicV1};
    use toolkit::api::canonical_prelude::CanonicalError;
    use toolkit_canonical_errors::Problem;
    use toolkit_gts::GtsSchema;

    use super::{ConsumerGroupResourceError, EventBrokerResourceError, TopicResourceError};
    use crate::domain::error::ErrorCode;

    fn resource_type_of(err: CanonicalError) -> String {
        let json = serde_json::to_value(Problem::from(err)).expect("Problem serializes");
        json["context"]["resource_type"]
            .as_str()
            .expect("resource_type must be present")
            .to_owned()
    }

    #[test]
    fn topic_resource_error_matches_sdk_constant() {
        assert_eq!(
            resource_type_of(
                TopicResourceError::not_found("x")
                    .with_resource("x")
                    .create()
            ),
            TopicV1::TYPE_ID,
        );
    }

    #[test]
    fn consumer_group_resource_error_matches_sdk_constant() {
        assert_eq!(
            resource_type_of(
                ConsumerGroupResourceError::not_found("x")
                    .with_resource("x")
                    .create()
            ),
            CONSUMER_GROUP_RESOURCE_TYPE,
        );
    }

    #[test]
    fn event_broker_resource_error_matches_sdk_constant() {
        assert_eq!(
            resource_type_of(
                EventBrokerResourceError::not_found("x")
                    .with_resource("x")
                    .create()
            ),
            REQUEST_RESOURCE_TYPE,
        );
    }

    /// The codes whose resource the code-driven builders pick from
    /// `ErrorCode::resource_type` must name the same `types-registry` ids the
    /// per-entity types above do. One shared table means nothing else keeps
    /// the two in step.
    #[test]
    fn error_code_resource_type_matches_sdk_constants() {
        assert_eq!(ErrorCode::TopicNotFound.resource_type(), resources::TOPIC);
        assert_eq!(
            ErrorCode::EventTypeNotFound.resource_type(),
            resources::EVENT_TYPE
        );
        assert_eq!(
            ErrorCode::SubscriptionNotFound.resource_type(),
            resources::SUBSCRIPTION
        );
        assert_eq!(
            ErrorCode::ConsumerGroupNotFound.resource_type(),
            resources::CONSUMER_GROUP
        );
        assert_eq!(
            ErrorCode::ProducerNotFound.resource_type(),
            resources::PRODUCER
        );
        assert_eq!(
            ErrorCode::PartitionNotFound.resource_type(),
            resources::PARTITION
        );
        assert_eq!(
            ErrorCode::BadTypePattern.resource_type(),
            resources::REQUEST
        );
    }
}
