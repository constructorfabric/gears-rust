/// Marker inserted into a response's extensions by a reverse-proxy or other
/// passthrough layer to declare: "this response's status, headers, and body
/// came from a genuine external upstream (or an underlying data-plane that
/// already decided how to represent its own failure) and must not be
/// touched by `canonical_error_middleware`."
///
/// Its content was never typed as [`crate::CanonicalError`] and was never
/// meant to be - replacing it would destroy the caller's own error
/// identity (RFC 9457 `type`/`title` it may already carry, or a
/// gear-specific error-source header), and for a body that's still being
/// streamed from the network, force full buffering of what would otherwise
/// flow straight through.
///
/// Inserted by `toolkit-gateway::Forwarder` on every response it relays
/// from an upstream, and by `oagw`'s proxy data-plane on every response it
/// relays from the underlying data-plane layer (upstream or pingora
/// itself) - both are reverse-proxy passthroughs where the response was
/// never constructed via this platform's canonical error system.
#[derive(Debug, Clone, Copy)]
pub struct ForeignPassthrough;
