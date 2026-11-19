mod detect;
mod event;
mod parse;
#[cfg(feature = "axum")]
mod response;
mod stream;

pub use detect::is_server_events_response;
pub use event::ServerEvent;
pub use parse::DEFAULT_MAX_EVENT_BYTES;
pub(crate) use parse::parse_server_events_stream;
#[cfg(feature = "axum")]
pub(crate) use response::server_events_response;
pub use stream::{FromServerEvent, ServerEventsResponse, ServerEventsStream};
