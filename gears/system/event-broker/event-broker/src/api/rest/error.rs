//! `DomainError` → `CanonicalError`/`Problem` mapping (`domain/error.rs`'s
//! reserved path). Category choice matches `docs/DESIGN.md` "Error Response
//! Format" (`cpt-cf-evbk-interface-error-codes`). `SchemaViolation` (`422`),
//! `SequenceViolation` and `TopologyVersionMismatch` (`412`), `BatchTooLarge`
//! and `EventTooLarge` (`413`), and `PositionsNotSet`, `StreamingInProgress`,
//! `ConsumerGroupHasActiveMembers` and `PartitionNotAssigned` (`409`) keep their
//! canonical category (dispatch-relevant fields - `type`/`title` - stay put) but
//! carry a `TransportOverride` so the wire `status` matches `docs/openapi.yaml`'s
//! literal status instead of the category default
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
/// The field-violation reasons of the batch-shape rules (feature 0001 §5.2),
/// spelled as the docs spell the codes.
const EMPTY_BATCH: &str = "empty_batch";
const DUPLICATE_EVENT_ID: &str = "duplicate_event_id";
const MALFORMED_RUN: &str = "malformed_run";

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
            "one stream per subscription; a second stream or a SEEK is refused while it is open",
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

/// An error and every `source()` below it, joined as `outer: inner: ..`, for
/// a server-side log line (the fields' own formatter may print only the
/// outermost `Display`). A source whose text ends the text of the link above it
/// is skipped: sea-orm's and sqlx's database errors print their cause as
/// `"<prefix>: {0}"`.
struct SourceChain<'a>(&'a (dyn std::error::Error + 'static));

impl std::fmt::Display for SourceChain<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut printed = self.0.to_string();
        let mut above = printed.clone();
        let mut source = self.0.source();
        while let Some(err) = source {
            let text = err.to_string();
            if !above.ends_with(&text) {
                printed.push_str(": ");
                printed.push_str(&text);
            }
            above = text;
            source = err.source();
        }
        f.write_str(&printed)
    }
}

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            // The body parsed, but its `data` fails the event type's `data_schema`:
            // `docs/openapi.yaml` documents a literal `422`, which a `TransportOverride`
            // surfaces over the `InvalidArgument` category's `400`. The `(payload)`
            // violation reproduces nothing the caller sent (no user input in error
            // bodies); the topic rides in `context.resource_name`.
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
                // `invalid_argument()` takes no detail argument; `.with_format`
                // is what sets `Problem.detail`.
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
                index,
                producer_id,
                topic,
                last_sequence,
            } => {
                // A precondition violation is a fixed `{type, subject,
                // description}` shape, so the run's `index`, `producer`,
                // `topic` and committed `last_sequence` (feature 0001 §5.2)
                // ride in `description` as text, `last_sequence` last: the
                // SDK reads the resync point as the description's last number.
                // `docs/openapi.yaml` documents a literal `412`; the override
                // only changes the wire `status`, not the category.
                TopicResourceError::failed_precondition()
                    .with_precondition_violation(
                        "(producer)",
                        format!(
                            "index={index} producer={producer_id} topic={topic} \
                             last_sequence={last_sequence}"
                        ),
                        reasons::SEQUENCE_MISMATCH,
                    )
                    .with_resource(topic)
                    .with_override(Http::status_code(412))
                    .create()
            }

            // The batch-shape rules R1, R13 and R3 of feature 0001 §5.2.
            DomainError::EmptyBatch => EventBrokerResourceError::invalid_argument()
                .with_field_violation("events", "a batch carries at least one event", EMPTY_BATCH)
                .create(),
            DomainError::DuplicateEventId { index } => EventBrokerResourceError::invalid_argument()
                .with_field_violation(
                    format!("events[{index}].id"),
                    "repeats the id of an earlier event of the batch",
                    DUPLICATE_EVENT_ID,
                )
                .create(),
            DomainError::MalformedRun { index } => EventBrokerResourceError::invalid_argument()
                .with_field_violation(
                    format!("events[{index}].meta"),
                    "does not follow the same producer's event before it in the batch",
                    MALFORMED_RUN,
                )
                .create(),

            // The fields and the reason are the ones the SDK reads its `BatchTooLarge`
            // back by.
            DomainError::BatchTooLarge { count, max } => {
                let message = format!("batch too large: {count} events (max {max})");
                // `docs/openapi.yaml` documents this as a literal `413`.
                EventBrokerResourceError::invalid_argument()
                    .with_field_violation("batch.count", message, reasons::BATCH_TOO_LARGE)
                    .with_override(Http::status_code(413))
                    .create()
            }

            DomainError::BatchPayloadTooLarge { bytes, max } => {
                let message = format!("batch payload too large: {bytes} bytes (max {max})");
                // `docs/openapi.yaml` documents this as a literal `413`.
                EventBrokerResourceError::invalid_argument()
                    .with_field_violation("batch.bytes", message, reasons::BATCH_TOO_LARGE)
                    .with_override(Http::status_code(413))
                    .create()
            }

            // The description ends with the size and the limit, which the SDK reads
            // back; it carries nothing of the event.
            DomainError::EventTooLarge { index, bytes, max } => {
                // `docs/openapi.yaml` documents this as a literal `413`.
                EventBrokerResourceError::invalid_argument()
                    .with_field_violation(
                        format!("events[{index}]"),
                        format!("{bytes} bytes encoded, over the limit of {max}"),
                        reasons::EVENT_TOO_LARGE,
                    )
                    .with_override(Http::status_code(413))
                    .create()
            }

            DomainError::PartitionKeyTooLong { index, bytes, max } => {
                EventBrokerResourceError::invalid_argument()
                    .with_field_violation(
                        format!("events[{index}].partition_key"),
                        format!("{bytes} bytes, over the limit of {max}"),
                        reasons::PARTITION_KEY_TOO_LONG,
                    )
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

            // A 5xx body names no cause: the backend kind, a topic id or a
            // driver message is for the operator. It is logged here, which the
            // REST and the in-process `LocalBroker` paths both pass, and the
            // wire carries the category's generic detail.
            fault @ DomainError::StorageUnavailable { .. } => {
                tracing::warn!(
                    error = %SourceChain(&fault),
                    "answering 503 with the generic body"
                );
                CanonicalError::service_unavailable().create()
            }

            DomainError::Internal(detail) => {
                tracing::error!(error = %detail, "answering 500 with the generic body");
                CanonicalError::internal(detail).create()
            }
            DomainError::Unimplemented(detail) => {
                ConsumerGroupResourceError::unimplemented(detail).create()
            }
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;

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
