//! Engine configuration with secure defaults.
//!
//! The protocol limits are part of the engine's security posture: they bound
//! what a malicious or broken upstream can make the gateway allocate or wait
//! for (HPACK bombs, CONTINUATION floods, reset floods, zero-window stalls,
//! oversized HTTP/1 heads). Override them only to tighten, or with a reason.

use std::sync::Arc;
use std::time::Duration;

use toolkit_http::TlsConfig;

/// Proxy engine configuration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProxyClientConfig {
    /// Budget for the TCP connect, and separately for the TLS handshake.
    pub connect_timeout: Duration,
    /// Budget for the response head after the request was handed to the
    /// connection, and for each response body frame after that.
    /// `None` disables both (callers then own all timeouts).
    pub read_timeout: Option<Duration>,
    /// Upload stall budget. A socket write or flush pending this long (the
    /// upstream stopped reading) severs the connection; an HTTP/2 stream
    /// whose flow-control window stays closed this long is reset. `None`
    /// disables both.
    pub write_timeout: Option<Duration>,
    /// Most bytes of response data merged into one body frame when it has
    /// already arrived in several small chunks or DATA frames. The engine
    /// never waits for data to fill a frame, so this saves the caller writes
    /// during a burst without delaying any byte. It bounds the copy made per
    /// merged frame; an upstream frame at least this large is handed out as
    /// is, uncopied. Zero disables merging.
    pub response_merge_limit: usize,
    /// Record the case of HTTP/1.1 response header names, so a hyper server
    /// with the same option writes them back as received (for case-sensitive
    /// legacy clients). Off by default: it copies every response header name
    /// per response (about 8% more client CPU on small responses). Requests
    /// need no option: one carrying hyper's case map (recorded by the
    /// caller's hyper server) always goes out with its case.
    pub preserve_header_case: bool,
    /// How long a negotiated protocol is remembered per host. Zero disables
    /// the cache (every new TLS connection offers `h2` and `http/1.1`).
    pub protocol_cache_ttl: Duration,
    /// Connection pool settings.
    pub pool: PoolConfig,
    /// HTTP/1.1 limits.
    pub h1: H1Limits,
    /// HTTP/2 limits.
    pub h2: H2Limits,
    /// TLS settings.
    pub tls: TlsOptions,
}

impl Default for ProxyClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            read_timeout: Some(Duration::from_secs(30)),
            write_timeout: Some(Duration::from_secs(30)),
            response_merge_limit: 32 * 1024,
            preserve_header_case: false,
            protocol_cache_ttl: Duration::from_secs(3600),
            pool: PoolConfig::default(),
            h1: H1Limits::default(),
            h2: H2Limits::default(),
            tls: TlsOptions::default(),
        }
    }
}

/// Connection pool settings.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PoolConfig {
    /// Idle HTTP/1.1 connections kept per host; extra ones are closed.
    pub max_idle_per_host: usize,
    /// Idle HTTP/1.1 connections kept across all hosts; beyond it the
    /// oldest idle connection (of any host) is closed. Bounds the file
    /// descriptors a gateway talking to many upstreams keeps open.
    pub max_idle_total: usize,
    /// Idle connections (and unused HTTP/2 connections) older than this
    /// are closed.
    pub idle_timeout: Duration,
    /// How often the background reaper runs.
    pub reap_interval: Duration,
    /// Exchanges an HTTP/2 connection carries before another connection to
    /// the same host is opened (spreads TLS and framing work over cores).
    pub h2_streams_per_connection: usize,
    /// Upper bound on HTTP/2 connections per host; beyond it, exchanges share
    /// the least loaded connection.
    pub h2_max_connections_per_host: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_idle_per_host: 128,
            max_idle_total: 1024,
            idle_timeout: Duration::from_secs(90),
            reap_interval: Duration::from_secs(30),
            h2_streams_per_connection: 8,
            h2_max_connections_per_host: 32,
        }
    }
}

/// HTTP/1.1 limits for upstream responses.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct H1Limits {
    /// Maximum number of response header fields.
    pub max_headers: usize,
    /// Maximum read buffer, which also caps the size of a response head
    /// (checked between reads, so a head delivered in one read may exceed it
    /// by at most one buffer growth step). hyper enforces a floor of 8 KiB.
    ///
    /// The task that reads a response body also reads the socket, so a body
    /// larger than the buffer streams through it without pauses; a larger
    /// buffer mostly costs memory per busy connection.
    pub max_buf_size: usize,
}

