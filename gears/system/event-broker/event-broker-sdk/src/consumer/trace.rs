//! Continue the producer's trace while a consumer handles delivered events.
//!
//! An event carries the W3C `traceparent` it was produced under (captured by
//! the producer at publish/enqueue). Handling it in a span parented on that
//! value puts the consumer's work in the same trace as the request that
//! caused the event. A batch can mix events from different traces, so a batch
//! span is linked to each distinct trace instead of claiming one parent.

use std::collections::BTreeSet;

use tracing::Span;

use super::types::{EventBatch, RawEvent};

/// Span for handling one delivered event, parented on the trace the event was
/// produced under. Without a (valid) `traceparent` it is an ordinary span under
/// whatever is current.
pub(crate) fn event_span(event: &RawEvent) -> Span {
    let span = tracing::info_span!(
        "event_broker.consume",
        event.id = %event.id,
        event.type_id = %event.type_id,
        event.topic = %event.topic,
        event.partition = event.partition,
        event.offset = %event.offset,
        trace_id = tracing::field::Empty,
        parent.trace_id = tracing::field::Empty,
    );
    if let Some(traceparent) = event.trace_parent.as_deref() {
        toolkit_trace_context::set_parent_from_traceparent(&span, traceparent);
    }
    span
}

/// Span for handling one topic-partition batch, linked to every distinct trace
/// its events were produced under (a batch has no single causing request).
pub(crate) fn batch_span(batch: &EventBatch<'_>) -> Span {
    let first = batch.next_event();
    let span = tracing::info_span!(
        "event_broker.consume_batch",
        event.topic = first.map(|event| event.topic.to_string()),
        event.partition = first.map(|event| event.partition),
        batch.len = batch.len(),
    );
    let traces: BTreeSet<&str> = batch
        .iter()
        .filter_map(|event| event.trace_parent.as_deref())
        .collect();
    for traceparent in traces {
        toolkit_trace_context::link_traceparent(&span, traceparent);
    }
    span
}
