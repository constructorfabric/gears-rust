//! SSE wire conversion and event ordering enforcement.
//!
//! - `into_sse_event()`: converts domain `StreamEvent` to Axum SSE `Event`
//! - `From<ClientSseEvent>`: translates provider events to domain events
//! - `StreamPhase`: state machine enforcing the ordering grammar
//!   `stream_started ping* (delta | tool)* citations? (done | error)`

use axum::response::sse::Event;

use crate::domain::stream_events::{CitationsData, DeltaData, StreamEvent, ToolData};
use crate::infra::llm::ClientSseEvent;

pub(crate) use crate::domain::stream_events::StreamEventKind;

// ════════════════════════════════════════════════════════════════════════════
// SSE wire conversion
// ════════════════════════════════════════════════════════════════════════════

impl StreamEvent {
    /// Convert to an Axum SSE [`Event`] with the correct `event:` name
    /// and `data:` JSON payload.
    pub fn into_sse_event(self) -> Result<Event, axum::Error> {
        match self {
            StreamEvent::StreamStarted(d) => Event::default().event("stream_started").json_data(&d),
            StreamEvent::Ping => Ok(Event::default().event("ping").data("{}")),
            StreamEvent::Delta(d) => Event::default().event("delta").json_data(&d),
            StreamEvent::Tool(t) => Event::default().event("tool").json_data(&t),
            StreamEvent::Citations(c) => Event::default().event("citations").json_data(&c),
            StreamEvent::Done(d) => Event::default().event("done").json_data(&*d),
            StreamEvent::Error(e) => Event::default().event("error").json_data(&e),
        }
    }
}

impl modkit::api::api_dto::ResponseApiDto for StreamEvent {}

// ════════════════════════════════════════════════════════════════════════════
// Provider → domain conversion
// ════════════════════════════════════════════════════════════════════════════

impl From<ClientSseEvent> for StreamEvent {
    fn from(event: ClientSseEvent) -> Self {
        match event {
            ClientSseEvent::Delta { r#type, content } => {
                StreamEvent::Delta(DeltaData { r#type, content })
            }
            ClientSseEvent::Tool {
                phase,
                name,
                details,
            } => StreamEvent::Tool(ToolData {
                phase,
                name: name.to_owned(),
                details,
            }),
            ClientSseEvent::Citations { items } => StreamEvent::Citations(CitationsData { items }),
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// StreamEventKind — Display
// ════════════════════════════════════════════════════════════════════════════

impl std::fmt::Display for StreamEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StreamStarted => f.write_str("StreamStarted"),
            Self::Ping => f.write_str("Ping"),
            Self::Delta => f.write_str("Delta"),
            Self::Tool => f.write_str("Tool"),
            Self::Citations => f.write_str("Citations"),
            Self::Terminal => f.write_str("Terminal"),
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// StreamPhase — event ordering state machine
// ════════════════════════════════════════════════════════════════════════════

/// Enforces the SSE ordering grammar:
/// `stream_started ping* (delta | tool)* citations? (done | error)`.
///
/// Delta and tool events may interleave freely within the `Streaming` phase.
/// Only forward transitions are allowed. Out-of-order events produce an
/// [`OrderingViolation`] error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPhase {
    /// Before any events. Accepts only `stream_started` (or terminal for immediate errors).
    Idle,
    /// After `stream_started`. Same transitions as `Idle` except `stream_started` (exactly-once).
    Started,
    /// After one or more pings. Accepts ping, delta, tool, citations, terminal.
    Pinging,
    /// After first delta or tool. Accepts delta, tool, citations, terminal.
    Streaming,
    /// After citations. Accepts terminal only.
    Citations,
    /// Terminal event emitted. No further events accepted.
    Terminal,
}

/// An event that violates the ordering grammar.
#[derive(Debug)]
pub struct OrderingViolation {
    pub phase: StreamPhase,
    pub event: StreamEventKind,
}

impl std::fmt::Display for OrderingViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SSE ordering violation: {} event in {} phase",
            self.event, self.phase
        )
    }
}

impl std::fmt::Display for StreamPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => f.write_str("Idle"),
            Self::Started => f.write_str("Started"),
            Self::Pinging => f.write_str("Pinging"),
            Self::Streaming => f.write_str("Streaming"),
            Self::Citations => f.write_str("Citations"),
            Self::Terminal => f.write_str("Terminal"),
        }
    }
}

impl StreamPhase {
    /// Whether this phase represents a terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, StreamPhase::Terminal)
    }

    /// Attempt to advance the phase based on the incoming event kind.
    ///
    /// Returns the new phase on success, or an [`OrderingViolation`] if the
    /// event would break the grammar.
    pub fn try_advance(self, kind: StreamEventKind) -> Result<StreamPhase, OrderingViolation> {
        match (self, kind) {
            // Terminal events are accepted from any phase after stream_started
            // (plus Idle for immediate pre-stream errors)
            (
                StreamPhase::Idle
                | StreamPhase::Started
                | StreamPhase::Pinging
                | StreamPhase::Streaming
                | StreamPhase::Citations,
                StreamEventKind::Terminal,
            ) => Ok(StreamPhase::Terminal),

            // StreamStarted: only from Idle (exactly-once)
            (StreamPhase::Idle, StreamEventKind::StreamStarted) => Ok(StreamPhase::Started),

            // Ping: from Started or Pinging
            (StreamPhase::Started | StreamPhase::Pinging, StreamEventKind::Ping) => {
                Ok(StreamPhase::Pinging)
            }

            // Delta or Tool: from Started, Pinging, or Streaming
            (
                StreamPhase::Started | StreamPhase::Pinging | StreamPhase::Streaming,
                StreamEventKind::Delta | StreamEventKind::Tool,
            ) => Ok(StreamPhase::Streaming),

            // Citations: from Started, Pinging, or Streaming (at most once)
            (
                StreamPhase::Started | StreamPhase::Pinging | StreamPhase::Streaming,
                StreamEventKind::Citations,
            ) => Ok(StreamPhase::Citations),

            // Everything else is a violation
            _ => Err(OrderingViolation {
                phase: self,
                event: kind,
            }),
        }
    }
}
// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;
