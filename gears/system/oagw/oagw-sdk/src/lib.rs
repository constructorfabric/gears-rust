pub mod api;
pub mod body;
pub mod codec;
pub mod error;
pub mod field;
pub mod gts;
pub mod multipart;
pub mod quota;
pub mod reason;
pub mod sse;
pub mod ws;

pub mod models;

pub use models::{
    AuthConfig, BudgetConfig, BudgetMode, BurstConfig, CorsConfig, CorsHttpMethod,
    CreateRouteRequest, CreateRouteRequestBuilder, CreateUpstreamRequest,
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
pub use error::{ServiceGatewayError, StreamingError};
pub use gts::{
    APIKEY_AUTH_PLUGIN_ID, AUTH_PLUGIN_SCHEMA, GUARD_PLUGIN_SCHEMA, HTTP_PROTOCOL_ID,
    PROTOCOL_SCHEMA, PROXY_SCHEMA, ROUTE_SCHEMA, TRANSFORM_PLUGIN_SCHEMA, UPSTREAM_SCHEMA,
};
pub use multipart::{MultipartBody, MultipartError, Part};
pub use sse::{FromServerEvent, ServerEvent, ServerEventsResponse, ServerEventsStream};
#[cfg(feature = "axum")]
pub use ws::axum_adapter;
pub use ws::{
    FromWebSocketMessage, WebSocketCloseFrame, WebSocketMessage, WebSocketReceiver,
    WebSocketSender, WebSocketSink, WebSocketStream, WebSocketStreamReceiver,
};
