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
    /// Upload stall budget: when the request body makes no progress for
    /// this long after the response head arrived (e.g. the upstream holds
    /// the flow-control window at zero), the connection is torn down.
    /// `None` disables the watchdog.
    pub write_timeout: Option<Duration>,
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
    /// Idle connections (and unused HTTP/2 connections) older than this
    /// are closed.
    pub idle_timeout: Duration,
    /// How often the background reaper runs.
    pub reap_interval: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_idle_per_host: 128,
            idle_timeout: Duration::from_secs(90),
            reap_interval: Duration::from_secs(30),
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
    pub max_buf_size: usize,
}

impl Default for H1Limits {
    fn default() -> Self {
        Self {
            max_headers: 128,
            max_buf_size: 256 * 1024,
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
    /// Maximum frame size we accept.
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
            max_frame_size: 16 * 1024,
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
