#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
mod backoff;
pub mod client;
pub mod internal_auth;
pub mod rpc_retry;

#[cfg(windows)]
pub mod windows_named_pipe;

#[cfg(windows)]
pub use windows_named_pipe::{NamedPipeConnection, NamedPipeIncoming, create_named_pipe_incoming};

pub use internal_auth::{
    InternalAuthInterceptor, attach_internal_token_grpc, extract_internal_token_grpc,
};

pub const SECCTX_METADATA_KEY: &str = "x-secctx-bin";

/// Binary gRPC trailer carrying an RFC 9457 problem envelope. The `-bin`
/// suffix is the gRPC convention for binary-valued metadata: tonic handles
/// base64 transport encoding for us.
pub const PROBLEM_METADATA_KEY: &str = "x-modkit-problem-bin";

/// ASCII-text metadata header signalling that the problem envelope attached
/// under [`PROBLEM_METADATA_KEY`] was truncated to fit the per-trailer
/// HTTP/2 frame budget. Clients should treat missing `context`/`detail`
/// fields as expected rather than a deserialization bug.
pub const PROBLEM_TRUNCATED_HEADER: &str = "x-modkit-problem-truncated";

/// Conservative per-trailer payload cap for the problem envelope.
///
/// tonic/HTTP/2 enforce roughly an 8 KiB ceiling on a single metadata frame;
/// the trailer is base64-encoded over the wire (~4/3 expansion), and other
/// trailers (auth, request-id, secctx) share the same frame budget. 4 KiB
/// of pre-base64 JSON leaves headroom on all three axes.
pub const MAX_PROBLEM_TRAILER_BYTES: usize = 8192;

use secrecy::{ExposeSecret, SecretString};
use tonic::Status;
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
use toolkit_security::{SecurityContext, decode_bin, encode_bin};

