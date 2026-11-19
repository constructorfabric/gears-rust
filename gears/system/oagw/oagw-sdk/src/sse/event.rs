/// A parsed Server-Sent Event.
///
/// Follows the W3C EventSource specification fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerEvent {
    /// The `id` field. If present, sets the last event ID.
    pub id: Option<String>,
    /// The `event` field as sent; `None` if the server sent none.
    /// [`Self::event_type`] applies the `"message"` default.
    pub event: Option<String>,
    /// The `data` field. Multiple `data:` lines are joined with newlines.
    pub data: String,
    /// The `retry` field in milliseconds.
    pub retry: Option<u64>,
}

impl ServerEvent {
    /// Returns true if this event has no meaningful content.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty() && self.id.is_none() && self.event.is_none() && self.retry.is_none()
    }

    /// The event type: the `event` field, or `"message"` when the server sent
    /// none or an empty one (the W3C EventSource default).
    #[must_use]
    pub fn event_type(&self) -> &str {
        match self.event.as_deref() {
            Some(name) if !name.is_empty() => name,
            _ => "message",
        }
    }

    /// Deserialize the `data` field as JSON into type `T`.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(&self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_defaults_to_message() {
        let unnamed = ServerEvent::default();
        assert_eq!(unnamed.event_type(), "message");

        let empty = ServerEvent {
            event: Some(String::new()),
            ..ServerEvent::default()
        };
        assert_eq!(empty.event_type(), "message");

        let named = ServerEvent {
            event: Some("tick".into()),
            ..ServerEvent::default()
        };
        assert_eq!(named.event_type(), "tick");
    }
}
