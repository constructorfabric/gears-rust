mod envelope;
mod record;

#[cfg(feature = "outbox")]
mod outbox;

#[cfg(test)]
mod envelope_tests;
#[cfg(feature = "outbox")]
#[cfg(test)]
mod outbox_tests;
#[cfg(test)]
mod record_tests;

pub use envelope::{DeadLetterEnvelope, DeadLetterSourceCoordinates};
#[cfg(feature = "outbox")]
pub use outbox::{ConsumerDlqOutbox, ConsumerDlqOutboxBuilder};
pub use record::{DeadLetterRecord, DeadLetterRecordBuilder, DeadLetterSink};
