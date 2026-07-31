//! Typed inputs for the [`GatewayProvider`](crate::GatewayProvider) abstraction.

use bytes::Bytes;
use http::uri::{Authority, Scheme, Uri};

use crate::error::GatewayError;

/// The stable name of a gear, as declared in `#[toolkit::gear(name = ...)]`.
///
/// Used as the registration key by a [`GatewayProvider`](crate::GatewayProvider).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GearName(String);

impl GearName {
    /// Wraps a gear name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Returns the gear name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GearName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for GearName {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for GearName {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// An `OpenAPI` document handed to a [`GatewayProvider`](crate::GatewayProvider),
/// in whichever form the caller already has — avoiding a forced clone or
/// re-serialization at the call site.
#[derive(Clone)]
pub enum OpenApiSpec<'a> {
    /// An owned document.
    Owned(Box<utoipa::openapi::OpenApi>),
    /// A borrowed document.
    Borrowed(&'a utoipa::openapi::OpenApi),
    /// A document already serialized to JSON bytes.
    SerializedJson(Bytes),
}

// `utoipa::openapi::OpenApi` does not implement `Debug`, so we cannot derive it.
// The document itself is large and not useful in logs; report only the variant
// (and byte length for the serialized form).
impl std::fmt::Debug for OpenApiSpec<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Owned(_) => f.write_str("OpenApiSpec::Owned(..)"),
            Self::Borrowed(_) => f.write_str("OpenApiSpec::Borrowed(..)"),
            Self::SerializedJson(bytes) => f
                .debug_tuple("OpenApiSpec::SerializedJson")
                .field(&bytes.len())
                .finish(),
        }
    }
}

/// The network location of a gear's HTTP server: a scheme plus an authority
/// (host and optional port).
///
/// The request path and query are taken from the inbound request at forward
/// time, so they are intentionally **not** stored here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    scheme: Scheme,
    authority: Authority,
}

impl Endpoint {
    /// Builds an endpoint from a scheme and authority.
    #[must_use]
    pub fn new(scheme: Scheme, authority: Authority) -> Self {
        Self { scheme, authority }
    }

    /// Parses an endpoint from a base URI such as `http://calculator:8080`.
    ///
    /// Any path, query, or fragment on `uri` is ignored — only the scheme and
    /// authority are retained.
    ///
    /// # Errors
    /// Returns [`GatewayError::InvalidEndpoint`] if `uri` cannot be parsed, or is
    /// missing a scheme or an authority.
    pub fn parse(uri: &str) -> Result<Self, GatewayError> {
        let parsed: Uri =
            uri.parse()
                .map_err(|err: http::uri::InvalidUri| GatewayError::InvalidEndpoint {
                    uri: uri.to_owned(),
                    reason: err.to_string(),
                })?;
        let parts = parsed.into_parts();
        let scheme = parts.scheme.ok_or_else(|| GatewayError::InvalidEndpoint {
            uri: uri.to_owned(),
            reason: "missing scheme".to_owned(),
        })?;
        let authority = parts
            .authority
            .ok_or_else(|| GatewayError::InvalidEndpoint {
                uri: uri.to_owned(),
                reason: "missing authority (host)".to_owned(),
            })?;
        Ok(Self { scheme, authority })
    }

    /// Returns the endpoint scheme (e.g. `http`).
    #[must_use]
    pub fn scheme(&self) -> &Scheme {
        &self.scheme
    }

    /// Returns the endpoint authority (host and optional port).
    #[must_use]
    pub fn authority(&self) -> &Authority {
        &self.authority
    }
}

#[cfg(test)]
mod tests {
    use super::{Endpoint, GearName};
    use crate::error::GatewayError;

    #[test]
    fn gear_name_roundtrips() {
        let name = GearName::from("calculator");
        assert_eq!(name.as_str(), "calculator");
        assert_eq!(name.to_string(), "calculator");
        assert_eq!(GearName::new("calculator"), name);
    }

    #[test]
    fn endpoint_parse_extracts_scheme_and_authority() {
        let endpoint = Endpoint::parse("http://calculator:8080").expect("valid endpoint");
        assert_eq!(endpoint.scheme().as_str(), "http");
        assert_eq!(endpoint.authority().as_str(), "calculator:8080");
    }

    #[test]
    fn endpoint_parse_ignores_path_and_query() {
        let endpoint =
            Endpoint::parse("https://gear.svc/ignored?x=1").expect("valid endpoint with path");
        assert_eq!(endpoint.scheme().as_str(), "https");
        assert_eq!(endpoint.authority().as_str(), "gear.svc");
    }

    #[test]
    fn endpoint_parse_rejects_missing_scheme() {
        let err = Endpoint::parse("calculator:8080").expect_err("scheme required");
        assert!(matches!(err, GatewayError::InvalidEndpoint { .. }));
    }

    #[test]
    fn endpoint_parse_rejects_empty() {
        let err = Endpoint::parse("").expect_err("empty is invalid");
        assert!(matches!(err, GatewayError::InvalidEndpoint { .. }));
    }
}
