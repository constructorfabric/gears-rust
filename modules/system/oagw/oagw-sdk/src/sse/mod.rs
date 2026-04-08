mod detect;
mod event;
mod parse;
#[cfg(feature = "axum")]
mod response;
mod stream;

pub(crate) use detect::is_server_events_response;
pub use event::ServerEvent;
pub(crate) use parse::parse_server_events_stream;
#[cfg(feature = "axum")]
pub(crate) use response::server_events_response;
pub use stream::{FromServerEvent, ServerEventsResponse, ServerEventsStream};
