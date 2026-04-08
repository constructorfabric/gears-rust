/// Codec adapter that provides automatic JSON serialization and deserialization
/// for both SSE and WebSocket streaming protocols.
///
/// `Json<T>` implements [`FromServerEvent`](crate::sse::FromServerEvent) and
/// [`FromWebSocketMessage`](crate::ws::FromWebSocketMessage), so any type that
/// derives `Serialize`/`Deserialize` can be used directly as the type parameter
/// of [`ServerEventsStream`](crate::sse::ServerEventsStream) or
/// [`WebSocketStream`](crate::ws::WebSocketStream) without writing manual
/// conversion logic.
///
/// This is the default "just parse it as JSON" path — covering the majority of
/// real-world streaming APIs. For non-JSON formats (e.g. OpenAI's `[DONE]`
/// sentinel, custom binary protocols), implement `FromServerEvent` or
/// `FromWebSocketMessage` directly on your own type instead.
///
/// # SSE usage
///
/// ```ignore
/// let ServerEventsResponse::Events(mut events) =
///     ServerEventsStream::from_response::<Json<MyPayload>>(resp)
/// else {
///     // handle non-SSE response
///     return;
/// };
/// while let Some(item) = events.next().await {
///     let payload: MyPayload = item?.into_inner();
/// }
/// ```
///
/// # WebSocket usage
///
/// ```ignore
/// // Sending
/// let msg = Json(ChatMessage { text: "hello".into() });
/// ws.send(&msg.to_ws_message()).await?;
///
/// // Receiving
/// let received = <Json<ChatMessage>>::from_ws_message(raw_msg)?;
/// println!("{}", received.text); // Deref gives access to inner T
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Json<T>(pub T);

impl<T> std::ops::Deref for Json<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for Json<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> Json<T> {
    /// Unwrap into the inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
}
