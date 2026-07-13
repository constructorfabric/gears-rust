//! GTS resource-type identifiers for OAGW resources.
//!
//! The string constants are the SDK-side mirror of the
//! `#[resource_error(...)]` literals in the impl crate
//! (`oagw/src/api/rest/error.rs`) and of the `*_SCHEMA` constants in
//! `oagw/src/domain/gts_helpers.rs`. Proc macros cannot resolve constants,
//! so the impl-side `#[resource_error]` attribute must still pass the
//! literal string. The `resource_type_strings_have_expected_shape` test in
//! this gear asserts the SDK constants stay well-formed; the round-trip
//! tests in [`crate::error`] pin each constant to the wire by constructing
//! a `CanonicalError` through it and asserting it lands in the `Problem`
//! body unchanged.
//!
//! Consumers that need to branch on the wire `resource_type` field of
//! `CanonicalError::NotFound` / `::AlreadyExists` should `match` directly
//! against the constants:
//!
//! ```ignore
//! match ctx.resource_type.as_deref() {
//!     Some(oagw_sdk::gts::UPSTREAM_SCHEMA) => /* upstream */,
//!     Some(oagw_sdk::gts::ROUTE_SCHEMA)    => /* route */,
//!     _ => /* other */,
//! }
//! ```

use toolkit_gts::gts_id;
/// Umbrella resource scope for gateway data-plane / proxy errors.
pub const PROXY_SCHEMA: &str = gts_id!("cf.core.oagw.proxy.v1~");

/// Resource scope for a specific upstream definition.
pub const UPSTREAM_SCHEMA: &str = gts_id!("cf.core.oagw.upstream.v1~");

/// Resource scope for a specific route definition.
pub const ROUTE_SCHEMA: &str = gts_id!("cf.core.oagw.route.v1~");

/// Resource scope for a specific authentication plugin instance.
pub const AUTH_PLUGIN_SCHEMA: &str = gts_id!("cf.core.oagw.auth_plugin.v1~");

/// Resource scope for errors raised by guard plugins.
pub const GUARD_PLUGIN_SCHEMA: &str = gts_id!("cf.core.oagw.guard_plugin.v1~");

/// Resource scope for a specific transform plugin instance.
pub const TRANSFORM_PLUGIN_SCHEMA: &str = gts_id!("cf.core.oagw.transform_plugin.v1~");

/// Resource scope for the protocol configuration type.
pub const PROTOCOL_SCHEMA: &str = gts_id!("cf.core.oagw.protocol.v1~");

/// Built-in HTTP protocol instance used by OAGW upstreams and routes.
pub const HTTP_PROTOCOL_ID: &str = gts_id!("cf.core.oagw.protocol.v1~cf.core.oagw.http.v1");

/// Built-in API-key auth plugin instance used by examples and defaults.
pub const APIKEY_AUTH_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1");

// ---------------------------------------------------------------------------
// Typed view of the wire `resource_type` strings above.
//
// Co-located with the constants so a reader sees the wire vocabulary and
// the typed dispatch surface in one place. The projection in `crate::error`
// uses `Resource::from_wire` to populate `ServiceGatewayError::NotFound`
// and `ServiceGatewayError::AlreadyExists`.
// ---------------------------------------------------------------------------

/// Typed discriminator for the wire `resource_type` strings declared above.
///
/// Carried by [`crate::ServiceGatewayError::NotFound::resource`] and
/// [`crate::ServiceGatewayError::AlreadyExists::resource`].
/// [`Self::Unknown`] preserves the raw canonical `resource_type` for any
/// scope the SDK does not specifically model — keeping the projection
/// forward-compatible if the impl crate grows new resource scopes without
/// an SDK bump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// Gateway data-plane / proxy umbrella scope ([`PROXY_SCHEMA`]).
    Proxy,
    /// A specific upstream definition ([`UPSTREAM_SCHEMA`]).
    Upstream,
    /// A specific route definition ([`ROUTE_SCHEMA`]).
    Route,
    /// A specific authentication plugin instance ([`AUTH_PLUGIN_SCHEMA`]).
    AuthPlugin,
    /// A specific guard plugin instance ([`GUARD_PLUGIN_SCHEMA`]).
    GuardPlugin,
    /// A specific transform plugin instance ([`TRANSFORM_PLUGIN_SCHEMA`]).
    TransformPlugin,
    /// Catch-all preserving the raw canonical `resource_type` for
    /// resource scopes the SDK does not specifically model.
    Unknown(String),
}

impl Resource {
    /// Project a canonical wire `resource_type` string into the typed
    /// discriminator. Empty / missing strings map to [`Self::Unknown`]
    /// (with the empty string preserved) so consumers can still match
    /// without losing the wire value.
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            PROXY_SCHEMA => Self::Proxy,
            UPSTREAM_SCHEMA => Self::Upstream,
            ROUTE_SCHEMA => Self::Route,
            AUTH_PLUGIN_SCHEMA => Self::AuthPlugin,
            GUARD_PLUGIN_SCHEMA => Self::GuardPlugin,
            TRANSFORM_PLUGIN_SCHEMA => Self::TransformPlugin,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its canonical GTS string.
    /// Inverse of [`Self::from_wire`] for the modeled variants.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Proxy => PROXY_SCHEMA,
            Self::Upstream => UPSTREAM_SCHEMA,
            Self::Route => ROUTE_SCHEMA,
            Self::AuthPlugin => AUTH_PLUGIN_SCHEMA,
            Self::GuardPlugin => GUARD_PLUGIN_SCHEMA,
            Self::TransformPlugin => TRANSFORM_PLUGIN_SCHEMA,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_gts::GTS_ID_PREFIX;

    /// Sanity-check the literal shape every constant must satisfy — caught at
    /// compile time by the `#[resource_error]` macro on the impl side, but
    /// re-asserted here so a typo in the SDK constant is obvious.
    #[test]
    fn resource_type_strings_have_expected_shape() {
        for s in [
            PROXY_SCHEMA,
            UPSTREAM_SCHEMA,
            ROUTE_SCHEMA,
            AUTH_PLUGIN_SCHEMA,
            GUARD_PLUGIN_SCHEMA,
            TRANSFORM_PLUGIN_SCHEMA,
        ] {
            assert!(s.starts_with(GTS_ID_PREFIX), "missing GTS prefix: {s}");
            assert!(s.ends_with('~'), "missing trailing ~: {s}");
            assert!(
                s.contains(".oagw."),
                "expected .oagw. namespace segment: {s}",
            );
        }
    }

    #[test]
    fn resource_round_trips_each_constant() {
        let cases = [
            (PROXY_SCHEMA, Resource::Proxy),
            (UPSTREAM_SCHEMA, Resource::Upstream),
            (ROUTE_SCHEMA, Resource::Route),
            (AUTH_PLUGIN_SCHEMA, Resource::AuthPlugin),
            (GUARD_PLUGIN_SCHEMA, Resource::GuardPlugin),
            (TRANSFORM_PLUGIN_SCHEMA, Resource::TransformPlugin),
        ];
        for (wire, expected) in cases {
            assert_eq!(Resource::from_wire(wire), expected);
            assert_eq!(expected.as_wire(), wire);
        }
    }

    #[test]
    fn resource_unknown_preserves_wire_string() {
        let raw = gts_id!("cf.future.oagw.something_new.v1~");
        let kind = Resource::from_wire(raw);
        match &kind {
            Resource::Unknown(s) => assert_eq!(s, raw),
            other => panic!("expected Unknown, got {other:?}"),
        }
        assert_eq!(kind.as_wire(), raw);
    }
}