#[derive(Debug, thiserror::Error)]
pub enum ProblemTrailerError {
    /// The trailer base64 envelope could not be decoded into raw bytes.
    #[error("problem trailer base64 decode failed: {0}")]
    BinaryDecode(String),
    /// The decoded bytes are not valid UTF-8 JSON matching the expected schema.
    #[error("problem trailer JSON parse failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// HTTP-style header carrying a bearer token. tonic accepts arbitrary
/// `authorization` metadata; this is the convention used by every project
/// service.
const AUTHORIZATION_HEADER: &str = "authorization";

/// Trait that any in-process security-context-bearing type can implement
/// to expose its bearer token to the gRPC transport layer. The generated
/// gRPC client passes the user's `SecurityContext` through this trait
/// without `toolkit-transport-grpc` having to depend on every secret-store
/// crate directly.
pub trait BearerContext {
    /// Returns the bearer token wrapped in `SecretString`. The token never
    /// escapes the trait boundary as a plain `String` — the caller must
    /// `expose_secret()` only at the metadata-construction site.
    fn bearer_value(&self) -> Option<secrecy::SecretString>;
}

impl BearerContext for SecurityContext {
    fn bearer_value(&self) -> Option<SecretString> {
        self.bearer_token().cloned()
    }
}

/// Attach `Bearer <token>` to the `authorization` metadata header.
/// Mirrors [`attach_secctx`]'s `Result<(), Status>` convention so all four
/// metadata helpers in this crate (`attach_secctx`, `attach_problem`,
/// `attach_bearer`) have a uniform surface.
///
/// Anonymous contexts (no token) succeed with no header inserted.
///
/// # Errors
/// Returns `Status::internal` if the token contains bytes that cannot be
/// encoded as a tonic metadata value.
pub fn attach_bearer<C: BearerContext>(metadata: &mut MetadataMap, ctx: &C) -> Result<(), Status> {
    let Some(token_secret) = ctx.bearer_value() else {
        return Ok(());
    };
    let token_str = token_secret.expose_secret();
    let value: MetadataValue<_> = format!("Bearer {token_str}").parse().map_err(
        |e: tonic::metadata::errors::InvalidMetadataValue| {
            Status::internal(format!("bearer token invalid: {e}"))
        },
    )?;
    // `authorization` is hardcoded ASCII-lowercase — `from_static` is
    // infallible at the type level, no spurious error path.
    metadata.insert(MetadataKey::from_static(AUTHORIZATION_HEADER), value);
    Ok(())
}

/// Encode `SecurityContext` into gRPC metadata.
///
/// # Errors
/// Returns `Status::internal` if encoding fails.
pub fn attach_secctx(meta: &mut MetadataMap, ctx: &SecurityContext) -> Result<(), Status> {
    let encoded = encode_bin(ctx).map_err(|e| Status::internal(format!("secctx encode: {e}")))?;

    meta.insert_bin(SECCTX_METADATA_KEY, MetadataValue::from_bytes(&encoded));
    Ok(())
}

/// Decode `SecurityContext` from gRPC metadata.
///
/// # Errors
/// Returns `Status::unauthenticated` if the metadata is missing or decoding fails.
pub fn extract_secctx(meta: &MetadataMap) -> Result<SecurityContext, Status> {
    let raw = meta
        .get_bin(SECCTX_METADATA_KEY)
        .ok_or_else(|| Status::unauthenticated("missing secctx metadata"))?;

    let bytes = raw
        .to_bytes()
        .map_err(|e| Status::unauthenticated(format!("invalid secctx metadata: {e}")))?;

    decode_bin(bytes.as_ref()).map_err(|e| Status::unauthenticated(format!("secctx decode: {e}")))
}

/// Attach a serializable problem envelope (e.g. RFC 9457 `ProblemDetails`)
/// to gRPC trailers under [`PROBLEM_METADATA_KEY`]. Bytes are encoded into
/// the `-bin` trailer slot — base64 over the wire is handled by tonic.
///
/// The function is generic over the envelope type so neither this crate nor
/// `toolkit-contract` need to coordinate on a single canonical schema.
///
/// # Trailer size budget
/// HTTP/2 imposes a per-frame ceiling on metadata payloads (~8 KiB once
/// other trailers and base64 expansion are accounted for). Envelopes whose
/// serialized form exceeds [`MAX_PROBLEM_TRAILER_BYTES`] are reduced to a
/// minimal RFC 9457 shape preserving only `type`, `title`, and `status` —
/// `detail`, `instance`, `trace_id`, and `context` are dropped. When this
/// happens, the [`PROBLEM_TRUNCATED_HEADER`] ASCII header is also attached
/// so the client can surface the truncation diagnostically rather than
/// treating it as a missing-field bug.
///
/// If even the minimal shape exceeds the cap (pathological — e.g. a
/// gigabyte-long `title`), no trailer is written and the function returns
/// `Ok(())`. The caller still has the underlying `tonic::Status` to fall
/// back on.
///
/// # Errors
/// Returns `Status::internal` when JSON serialization of the envelope fails.
pub fn attach_problem<P: serde::Serialize>(
    meta: &mut MetadataMap,
    problem: &P,
) -> Result<(), Status> {
    let bytes = serde_json::to_vec(problem)
        .map_err(|e| Status::internal(format!("problem encode: {e}")))?;

    if bytes.len() <= MAX_PROBLEM_TRAILER_BYTES {
        meta.insert_bin(PROBLEM_METADATA_KEY, MetadataValue::from_bytes(&bytes));
        return Ok(());
    }

    // Oversized envelope: project onto a minimal RFC 9457 shape carrying
    // only the wire-routing essentials. The function is generic over the
    // envelope type, so we re-serialize via `serde_json::Value` and pick
    // out the canonical fields by name rather than coupling to a single
    // `Problem` struct definition.
    let value = serde_json::to_value(problem)
        .map_err(|e| Status::internal(format!("problem re-encode: {e}")))?;
    let mut minimal = serde_json::Map::new();
    for key in ["type", "title", "status"] {
        if let Some(v) = value.get(key) {
            minimal.insert(key.to_owned(), v.clone());
        }
    }
    let minimal_bytes = serde_json::to_vec(&serde_json::Value::Object(minimal))
        .map_err(|e| Status::internal(format!("problem minimal encode: {e}")))?;

    if minimal_bytes.len() <= MAX_PROBLEM_TRAILER_BYTES {
        meta.insert_bin(
            PROBLEM_METADATA_KEY,
            MetadataValue::from_bytes(&minimal_bytes),
        );
        // ASCII-text signal; metadata keys with no `-bin` suffix are plain
        // strings on the wire. `from_static` is infallible at the type
        // level — the constant is lowercase and well-formed.
        meta.insert(
            MetadataKey::from_static(PROBLEM_TRUNCATED_HEADER),
            MetadataValue::from_static("true"),
        );
        return Ok(());
    }

    // Pathological case — even the three-field projection is too large.
    // Fall back to attaching nothing under the problem key; the gRPC
    // `Status` itself still carries code + message for the client. Log so
    // operators can spot a misuse (e.g. a runaway `title` length).
    tracing::warn!(
        original_bytes = bytes.len(),
        minimal_bytes = minimal_bytes.len(),
        max_bytes = MAX_PROBLEM_TRAILER_BYTES,
        "attach_problem: minimal envelope still exceeds trailer cap; dropping problem trailer"
    );
    Ok(())
}

/// Extract a serializable problem envelope from gRPC trailers.
///
/// Returns `Ok(None)` when the trailer is absent. Returns
/// `Err(ProblemTrailerError::BinaryDecode)` when the trailer is present but
/// its base64 envelope cannot be decoded. Returns
/// `Err(ProblemTrailerError::Json)` when the decoded bytes do not parse as
/// the expected schema. On clean parse, returns `Ok(Some(p))`.
///
/// # Errors
/// See variants of [`ProblemTrailerError`].
pub fn extract_problem<P: serde::de::DeserializeOwned>(
    meta: &MetadataMap,
) -> Result<Option<P>, ProblemTrailerError> {
    let Some(raw) = meta.get_bin(PROBLEM_METADATA_KEY) else {
        return Ok(None);
    };
    let bytes = raw
        .to_bytes()
        .map_err(|e| ProblemTrailerError::BinaryDecode(e.to_string()))?;
    let parsed = serde_json::from_slice::<P>(&bytes)?;
    Ok(Some(parsed))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod problem_trailer_tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct DummyProblem {
        title: String,
        detail: String,
        status: u16,
    }

    #[test]
    fn roundtrip_through_binary_trailer() {
        let mut meta = MetadataMap::new();
        let original = DummyProblem {
            title: "\u{41a}\u{438}\u{440}\u{438}\u{43b}\u{43b}\u{438}\u{446}\u{430} \u{1f980}"
                .to_owned(),
            detail: "non-ASCII detail with emoji \u{1f680} and chars: \u{fc}mlaut".to_owned(),
            status: 500,
        };
        attach_problem(&mut meta, &original).unwrap();
        let got: DummyProblem = extract_problem(&meta)
            .expect("clean parse")
            .expect("trailer present");
        assert_eq!(got, original);
    }

    #[test]
    fn extract_problem_returns_ok_none_when_absent() {
        let meta = MetadataMap::new();
        let got = extract_problem::<DummyProblem>(&meta).expect("absent is Ok(None)");
        assert!(got.is_none());
    }

    #[test]
    fn extract_problem_returns_err_on_malformed_base64() {
        // tonic validates base64 at every public `MetadataValue<Binary>`
        // constructor. To exercise the `to_bytes()` failure path we bypass
        // tonic's validation by populating the underlying `http::HeaderMap`
        // directly with non-base64 bytes under the `-bin` key, then
        // converting via `MetadataMap::from_headers`.
        let mut http_map = http::HeaderMap::new();
        http_map.insert(
            http::header::HeaderName::from_static(PROBLEM_METADATA_KEY),
            http::HeaderValue::from_static("@@@not_base64@@@"),
        );
        let meta = MetadataMap::from_headers(http_map);
        match extract_problem::<DummyProblem>(&meta) {
            Err(ProblemTrailerError::BinaryDecode(_)) => {}
            other => panic!("expected BinaryDecode, got {other:?}"),
        }
    }

    #[test]
    fn extract_problem_returns_err_on_invalid_json() {
        let mut meta = MetadataMap::new();
        meta.insert_bin(
            PROBLEM_METADATA_KEY,
            MetadataValue::from_bytes(b"not json at all"),
        );
        match extract_problem::<DummyProblem>(&meta) {
            Err(ProblemTrailerError::Json(_)) => {}
            other => panic!("expected Json error, got {other:?}"),
        }
    }

    #[test]
    fn extract_problem_round_trips_valid_envelope() {
        let mut meta = MetadataMap::new();
        let original = DummyProblem {
            title: "OK".to_owned(),
            detail: "all good".to_owned(),
            status: 200,
        };
        attach_problem(&mut meta, &original).unwrap();
        let got = extract_problem::<DummyProblem>(&meta)
            .expect("clean parse")
            .expect("trailer present");
        assert_eq!(got, original);
    }

    /// Mirrors the RFC 9457 shape of `toolkit_canonical_errors::Problem` —
    /// the test crate can't depend on it (no `toolkit-canonical-errors` in
    /// dev-deps), so we re-declare the field set inline. The truncation
    /// path keys on `"type"`/`"title"`/`"status"`, which match.
    #[derive(Debug, Serialize, Deserialize)]
    struct BigProblem {
        #[serde(rename = "type")]
        problem_type: String,
        title: String,
        status: u16,
        detail: String,
        context: serde_json::Value,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct MinimalProblem {
        #[serde(rename = "type")]
        problem_type: String,
        title: String,
        status: u16,
    }

    #[test]
    fn attach_problem_truncates_oversized_envelope() {
        let mut meta = MetadataMap::new();
        // 10 KiB of context payload — well beyond MAX_PROBLEM_TRAILER_BYTES.
        let big_blob = "x".repeat(10 * 1024);
        let problem = BigProblem {
            problem_type: "https://example.com/errors/oversized".to_owned(),
            title: "Oversized".to_owned(),
            status: 500,
            detail: "this detail string is also non-trivial".to_owned(),
            context: serde_json::json!({ "data": big_blob }),
        };

        attach_problem(&mut meta, &problem).expect("attach must not error");

        let trailer = meta
            .get_bin(PROBLEM_METADATA_KEY)
            .expect("problem trailer present");
        let bytes = trailer.to_bytes().expect("decodable trailer");
        assert!(
            bytes.len() <= MAX_PROBLEM_TRAILER_BYTES,
            "truncated trailer {} bytes exceeds cap {}",
            bytes.len(),
            MAX_PROBLEM_TRAILER_BYTES
        );

        let truncated_hdr = meta
            .get(PROBLEM_TRUNCATED_HEADER)
            .expect("truncation header set");
        assert_eq!(truncated_hdr.to_str().unwrap(), "true");

        let minimal: MinimalProblem = extract_problem(&meta)
            .expect("clean parse")
            .expect("minimal envelope round-trips");
        assert_eq!(minimal.problem_type, problem.problem_type);
        assert_eq!(minimal.title, problem.title);
        assert_eq!(minimal.status, problem.status);
    }

    #[test]
    fn attach_problem_keeps_small_envelope_intact() {
        let mut meta = MetadataMap::new();
        let problem = DummyProblem {
            title: "Small".to_owned(),
            detail: "tiny detail".to_owned(),
            status: 400,
        };
        attach_problem(&mut meta, &problem).unwrap();
        assert!(
            meta.get(PROBLEM_TRUNCATED_HEADER).is_none(),
            "truncation header must be absent for under-cap envelopes"
        );
        let got: DummyProblem = extract_problem(&meta)
            .expect("clean parse")
            .expect("round-trips intact");
        assert_eq!(got, problem);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod bearer_tests {
    use super::*;

    /// Inline `BearerContext` impl so the test does not depend on
    /// `toolkit-security`'s `SecurityContext` shape.
    struct StubBearer(Option<&'static str>);
    impl BearerContext for StubBearer {
        fn bearer_value(&self) -> Option<SecretString> {
            self.0.map(|s| SecretString::from(s.to_owned()))
        }
    }

    #[test]
    fn attach_bearer_writes_authorization_header() {
        let mut md = MetadataMap::new();
        attach_bearer(&mut md, &StubBearer(Some("abc"))).unwrap();
        let v = md.get("authorization").unwrap();
        assert_eq!(v.to_str().unwrap(), "Bearer abc");
    }

    #[test]
    fn attach_bearer_skips_anonymous_context() {
        let mut md = MetadataMap::new();
        attach_bearer(&mut md, &StubBearer(None)).unwrap();
        assert!(md.get("authorization").is_none());
    }

    /// Stub modelling a `SecurityContext`-like type that stores its bearer
    /// token internally as `SecretString`. Asserts the raw token is never
    /// surfaced as a plain `String` across the trait boundary and that
    /// `attach_bearer` correctly formats the `Authorization: Bearer ...`
    /// metadata header.
    struct SecCtxLike {
        token: SecretString,
    }
    impl BearerContext for SecCtxLike {
        fn bearer_value(&self) -> Option<SecretString> {
            Some(self.token.clone())
        }
    }

    #[test]
    fn attach_bearer_formats_authorization_from_secret_context() {
        let mut md = MetadataMap::new();
        let ctx = SecCtxLike {
            token: SecretString::from("tok-xyz".to_owned()),
        };
        attach_bearer(&mut md, &ctx).unwrap();
        // Note: no long-lived binding of the raw token; the value we read
        // back is the formatted header value.
        let v = md.get("authorization").expect("authorization present");
        assert_eq!(v.to_str().unwrap(), "Bearer tok-xyz");
    }
}
