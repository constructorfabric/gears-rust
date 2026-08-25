//! Transport-specific overrides for the wire projection of a `CanonicalError`
//! occurrence. Overrides never change the error's canonical category (GTS
//! type, title, context schema) — only the named transport's wire mapping
//! reads them. See `docs/arch/errors/DESIGN.md` for the caller contract.

// ---------------------------------------------------------------------------
// TransportOverride (public)
// ---------------------------------------------------------------------------

/// A transport-specific override applied to the default canonical → wire
/// mapping for a single error occurrence.
///
/// Construct via a transport namespace type (e.g. [`Http::status_code`]) and
/// attach with `.with_override(...)` on any error builder. `#[non_exhaustive]`
/// so future transports (gRPC, SSE) can add their own variant without a
/// breaking change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TransportOverride {
    /// Override the HTTP status code used by the `Problem` (RFC 9457)
    /// projection. See [`Http::status_code`].
    Http(HttpOverride),
}

/// Payload for [`TransportOverride::Http`]. Constructed via
/// [`Http::status_code`], never directly.
#[derive(Debug, Clone)]
pub struct HttpOverride {
    pub(crate) status_code: u16,
}

/// Namespace for constructing HTTP transport overrides.
pub struct Http;

impl Http {
    /// Override the HTTP status code for a single `CanonicalError`
    /// occurrence, without changing its canonical category.
    ///
    /// A `const fn` — it only constructs plain data, so callers can bind a
    /// reusable named constant instead of repeating a literal at each call
    /// site, e.g. `pub const GONE: TransportOverride = Http::status_code(410);`.
    #[must_use]
    pub const fn status_code(code: u16) -> TransportOverride {
        TransportOverride::Http(HttpOverride { status_code: code })
    }
}

// ---------------------------------------------------------------------------
// TransportOverrides (private accumulator)
// ---------------------------------------------------------------------------

/// Per-error accumulator of transport overrides. One field per transport —
/// setting one transport's override never touches another's. Never exported;
/// the only public surface is [`TransportOverride`] and [`Http`].
#[derive(Debug, Clone, Default)]
pub(crate) struct TransportOverrides {
    pub(crate) http_status: Option<u16>,
}

impl TransportOverrides {
    pub(crate) fn apply(&mut self, ov: &TransportOverride) {
        match ov {
            TransportOverride::Http(HttpOverride { status_code }) => {
                self.http_status = Some(*status_code);
            }
        }
    }
}
