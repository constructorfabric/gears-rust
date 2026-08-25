//! Query-parameter contract shared by the generated client, the generated
//! server route, and the `OpenAPI` spec.
//!
//! # Why this module exists
//!
//! The three consumers of a query parameter used to disagree. The client
//! flattened the parameter to JSON and hand-rolled a query string; the server
//! decoded with `serde_urlencoded` via `axum::extract::Query`; the spec
//! described whatever the macro could infer from the Rust type at expansion
//! time. Nothing reconciled them, so several shapes compiled cleanly and then
//! failed on every request:
//!
//! * a scalar parameter (`count: u64`) produced `Query<u64>`, and
//!   `serde_urlencoded`'s top-level deserializer only yields a map — a
//!   guaranteed 400 against a spec that advertised the parameter as valid;
//! * a `Vec<T>` field went out as repeated keys, which `serde_urlencoded`
//!   cannot collect into a sequence;
//! * a nested struct lost its outer key entirely during flattening.
//!
//! # The contract now
//!
//! A query parameter is a struct deriving [`macro@crate::QueryParams`]. Both
//! ends encode and decode it with `serde_html_form`, which round-trips repeated
//! keys as sequences, and the derive emits the parameter list the `OpenAPI` spec
//! is built from — so the spec is generated from the same declaration the wire
//! format is.
//!
//! Each field's leaf type must implement [`QueryScalar`]. That bound is what
//! rejects nesting: a query string is a flat list of key/value pairs, and
//! neither `serde_html_form` nor any other single-level codec can represent a
//! sub-object unambiguously.

use serde::Serialize;

/// A type that can appear as a query-parameter value.
///
/// Implemented for the primitives; implement it for a domain newtype or a
/// unit-only enum that serializes as a plain string. The bound exists to keep
/// nested structs out of query parameters — see the module docs.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be a query-parameter field",
    label = "not a query scalar",
    note = "a query string is a flat list of key/value pairs, so a query struct's fields must be \
            scalars, `Option<scalar>`, or `Vec<scalar>` — nested structs cannot be represented",
    note = "for a unit-only enum or a newtype that serializes as a string, add \
            `impl toolkit_contract::query::QueryScalar for {Self} {{}}`",
    note = "for a genuinely nested shape, mark the method `#[server_manual]` and handle the query \
            string by hand"
)]
pub trait QueryScalar {
    /// The `OpenAPI` primitive type this value renders as.
    const OPENAPI_TYPE: &'static str = "string";
}

macro_rules! impl_query_scalar {
    ($ty:ty => $openapi:literal) => {
        impl QueryScalar for $ty {
            const OPENAPI_TYPE: &'static str = $openapi;
        }
    };
}

impl_query_scalar!(String => "string");
impl_query_scalar!(str => "string");
impl_query_scalar!(bool => "boolean");
impl_query_scalar!(f32 => "number");
impl_query_scalar!(f64 => "number");
impl_query_scalar!(i8 => "integer");
impl_query_scalar!(i16 => "integer");
impl_query_scalar!(i32 => "integer");
impl_query_scalar!(i64 => "integer");
impl_query_scalar!(i128 => "integer");
impl_query_scalar!(isize => "integer");
impl_query_scalar!(u8 => "integer");
impl_query_scalar!(u16 => "integer");
impl_query_scalar!(u32 => "integer");
impl_query_scalar!(u64 => "integer");
impl_query_scalar!(u128 => "integer");
impl_query_scalar!(usize => "integer");

/// One query parameter as it should appear in the `OpenAPI` document.
///
/// Emitted by `#[derive(QueryParams)]` and consumed by the generated server
/// route, so the spec cannot drift from what the client actually sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryParamSpec {
    /// Wire name of the parameter.
    pub name: &'static str,
    /// `OpenAPI` type of a single value (the *item* type when `array`).
    pub openapi_type: &'static str,
    /// `false` for `Option<..>` fields.
    pub required: bool,
    /// `true` for `Vec<..>` fields, which encode as repeated keys.
    pub array: bool,
}

/// A struct usable as a REST query parameter.
///
/// Derive it with `#[derive(toolkit_contract::QueryParams)]`; the derive also
/// checks each field's leaf type against [`QueryScalar`].
pub trait QueryParams {
    /// The parameters this struct contributes to an operation's `OpenAPI` spec.
    fn openapi_params() -> &'static [QueryParamSpec];
}

