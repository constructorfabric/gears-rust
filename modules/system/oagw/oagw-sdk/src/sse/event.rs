/// A parsed Server-Sent Event.
///
/// Follows the W3C EventSource specification fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerEvent {
    /// The `id` field. If present, sets the last event ID.
    pub id: Option<String>,
    /// The `event` field. Defaults to "message" if omitted by the server.
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

    /// Deserialize the `data` field as JSON into type `T`.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(&self.data)
    }
}