impl Default for H1Limits {
    fn default() -> Self {
        Self {
            max_headers: 128,
            max_buf_size: 64 * 1024,
        }
    }
}

/// HTTP/2 limits.
///
/// `max_header_list_size` is enforced on the *decoded* header list,
/// counting 32 octets of overhead per field (RFC 9113 §6.5.2), so it also
/// bounds HPACK amplification: thousands of 1-byte indexed references to a
/// tiny dynamic-table entry still cost 32+ octets each (CVE-2026-49975).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct H2Limits {
    /// `SETTINGS_MAX_HEADER_LIST_SIZE` advertised and enforced locally.
    pub max_header_list_size: u32,
    /// `SETTINGS_HEADER_TABLE_SIZE` (HPACK dynamic table) for decoding.
    pub header_table_size: u32,
    /// Maximum frame size we accept (`SETTINGS_MAX_FRAME_SIZE`); a whole
    /// frame is buffered before it is decoded. Larger frames mean fewer
    /// frames per body (the protocol default, 16 KiB, cuts large-response
    /// throughput by about a fifth), but a large frame also delays the other
    /// streams on its connection.
    pub max_frame_size: u32,
    /// Initial per-stream receive window.
    pub initial_stream_window_size: u32,
    /// Initial connection receive window.
    pub initial_connection_window_size: u32,
    /// Locally reset streams kept around to absorb late frames.
    pub max_concurrent_reset_streams: usize,
    /// Remotely reset streams pending accept before the connection errors.
    pub max_pending_accept_reset_streams: usize,
    /// Streams we reset because of peer errors before the connection is
    /// torn down with `ENHANCE_YOUR_CALM` (the "`MadeYouReset`" attack).
    pub max_local_error_reset_streams: usize,
    /// Maximum buffered outbound data per stream. h2 buffers up to this
    /// much regardless of the peer's flow-control window, and the upload
    /// stall watchdog can only see a stall once the buffer is full, so this
    /// also bounds what a zero-window peer can park per stream.
    pub max_send_buf_size: usize,
    /// Ping interval for liveness; `None` disables keep-alive pings.
    pub keep_alive_interval: Option<Duration>,
    /// Ping acknowledgement timeout before the connection is closed.
    pub keep_alive_timeout: Duration,
}

impl Default for H2Limits {
    fn default() -> Self {
        Self {
            max_header_list_size: 64 * 1024,
            header_table_size: 4096,
            max_frame_size: 64 * 1024,
            initial_stream_window_size: 1024 * 1024,
            initial_connection_window_size: 4 * 1024 * 1024,
            max_concurrent_reset_streams: 64,
            max_pending_accept_reset_streams: 32,
            max_local_error_reset_streams: 128,
            max_send_buf_size: 64 * 1024,
            keep_alive_interval: Some(Duration::from_secs(30)),
            keep_alive_timeout: Duration::from_secs(10),
        }
    }
}

/// Trust anchors for upstream certificates.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum RootStore {
    /// OS store, falling back to Mozilla's roots when the OS store is empty
    /// (minimal containers).
    #[default]
    NativeOrWebPki,
    /// OS store only; construction fails when it is empty.
    Native,
    /// Mozilla's roots (`webpki-roots`) only.
    WebPki,
    /// Exactly these anchors (e.g. a private CA).
    Custom(Arc<rustls::RootCertStore>),
}

/// Upstream certificate verification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum CertVerification {
    /// Full chain and host name verification (always used in production).
    #[default]
    Verify,
    /// Accept any certificate. **Tests only**; refused under `fips`.
    #[cfg(feature = "test-utils")]
    DangerAcceptAny,
}

/// TLS settings.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TlsOptions {
    /// Trust anchors.
    pub roots: RootStore,
    /// Protocol floor and optional client certificate, shared with
    /// `toolkit-http`.
    pub settings: TlsConfig,
    /// Certificate verification mode.
    pub verification: CertVerification,
}