/// Serialize a query struct into a query string (no leading `?`).
///
/// Returns `None` when the result is empty, so the caller can skip the `?`
/// entirely. `Vec` fields become repeated keys and `None` fields are omitted.
///
/// # Errors
/// Returns the underlying `serde_html_form` error if `value` cannot be
/// represented as a flat key/value list.
pub fn to_query_string<T: Serialize>(
    value: &T,
) -> Result<Option<String>, serde_html_form::ser::Error> {
    let encoded = serde_html_form::to_string(value)?;
    if encoded.is_empty() {
        Ok(None)
    } else {
        Ok(Some(encoded))
    }
}

#[cfg(feature = "rest-server")]
pub use extract::QueryParamsExtractor;

#[cfg(feature = "rest-server")]
mod extract {
    use axum::extract::FromRequestParts;
    use axum::http::request::Parts;
    use axum::response::{IntoResponse, Response};
    use serde::de::DeserializeOwned;
    use toolkit_canonical_errors::resource_error;

    /// GTS-typed error scope for query-string decoding on generated routes.
    #[resource_error(gts_id!("cf.core.contract.query.v1~"))]
    pub struct QueryError;

    /// Axum extractor for a query struct, decoded with the same
    /// `serde_html_form` codec the generated client encodes with.
    ///
    /// Replaces `axum::extract::Query`, which decodes via `serde_urlencoded`
    /// and therefore cannot collect repeated keys into a `Vec`. It also
    /// rejects with a canonical error, so a malformed query string comes back
    /// as the same RFC 9457 Problem envelope as every other error from a
    /// generated route instead of axum's plain-text 400.
    pub struct QueryParamsExtractor<T>(pub T);

    impl<T, S> FromRequestParts<S> for QueryParamsExtractor<T>
    where
        T: DeserializeOwned,
        S: Send + Sync,
    {
        type Rejection = Response;

        async fn from_request_parts(
            parts: &mut Parts,
            _state: &S,
        ) -> Result<Self, Self::Rejection> {
            let raw = parts.uri.query().unwrap_or_default();
            match serde_html_form::from_str::<T>(raw) {
                Ok(value) => Ok(Self(value)),
                Err(e) => Err(QueryError::invalid_argument()
                    .with_field_violation("query", format!("{e}"), "INVALID_QUERY_STRING")
                    .create()
                    .into_response()),
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Filter {
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        // `#[serde(default)]` is mandatory on a repeated field: an empty `Vec`
        // emits no key at all, so without it the round trip fails with
        // `missing field`. `#[derive(QueryParams)]` enforces this.
        #[serde(default)]
        tags: Vec<String>,
        limit: u32,
    }

    /// The property that matters: whatever the client writes, the server reads
    /// back unchanged. Each case below is a shape that silently produced a 400
    /// under the old split codec.
    fn round_trip(value: &Filter) -> Filter {
        let encoded = to_query_string(value).unwrap().unwrap_or_default();
        serde_html_form::from_str(&encoded).unwrap()
    }

    #[test]
    fn round_trips_a_vec_field_as_repeated_keys() {
        let value = Filter {
            status: Some("paid".to_owned()),
            tags: vec!["a".to_owned(), "b".to_owned()],
            limit: 10,
        };
        let encoded = to_query_string(&value).unwrap().unwrap();
        assert!(
            encoded.contains("tags=a") && encoded.contains("tags=b"),
            "expected repeated keys, got: {encoded}"
        );
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn round_trips_an_empty_vec() {
        // An empty `Vec` emits no key at all, so the field must still
        // deserialize — `serde_html_form` yields an empty sequence.
        let value = Filter {
            status: None,
            tags: Vec::new(),
            limit: 0,
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn omits_a_none_option() {
        let value = Filter {
            status: None,
            tags: vec!["x".to_owned()],
            limit: 1,
        };
        let encoded = to_query_string(&value).unwrap().unwrap();
        assert!(
            !encoded.contains("status"),
            "None must not appear on the wire, got: {encoded}"
        );
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn empty_struct_yields_no_query_string() {
        #[derive(Serialize)]
        struct Empty;
        assert_eq!(to_query_string(&Empty {}).unwrap(), None);
    }
}
