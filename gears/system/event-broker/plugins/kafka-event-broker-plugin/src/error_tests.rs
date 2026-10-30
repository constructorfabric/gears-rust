use event_broker_sdk::backend::BackendError;
use rdkafka::error::{KafkaError, RDKafkaErrorCode};

use crate::error::{self, Operation};

fn shown(err: &BackendError) -> String {
    format!("{err:?}")
}

#[test]
fn transient_codes_are_unavailable() {
    let cases = [
        KafkaError::MessageProduction(RDKafkaErrorCode::AllBrokersDown),
        KafkaError::AdminOp(RDKafkaErrorCode::OperationTimedOut),
        KafkaError::MetadataFetch(RDKafkaErrorCode::NotEnoughReplicas),
    ];
    assert_eq!(
        cases.map(|err| shown(&error::map(Operation::Append, &err))),
        [
            r#"Unavailable { reason: "append: kafka error (AllBrokersDown)", detail: "" }"#,
            r#"Unavailable { reason: "append: kafka error (OperationTimedOut)", detail: "" }"#,
            r#"Unavailable { reason: "append: kafka error (NotEnoughReplicas)", detail: "" }"#,
        ]
    );
}

#[test]
fn other_codes_fail_per_operation() {
    let err = KafkaError::MessageProduction(RDKafkaErrorCode::MessageSizeTooLarge);
    assert_eq!(
        [Operation::Append, Operation::Read, Operation::Provision]
            .map(|operation| shown(&error::map(operation, &err))),
        [
            r#"AppendFailed { reason: "append: kafka error (MessageSizeTooLarge)", detail: "" }"#,
            r#"ReadFailed { reason: "read: kafka error (MessageSizeTooLarge)", detail: "" }"#,
            r#"Internal("provision: kafka error (MessageSizeTooLarge)")"#,
        ]
    );
}

#[test]
fn client_errors_are_internal_and_quote_nothing() {
    let err = KafkaError::ClientCreation("broker secret-host:9092 rejected".to_owned());
    assert_eq!(
        shown(&error::map(Operation::Read, &err)),
        r#"Internal("read: kafka error (client creation failed)")"#
    );
}

#[test]
fn a_purge_after_fencing_counts_as_fenced() {
    assert_eq!(
        [
            KafkaError::MessageProduction(RDKafkaErrorCode::PurgeQueue),
            KafkaError::MessageProduction(RDKafkaErrorCode::ProducerFenced),
            KafkaError::MessageProduction(RDKafkaErrorCode::MessageSizeTooLarge),
        ]
        .map(|err| error::is_fenced(&err)),
        [true, true, false]
    );
}
