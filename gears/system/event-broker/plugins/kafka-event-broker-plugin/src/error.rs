//! `KafkaError` -> `BackendError` (`docs/DESIGN.md` §7). A message carries the
//! operation and librdkafka's error code name only: librdkafka's own text may
//! quote broker addresses and topic names.

use std::fmt;

use event_broker_sdk::backend::BackendError;
use rdkafka::error::{KafkaError, RDKafkaErrorCode};

/// The `Backend` operation a failure happened in: it names the failure, and
/// decides what one the cluster does not recover from on its own maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Creating a topic's Kafka topic and log record.
    Provision,
    /// Writing events under a fence.
    Append,
    /// Reading events from a partition.
    Read,
    /// Turning a requested position into a sequence.
    Resolve,
    /// Describing what a partition stores.
    Query,
}

impl Operation {
    /// The operation's name, as error messages carry it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Provision => "provision",
            Self::Append => "append",
            Self::Read => "read",
            Self::Resolve => "resolve",
            Self::Query => "query",
        }
    }

    /// `AppendFailed` on the append path, `ReadFailed` on a read path;
    /// `provision` has no variant of its own.
    pub fn failed(self, reason: String) -> BackendError {
        match self {
            Self::Append => BackendError::append_failed(reason),
            Self::Read | Self::Resolve | Self::Query => BackendError::read_failed(reason),
            Self::Provision => BackendError::Internal(reason),
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Codes the cluster recovers from on its own, or that mean it cannot be
/// reached: the same call may succeed later.
pub fn is_transient(code: RDKafkaErrorCode) -> bool {
    use RDKafkaErrorCode as C;
    matches!(
        code,
        C::BrokerTransportFailure
            | C::Resolve
            | C::MessageTimedOut
            | C::AllBrokersDown
            | C::OperationTimedOut
            | C::QueueFull
            | C::ISRInsufficient
            | C::WaitingForCoordinator
            | C::TimedOutQueue
            | C::Retry
            | C::Fenced
            | C::PurgeQueue
            | C::PurgeInflight
            | C::DestroyBroker
            | C::BrokerDestroy
            | C::UnknownTopicOrPartition
            | C::UnknownTopic
            | C::UnknownPartition
            | C::LeaderNotAvailable
            | C::NotLeaderForPartition
            | C::RequestTimedOut
            | C::BrokerNotAvailable
            | C::ReplicaNotAvailable
            | C::NetworkException
            | C::CoordinatorLoadInProgress
            | C::CoordinatorNotAvailable
            | C::NotCoordinator
            | C::NotEnoughReplicas
            | C::NotEnoughReplicasAfterAppend
            | C::ConcurrentTransactions
            | C::ProducerFenced
            | C::InvalidProducerEpoch
            | C::TransactionCoordinatorFenced
            | C::KafkaStorageError
            | C::FencedLeaderEpoch
            | C::UnknownLeaderEpoch
            | C::OffsetNotAvailable
            | C::PreferredLeaderNotAvailable
            | C::ThrottlingQuotaExceeded
            | C::Authentication
            | C::SaslAuthenticationFailed
    )
}

/// Whether `init_transactions` failed because the transaction coordinator is
/// not ready yet: worth another attempt after a pause.
pub fn is_coordinator_busy(err: &KafkaError) -> bool {
    matches!(
        code_of(err),
        Some(
            RDKafkaErrorCode::ConcurrentTransactions
                | RDKafkaErrorCode::CoordinatorLoadInProgress
                | RDKafkaErrorCode::CoordinatorNotAvailable
                | RDKafkaErrorCode::NotCoordinator
                | RDKafkaErrorCode::WaitingForCoordinator
                | RDKafkaErrorCode::OperationTimedOut
                | RDKafkaErrorCode::RequestTimedOut
                | RDKafkaErrorCode::TimedOutQueue
                | RDKafkaErrorCode::BrokerTransportFailure
                | RDKafkaErrorCode::AllBrokersDown
        )
    )
}

/// The error's librdkafka code; `rdkafka` leaves an admin operation's out.
pub fn code_of(err: &KafkaError) -> Option<RDKafkaErrorCode> {
    match err {
        KafkaError::AdminOp(code) => Some(*code),
        other => other.rdkafka_error_code(),
    }
}

/// Whether the producer that hit `err` was fenced by a newer instance of its
/// transactional id. A fenced producer fails what it has queued with a purge
/// code, so those count too.
pub fn is_fenced(err: &KafkaError) -> bool {
    matches!(
        code_of(err),
        Some(
            RDKafkaErrorCode::Fenced
                | RDKafkaErrorCode::ProducerFenced
                | RDKafkaErrorCode::InvalidProducerEpoch
                | RDKafkaErrorCode::PurgeQueue
                | RDKafkaErrorCode::PurgeInflight
        )
    )
}

/// The failure's code, by name only.
fn code_name(err: &KafkaError) -> String {
    match code_of(err) {
        Some(code) => format!("{code:?}"),
        None => match err {
            KafkaError::Canceled => "client dropped".to_owned(),
            KafkaError::ClientConfig(..) => "client configuration rejected".to_owned(),
            KafkaError::ClientCreation(_) => "client creation failed".to_owned(),
            KafkaError::Nul(_) => "name with a NUL byte".to_owned(),
            _ => "client error".to_owned(),
        },
    }
}

/// Maps a failure of `operation`: transient ones and lost connectivity to
/// `Unavailable`, others per [`Operation`], the client's own to `Internal`.
pub fn map(operation: Operation, err: &KafkaError) -> BackendError {
    let reason = format!("{operation}: kafka error ({})", code_name(err));
    match code_of(err) {
        Some(code) if is_transient(code) => BackendError::unavailable(reason),
        Some(_) => operation.failed(reason),
        None => BackendError::Internal(reason),
    }
}

/// Maps a bare error code, as admin results carry them.
pub fn map_code(operation: Operation, code: RDKafkaErrorCode) -> BackendError {
    let reason = format!("{operation}: kafka error ({code:?})");
    if is_transient(code) {
        BackendError::unavailable(reason)
    } else {
        operation.failed(reason)
    }
}

/// A step that did not finish within the request timeout.
pub fn timed_out(operation: Operation, step: &str) -> BackendError {
    BackendError::unavailable(format!("{operation}: {step} timed out"))
}

/// Every call after `stop`.
pub fn stopped(operation: Operation) -> BackendError {
    BackendError::unavailable(format!("{operation}: the backend is stopped"))
}
