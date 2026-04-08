pub mod api;
pub mod body;
pub mod codec;
pub mod error;
pub mod multipart;
pub mod sse;
pub mod ws;

pub mod models;

pub use models::{
    AuthConfig, BurstConfig, CreateRouteRequest, CreateRouteRequestBuilder, CreateUpstreamRequest,
    CreateUpstreamRequestBuilder, Endpoint, GrpcMatch, HeadersConfig, HttpMatch, HttpMethod,
    ListQuery, MatchRules, PassthroughMode, PathSuffixMode, PluginBinding, PluginsConfig,
    RateLimitAlgorithm, RateLimitConfig, RateLimitScope, RateLimitStrategy, RequestHeaderRules,
    ResponseHeaderRules, Route, Scheme, Server, SharingMode, SustainedRate, UpdateRouteRequest,
    UpdateRouteRequestBuilder, UpdateUpstreamRequest, UpdateUpstreamRequestBuilder, Upstream,
    Window,
};

pub use api::ServiceGatewayClientV1;
pub use body::Body;
pub use codec::Json;
pub use error::StreamingError;
pub use multipart::{MultipartBody, MultipartError, Part};
pub use sse::{FromServerEvent, ServerEvent, ServerEventsResponse, ServerEventsStream};
#[cfg(feature = "axum")]
pub use ws::axum_adapter;
pub use ws::{
    FromWebSocketMessage, WebSocketCloseFrame, WebSocketMessage, WebSocketReceiver,
    WebSocketSender, WebSocketSink, WebSocketStream, WebSocketStreamReceiver,
};
