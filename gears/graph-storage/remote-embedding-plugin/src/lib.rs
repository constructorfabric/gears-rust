//! Remote embedding provider for the graph-storage gear.
//!
//! ADR-0004 names two real providers: the in-process ONNX default and *"a
//! second plugin [that] calls a remote inference endpoint"*. This is the
//! second one. It speaks the `OpenAI`-compatible `POST /embeddings` protocol,
//! which is what `OpenAI`, Azure `OpenAI`, Groq, Together, Ollama, vLLM and
//! most self-hosted inference servers expose, so one plugin covers the
//! deployments that cannot run a model in the gear's own process — a memory
//! ceiling, a CPU budget, or a platform that already pays for an inference
//! service.
//!
//! # What the identity can and cannot promise
//!
//! The ONNX plugin names its embedding space by the SHA-256 of the bytes it
//! loaded. A remote endpoint offers no bytes to hash: the only identity it
//! has is *which model, at which endpoint, at which width*, and that is what
//! this plugin declares. Two deployments pointing one model name at one host
//! agree on the space; the same name at a different host, or a different
//! requested width, do not. What no remote identity can catch is a vendor
//! silently changing the weights behind a stable model name — ADR-0004 puts
//! that under model governance rather than under the plugin, and it is the
//! reason the ADR calls remote embedding *governed data egress* rather than an
//! ordinary plugin call.
//!
//! # Vectors are normalized here
//!
//! The gear's index serves cosine similarity, and not every compatible
//! endpoint returns unit vectors (`OpenAI` does, several self-hosted servers
//! do not). Normalizing on this side makes the stored vectors comparable
//! whatever the endpoint's habit, and is part of the declared identity.

use std::time::Duration;

use async_trait::async_trait;
use graph_storage_sdk::models::EmbeddingSpaceId;
use graph_storage_sdk::plugin_api::{
    EmbedRequest, EmbedResponse, EmbeddingProviderError, EmbeddingProviderV1,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};
use url::Url;

/// What a deployment declares about its endpoint.
#[derive(Clone, Debug)]
pub struct RemoteProviderConfig {
    /// API root the `/embeddings` path is appended to, e.g.
    /// `https://api.openai.com/v1` or `http://ollama:11434/v1`.
    pub base_url: String,
    /// Model name as the endpoint knows it, e.g. `text-embedding-3-small`.
    pub model: String,
    /// Bearer credential. `None` for an endpoint that takes no credential
    /// (a self-hosted server on a private network).
    pub api_key: Option<SecretString>,
    /// Vector width the deployment's column was migrated with. Every vector
    /// the endpoint returns is checked against it.
    pub dimension: u32,
    /// Send the `dimensions` request field. Models that support Matryoshka
    /// truncation (`text-embedding-3-*`) then return exactly `dimension`
    /// lanes; a model of a fixed width ignores or rejects the field, and a
    /// deployment on such a model turns this off and sets `dimension` to the
    /// model's native width.
    pub request_dimensions: bool,
    /// L2-normalize every vector before it is stored.
    pub normalize: bool,
    /// Inputs per request. The endpoint's own limit is the ceiling; 64 is
    /// well under every known one.
    pub batch_size: usize,
    /// Per-request timeout. The caller's budget shortens it, never lengthens.
    pub timeout: Duration,
    /// How many times a chunk is re-sent after a *transient* refusal — a
    /// rate limit or a gateway that is briefly unwell.
    ///
    /// One 503 otherwise drops a whole ingest batch's vectors, and the nodes
    /// stay unembedded until something touches them again. Zero turns retries
    /// off. The caller's remaining budget is the real ceiling: a retry that
    /// would start after the deadline is not attempted, so this can never
    /// make a request outlive the request that asked for it.
    pub max_retries: u32,
}

impl RemoteProviderConfig {
    /// The `text-embedding-3-small` shape at the gear's default width: 384
    /// lanes requested through the `dimensions` field, normalized.
    #[must_use]
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: None,
            dimension: 384,
            request_dimensions: true,
            normalize: true,
            batch_size: 64,
            timeout: Duration::from_mins(1),
            max_retries: 2,
        }
    }

    /// Attach the bearer credential.
    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(SecretString::from(api_key.into()));
        self
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RemoteConfigError {
    #[error("base_url {0:?} is not an absolute http(s) URL")]
    BaseUrl(String),
    #[error(
        "base_url carries credentials in its userinfo; put the key in the environment variable \
         `embedding_remote_api_key_env` names instead, where it is not part of a URL that gets \
         logged, echoed in an error, or copied into a bug report"
    )]
    CredentialsInUrl,
    #[error(
        "an api_key is configured and base_url is plain http to {host}, which puts the bearer \
         token on the wire in clear for every proxy and log in between; use https, or a \
         loopback host if this is a local endpoint"
    )]
    CredentialOverPlainHttp { host: String },
    #[error("model must not be empty")]
    Model,
    #[error("model must be at most {max} characters, and this one is {got}")]
    ModelTooLong { max: usize, got: usize },
    #[error(
        "model must not carry control characters: it becomes part of the embedding-space \
         identity, which is compared for equality and written to the operator log"
    )]
    ModelControlCharacters,
    #[error("dimension must be positive")]
    Dimension,
    #[error("batch_size must be positive")]
    BatchSize,
    #[error("the HTTP client could not be built: {0}")]
    Client(String),
}

/// Whether a URL's host is the local machine.
///
/// A literal `127.0.0.0/8` or `::1` address, or the name `localhost`. The name
/// is included because that is how a local endpoint is usually written, and
/// resolving it to check would make configuration validation depend on DNS --
/// which would also be a lie, since resolution can change after boot.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// The longest a model name may be, in characters.
///
/// Generous next to any real one -- `text-embedding-3-small` is 22 -- because
/// the point is to have a bound at all rather than to guess a vendor's naming.
/// An identity this string is part of gets compared and logged, so it cannot be
/// unbounded.
const MAX_MODEL_LEN: usize = 200;

/// What an endpoint that cannot embed an empty string is asked to embed
/// instead. The coordinator composes a node's input from its name and
/// declared payload paths, so an empty input is a node with neither — rare,
/// but the contract requires it to get a vector aligned with its position.
const EMPTY_INPUT_PLACEHOLDER: &str = "(empty)";

/// An `OpenAI`-compatible `/embeddings` endpoint, as the gear's provider.
pub struct RemoteEmbeddingProvider {
    http: reqwest::Client,
    /// The full `/embeddings` URL, resolved once.
    endpoint: Url,
    config: RemoteProviderConfig,
    space: EmbeddingSpaceId,
}

impl RemoteEmbeddingProvider {
    /// Validate the configuration and build the client.
    ///
    /// Nothing is sent: the identity is declarative, and a deployment whose
    /// endpoint is down at boot should still start and report the arm as
    /// unavailable when asked, not refuse to boot.
    ///
    /// # Errors
    ///
    /// A base URL that is not absolute `http(s)`, an empty model, a zero
    /// width or batch size, or a client that cannot be constructed.
    pub fn new(config: RemoteProviderConfig) -> Result<Self, RemoteConfigError> {
        let base = Url::parse(config.base_url.trim_end_matches('/'))
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
            .ok_or_else(|| RemoteConfigError::BaseUrl(config.base_url.clone()))?;
        // `https://user:key@host/v1` parses, and the userinfo then rides on
        // the endpoint this provider hands out and prints. A credential
        // belongs in the environment variable the configuration names, which
        // is the one place this plugin already reads one from.
        if !base.username().is_empty() || base.password().is_some() {
            return Err(RemoteConfigError::CredentialsInUrl);
        }
        // A bearer token is a header, and a header on plain `http` is on the
        // wire in clear: every proxy, NAT, TLS-inspecting middlebox and load
        // balancer log between here and the endpoint reads it. The scheme
        // check above allows `http` because a local endpoint is a real thing
        // to run against; what it cannot allow is `http` to somewhere else
        // while a credential is attached, and `embed_chunk` attaches it
        // whenever one is configured, without consulting the scheme.
        //
        // Loopback stays allowed with a key. There the traffic does not leave
        // the machine, and refusing it would push a developer towards putting
        // the credential somewhere worse.
        if config.api_key.is_some() && base.scheme() == "http" && !is_loopback(&base) {
            return Err(RemoteConfigError::CredentialOverPlainHttp {
                host: base.host_str().unwrap_or_default().to_owned(),
            });
        }
        // The model is not a free-text field. It is trimmed into the
        // embedding-space identity, which is compared for equality to decide
        // whether a stored vector is still valid, it is written to the
        // operator log at boot, and it is sent in the body of every request.
        // So it is bounded and shaped here, where `base_url` and the two
        // counts are already checked -- refusing at configuration time rather
        // than discovering it in an identity comparison or a log line.
        let model = config.model.trim();
        if model.is_empty() {
            return Err(RemoteConfigError::Model);
        }
        if model.chars().count() > MAX_MODEL_LEN {
            return Err(RemoteConfigError::ModelTooLong {
                max: MAX_MODEL_LEN,
                got: model.chars().count(),
            });
        }
        if model.chars().any(char::is_control) {
            return Err(RemoteConfigError::ModelControlCharacters);
        }
        if config.dimension == 0 {
            return Err(RemoteConfigError::Dimension);
        }
        if config.batch_size == 0 {
            return Err(RemoteConfigError::BatchSize);
        }

        let mut endpoint = base.clone();
        endpoint.set_path(&format!("{}/embeddings", base.path().trim_end_matches('/')));
        endpoint.set_query(None);

        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|error| RemoteConfigError::Client(error.to_string()))?;

        // The endpoint's origin and path are the "artifact"; the query string
        // and a trailing slash are not part of which model answers, so they
        // are not part of the identity either.
        let space = EmbeddingSpaceId::new(
            format!("{}@{}", config.model.trim(), endpoint_name(&endpoint)),
            "provider-managed",
            serde_json::json!({
                "protocol": "openai-embeddings-v1",
                "requested_dimensions": config.request_dimensions.then_some(config.dimension),
                "empty_input": EMPTY_INPUT_PLACEHOLDER,
            }),
            serde_json::json!({ "strategy": "provider" }),
            serde_json::json!({ "l2": config.normalize }),
            config.dimension,
        );

        Ok(Self {
            http,
            endpoint,
            config,
            space,
        })
    }

    /// The resolved `/embeddings` URL, for logs and diagnostics.
    #[must_use]
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
}

/// `host[:port]/path` — what distinguishes one endpoint from another.
fn endpoint_name(endpoint: &Url) -> String {
    // The scheme is part of the identity, not decoration: `http://host/v1`
    // and `https://host/v1` are different endpoints, and without it a
    // downgrade or a routing change that keeps the host and path would reuse
    // vectors from what is, as far as this gear can tell, another provider.
    let scheme = endpoint.scheme();
    let host = endpoint.host_str().unwrap_or("unknown-host");
    match endpoint.port() {
        Some(port) => format!("{scheme}://{host}:{port}{}", endpoint.path()),
        None => format!("{scheme}://{host}{}", endpoint.path()),
    }
}

#[derive(Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    input: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    #[serde(default)]
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    index: usize,
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingProviderV1 for RemoteEmbeddingProvider {
    fn embedding_space(&self) -> &EmbeddingSpaceId {
        &self.space
    }

    fn dimension(&self) -> u32 {
        self.config.dimension
    }

    async fn embed(&self, req: EmbedRequest) -> Result<EmbedResponse, EmbeddingProviderError> {
        if req.cancel.is_cancelled() {
            return Err(EmbeddingProviderError::Cancelled);
        }
        if req.budget.is_exhausted() {
            return Err(EmbeddingProviderError::Deadline);
        }

        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(req.inputs.len());
        for chunk in req.inputs.chunks(self.config.batch_size) {
            // Checked per round trip: a batch that outlives its deadline in
            // the middle should not spend another request on the remainder.
            if req.cancel.is_cancelled() {
                return Err(EmbeddingProviderError::Cancelled);
            }
            let remaining = req.budget.remaining();
            if remaining.is_zero() {
                return Err(EmbeddingProviderError::Deadline);
            }
            let batch = self.embed_chunk_with_retries(chunk, &req).await?;
            vectors.extend(batch);
        }

        Ok(EmbedResponse {
            vectors,
            space: self.space.clone(),
        })
    }

    /// One short request. The endpoint has no cheaper probe that every
    /// compatible server implements, so readiness costs one embedding.
    async fn health(&self) -> Result<(), EmbeddingProviderError> {
        self.embed_chunk(
            &["health".to_owned()],
            self.config.timeout.min(Duration::from_secs(10)),
        )
        .await
        .map(drop)
        .map_err(|refusal| refusal.error)
    }
}

impl RemoteEmbeddingProvider {
    /// One chunk, re-sent while the refusal is transient and the budget has
    /// room for another attempt.
    ///
    /// Without this a single 503 or rate limit drops the vectors of a whole
    /// ingest batch, and those nodes stay unembedded until something touches
    /// them again — a quiet loss of recall rather than a visible failure.
    /// Only the refusals a retry can fix are retried, and the caller's
    /// remaining budget bounds the whole sequence: an attempt that would
    /// start after the deadline is not made, so a retry can never make a
    /// request outlive the one that asked for it.
    async fn embed_chunk_with_retries(
        &self,
        chunk: &[String],
        req: &EmbedRequest,
    ) -> Result<Vec<Vec<f32>>, EmbeddingProviderError> {
        let mut backoff = Duration::from_millis(200);
        for attempt in 0..=self.config.max_retries {
            let remaining = req.budget.remaining();
            if remaining.is_zero() {
                return Err(EmbeddingProviderError::Deadline);
            }
            let call = self.embed_chunk(chunk, remaining.min(self.config.timeout));
            let outcome = tokio::select! {
                () = req.cancel.cancelled() => return Err(EmbeddingProviderError::Cancelled),
                result = call => result,
            };
            let refusal = match outcome {
                Ok(vectors) => return Ok(vectors),
                Err(refusal) => refusal,
            };
            // A credential the endpoint refuses, a malformed answer or a
            // width that does not match are not going to be different next
            // time; a deadline is the caller's, not the endpoint's. Each said
            // so at the point it was classified.
            if attempt == self.config.max_retries || !refusal.retryable {
                return Err(refusal.error);
            }
            let wait = backoff.min(req.budget.remaining());
            if wait.is_zero() {
                return Err(refusal.error);
            }
            warn!(
                endpoint = %endpoint_name(&self.endpoint),
                attempt = attempt + 1,
                wait_ms = wait.as_millis(),
                "the embeddings endpoint answered transiently; retrying"
            );
            tokio::select! {
                () = req.cancel.cancelled() => return Err(EmbeddingProviderError::Cancelled),
                () = tokio::time::sleep(wait) => {}
            }
            backoff = backoff.saturating_mul(2);
        }
        // `0..=max_retries` always runs at least once, so this is unreachable
        // — stated rather than `unwrap`ped.
        Err(EmbeddingProviderError::Internal(
            "the retry loop ended without an attempt".to_owned(),
        ))
    }

    /// One request for one chunk, aligned to the inputs by the response's
    /// `index` field.
    async fn embed_chunk(
        &self,
        inputs: &[String],
        timeout: Duration,
    ) -> Result<Vec<Vec<f32>>, Refusal> {
        let body = EmbeddingsRequest {
            model: self.config.model.trim(),
            input: inputs
                .iter()
                .map(|text| {
                    if text.trim().is_empty() {
                        EMPTY_INPUT_PLACEHOLDER
                    } else {
                        text.as_str()
                    }
                })
                .collect(),
            dimensions: self
                .config
                .request_dimensions
                .then_some(self.config.dimension),
        };

        let mut request = self
            .http
            .post(self.endpoint.clone())
            .timeout(timeout)
            .json(&body);
        if let Some(key) = &self.config.api_key {
            request = request.bearer_auth(key.expose_secret());
        }

        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                // The caller's deadline, not the endpoint's refusal.
                Refusal::permanent(EmbeddingProviderError::Deadline)
            } else {
                Refusal::transient(EmbeddingProviderError::Unavailable {
                    reason: format!("{}: {error}", endpoint_name(&self.endpoint)),
                })
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            // The status is the diagnosis; the body is the vendor's and may
            // echo the request, so only its size is logged (telemetry
            // contract: no provider response bodies in logs).
            let body_bytes = response.bytes().await.map_or(0, |b| b.len());
            warn!(
                endpoint = %endpoint_name(&self.endpoint),
                status = status.as_u16(),
                body_bytes,
                "the embeddings endpoint refused the request"
            );
            return Err(classify_status(status));
        }

        let parsed: EmbeddingsResponse = response.json().await.map_err(|error| {
            Refusal::permanent(EmbeddingProviderError::Internal(format!(
                "unparseable embeddings response: {error}"
            )))
        })?;
        self.align(inputs.len(), parsed.data)
            .map_err(Refusal::permanent)
    }

    /// Place each returned vector at its declared index, and refuse a
    /// response that does not fill every slot exactly once.
    fn align(
        &self,
        expected: usize,
        data: Vec<EmbeddingDatum>,
    ) -> Result<Vec<Vec<f32>>, EmbeddingProviderError> {
        let width = self.config.dimension as usize;
        let mut slots: Vec<Option<Vec<f32>>> = vec![None; expected];
        for datum in data {
            let Some(slot) = slots.get_mut(datum.index) else {
                return Err(EmbeddingProviderError::Internal(format!(
                    "the endpoint returned index {} for a batch of {expected}",
                    datum.index
                )));
            };
            if slot.is_some() {
                return Err(EmbeddingProviderError::Internal(format!(
                    "the endpoint returned index {} twice",
                    datum.index
                )));
            }
            if datum.embedding.len() != width {
                debug!(
                    got = datum.embedding.len(),
                    want = width,
                    "the endpoint returned a vector of another width"
                );
                return Err(EmbeddingProviderError::SpaceMismatch);
            }
            if !datum.embedding.iter().all(|lane| lane.is_finite()) {
                return Err(EmbeddingProviderError::Internal(format!(
                    "vector {} carries a non-finite lane",
                    datum.index
                )));
            }
            *slot = Some(if self.config.normalize {
                normalize(datum.embedding)
            } else {
                datum.embedding
            });
        }
        slots
            .into_iter()
            .enumerate()
            .map(|(index, slot)| {
                slot.ok_or_else(|| {
                    EmbeddingProviderError::Internal(format!(
                        "the endpoint returned no vector for input {index} of {expected}"
                    ))
                })
            })
            .collect()
    }
}

/// A refusal, plus the one thing the port's error type cannot carry: whether
/// another identical request could answer differently.
///
/// The retry loop used to re-derive that from the error variant, which cannot
/// work. A refused credential and an overloaded endpoint are both `Unavailable`
/// to the gear, and rightly so -- either way the vector arm is down and the
/// caller can repair neither by changing the batch. The difference between
/// them is only visible here, where the status code still exists, so it is
/// decided here and carried rather than guessed at later.
struct Refusal {
    error: EmbeddingProviderError,
    retryable: bool,
}

impl Refusal {
    /// Another attempt could answer differently: a rate limit, a gateway
    /// error, a connection that did not open.
    const fn transient(error: EmbeddingProviderError) -> Self {
        Self {
            error,
            retryable: true,
        }
    }

    /// No repetition resolves this one (ADR-0004): a refused credential, a
    /// malformed answer, a width that does not match -- or a deadline and a
    /// cancellation, which are the caller's and not the endpoint's.
    const fn permanent(error: EmbeddingProviderError) -> Self {
        Self {
            error,
            retryable: false,
        }
    }
}

/// Map a refused status onto what the gear is told and whether to try again.
///
/// 401 and 403 stay `Unavailable`, because that is what they mean to the gear:
/// the provider cannot serve, and the vector arm is down until an operator
/// acts. They are not retried, because a credential the endpoint just refused
/// will be refused again -- and a rotated key retried on every chunk of every
/// batch is how a misconfiguration turns into provider-side rate limiting.
fn classify_status(status: reqwest::StatusCode) -> Refusal {
    match status.as_u16() {
        401 | 403 => Refusal::permanent(EmbeddingProviderError::Unavailable {
            reason: format!("the endpoint refused the credential (HTTP {status})"),
        }),
        // Listed rather than a `500..=599` range. 5xx is not one class: 500,
        // 502, 503 and 504 say the server is having a moment, while 501 says
        // it does not implement this and 505 says the two sides cannot agree
        // on a protocol version. Neither of those changes on the next attempt,
        // and ADR-0004 scopes the retry to "a rate limit, a gateway error, a
        // connection that did not open" -- which is this list and not the
        // range. A misconfigured endpoint answering 501 would otherwise spend
        // three requests and the whole backoff per chunk of every batch.
        408 | 429 | 500 | 502 | 503 | 504 => {
            Refusal::transient(EmbeddingProviderError::Unavailable {
                reason: format!("HTTP {status}"),
            })
        }
        // Still `Unavailable` to the gear -- the vector arm is down either way
        // -- but there is nothing a repeat can fix.
        501 | 505 => Refusal::permanent(EmbeddingProviderError::Unavailable {
            reason: format!("the endpoint cannot serve this request at all (HTTP {status})"),
        }),
        _ => Refusal::permanent(EmbeddingProviderError::Internal(format!(
            "the endpoint answered HTTP {status}"
        ))),
    }
}

/// Unit length, in f64 so a long vector's squared sum does not lose lanes on
/// the way. A zero vector stays zero rather than becoming NaN.
#[expect(
    clippy::cast_possible_truncation,
    reason = "narrowing to f32 is the destination: pgvector stores single precision"
)]
fn normalize(vector: Vec<f32>) -> Vec<f32> {
    let norm = vector
        .iter()
        .map(|lane| f64::from(*lane).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm == 0.0 {
        return vector;
    }
    vector
        .into_iter()
        .map(|lane| (f64::from(lane) / norm) as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(base_url: &str, model: &str) -> RemoteEmbeddingProvider {
        RemoteEmbeddingProvider::new(RemoteProviderConfig::new(base_url, model))
            .unwrap_or_else(|error| panic!("a valid configuration must build: {error}"))
    }

    #[test]
    fn the_endpoint_is_the_base_url_plus_embeddings() {
        assert_eq!(
            provider("https://api.openai.com/v1", "m")
                .endpoint()
                .as_str(),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            provider("https://api.openai.com/v1/", "m")
                .endpoint()
                .as_str(),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            provider("http://ollama:11434/v1?x=1", "m")
                .endpoint()
                .as_str(),
            "http://ollama:11434/v1/embeddings"
        );
    }

    #[test]
    fn a_trailing_slash_or_query_does_not_change_the_identity() {
        let one = provider("https://api.openai.com/v1", "text-embedding-3-small");
        let other = provider(
            "https://api.openai.com/v1/?trace=1",
            "text-embedding-3-small",
        );
        assert_eq!(
            one.embedding_space().identity_hash,
            other.embedding_space().identity_hash
        );
    }

    #[test]
    fn model_endpoint_and_width_are_each_part_of_the_identity() {
        let base = provider("https://api.openai.com/v1", "text-embedding-3-small");
        let other_model = provider("https://api.openai.com/v1", "text-embedding-3-large");
        let other_host = provider("https://eu.api.example.com/v1", "text-embedding-3-small");
        let mut narrow =
            RemoteProviderConfig::new("https://api.openai.com/v1", "text-embedding-3-small");
        narrow.dimension = 256;
        let narrow = RemoteEmbeddingProvider::new(narrow)
            .unwrap_or_else(|error| panic!("a valid configuration must build: {error}"));

        let hash = |p: &RemoteEmbeddingProvider| p.embedding_space().identity_hash.clone();
        assert_ne!(hash(&base), hash(&other_model));
        assert_ne!(hash(&base), hash(&other_host));
        assert_ne!(hash(&base), hash(&narrow));
    }

    /// A credential in the URL is refused, not carried.
    ///
    /// `https://user:key@host/v1` parses and the userinfo then rides on the
    /// endpoint this provider hands out and prints — into logs, error
    /// messages and bug reports. The key belongs in the environment variable
    /// the configuration already names.
    #[test]
    fn a_base_url_carrying_credentials_is_refused() {
        for url in [
            "https://user:secret@embeddings.test/v1",
            "https://tokenonly@embeddings.test/v1",
        ] {
            let error = RemoteEmbeddingProvider::new(RemoteProviderConfig::new(url, "m"))
                .err()
                .expect("a URL with userinfo is refused");
            assert!(
                matches!(error, RemoteConfigError::CredentialsInUrl),
                "{url}: {error}"
            );
        }
        // The same host without them is fine.
        let ok = provider("https://embeddings.test/v1", "m");
        assert_eq!(ok.endpoint().username(), "");
    }

    /// Two endpoints that differ only by transport are two endpoints.
    ///
    /// The identity is what decides whether stored vectors may be ranked
    /// against a new query. Without the scheme, a downgrade to `http` — or a
    /// routing change that keeps host and path — would silently reuse vectors
    /// from what is, as far as this gear can tell, a different provider.
    #[test]
    fn the_transport_is_part_of_the_identity() {
        let secure = provider("https://embeddings.test/v1", "m");
        let plain = provider("http://embeddings.test/v1", "m");
        assert_ne!(
            secure.embedding_space().identity_hash,
            plain.embedding_space().identity_hash,
            "http and https must not share an embedding space"
        );
    }

    #[test]
    fn a_relative_or_non_http_base_url_is_refused() {
        for bad in ["api.openai.com/v1", "ftp://x/v1", "", "https://"] {
            let error = RemoteEmbeddingProvider::new(RemoteProviderConfig::new(bad, "m"))
                .err()
                .unwrap_or_else(|| panic!("{bad:?} must be refused"));
            assert!(
                matches!(error, RemoteConfigError::BaseUrl(_)),
                "{bad:?}: {error}"
            );
        }
    }

    #[test]
    fn the_credential_does_not_render_in_debug() {
        let config = RemoteProviderConfig::new("https://api.openai.com/v1", "m")
            .with_api_key("sk-this-must-not-leak");
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("sk-this-must-not-leak"), "{rendered}");
    }

    #[test]
    fn normalization_yields_unit_length_and_leaves_zero_alone() {
        let unit = normalize(vec![3.0, 4.0]);
        let norm: f64 = unit
            .iter()
            .map(|x| f64::from(*x).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "{norm}");
        assert_eq!(normalize(vec![0.0, 0.0]), vec![0.0, 0.0]);
    }

    fn classify(code: u16) -> Refusal {
        classify_status(reqwest::StatusCode::from_u16(code).unwrap_or_default())
    }

    /// A bearer token on plain `http` is on the wire in clear. The scheme
    /// check allows `http` because a local endpoint is a real thing to run
    /// against, and `embed_chunk` attaches the credential whenever one is
    /// configured without looking at the scheme, so the combination is what
    /// has to be refused -- at configuration time, before the first request
    /// carries the token past a proxy.
    #[test]
    fn a_credential_is_not_sent_in_clear_to_another_host() {
        let build = |url: &str, key: Option<&str>| {
            let mut config = RemoteProviderConfig::new(url, "text-embedding-3-small");
            if let Some(key) = key {
                config = config.with_api_key(key);
            }
            RemoteEmbeddingProvider::new(config)
        };

        for url in [
            "http://embeddings.internal/v1",
            "http://10.0.0.7:8080/v1",
            "http://example.test/v1",
        ] {
            let refused = build(url, Some("sk-secret"))
                .err()
                .expect("a credential over plain http to another host must be refused");
            assert!(
                matches!(refused, RemoteConfigError::CredentialOverPlainHttp { .. }),
                "{url} must be refused for the scheme, got {refused}"
            );
            assert!(
                !refused.to_string().contains("sk-secret"),
                "and the refusal must not quote the credential: {refused}"
            );
        }

        // The same hosts are fine without a credential: there is nothing to
        // expose, and the plugin is not in the business of banning `http`.
        for url in ["http://embeddings.internal/v1", "http://10.0.0.7:8080/v1"] {
            assert!(
                build(url, None).is_ok(),
                "{url} carries no credential and stays allowed"
            );
        }

        // Loopback keeps its credential: the traffic never leaves the machine,
        // and refusing it would push the key somewhere worse.
        for url in [
            "http://127.0.0.1:11434/v1",
            "http://localhost:11434/v1",
            "http://[::1]:11434/v1",
        ] {
            assert!(
                build(url, Some("sk-secret")).is_ok(),
                "{url} is the local machine and stays allowed"
            );
        }

        // And https is the ordinary case.
        assert!(
            build("https://api.openai.com/v1", Some("sk-secret")).is_ok(),
            "https with a credential is the point of the feature"
        );
    }

    /// The model is checked as hard as its neighbours in the same constructor.
    /// It is not free text: it is trimmed into the embedding-space identity,
    /// which decides whether a stored vector is still valid, and it is written
    /// to the operator log at boot.
    #[test]
    fn a_model_that_is_not_a_model_name_is_refused() {
        // `.err()` rather than `expect_err`: the provider is not `Debug`, and
        // giving it one would print a configuration that carries a credential.
        let refused = |model: &str| {
            RemoteEmbeddingProvider::new(RemoteProviderConfig::new(
                "https://example.test/v1",
                model,
            ))
            .err()
            .expect("this model must be refused")
        };

        assert!(
            matches!(refused("  "), RemoteConfigError::Model),
            "a blank model keeps its own error"
        );
        assert!(
            matches!(
                refused(&"m".repeat(MAX_MODEL_LEN + 1)),
                RemoteConfigError::ModelTooLong { .. }
            ),
            "a model past the bound must be refused"
        );
        for model in ["text-embedding\n3-small", "text\u{0}embedding", "a\tb"] {
            assert!(
                matches!(refused(model), RemoteConfigError::ModelControlCharacters),
                "{model:?} must be refused"
            );
        }
    }

    /// And the shapes a real vendor uses still pass, including the bound
    /// exactly.
    #[test]
    fn a_real_model_name_is_accepted() {
        for model in [
            "text-embedding-3-small",
            "  text-embedding-3-small  ",
            "sentence-transformers/all-MiniLM-L6-v2",
            &"m".repeat(MAX_MODEL_LEN),
        ] {
            assert!(
                RemoteEmbeddingProvider::new(RemoteProviderConfig::new(
                    "https://example.test/v1",
                    model,
                ))
                .is_ok(),
                "{model:?} must be accepted"
            );
        }
    }

    #[test]
    fn statuses_split_into_unavailable_and_internal() {
        let unavailable = |code: u16| {
            matches!(
                classify(code).error,
                EmbeddingProviderError::Unavailable { .. }
            )
        };
        assert!(unavailable(401));
        assert!(unavailable(429));
        assert!(unavailable(503));
        assert!(!unavailable(400));
        assert!(!unavailable(404));
    }

    /// ADR-0004: "A refused credential, a malformed answer or a width that
    /// does not match are not retried, because no repetition resolves them."
    /// A refused credential reads as `Unavailable` to the gear -- the vector
    /// arm is down either way -- so the variant cannot carry this and the
    /// classification has to.
    #[test]
    fn a_refused_credential_is_unavailable_but_not_retried() {
        for code in [401, 403] {
            let refusal = classify(code);
            assert!(
                matches!(refusal.error, EmbeddingProviderError::Unavailable { .. }),
                "HTTP {code} is still an unavailable provider"
            );
            assert!(
                !refusal.retryable,
                "HTTP {code} must not be retried: the credential will be refused again, and \
                 retrying every chunk of every batch is how a rotated key becomes a rate limit"
            );
        }
    }

    /// 5xx is not one class. A server having a moment and a server that does
    /// not implement the endpoint answer in the same hundred, and only one of
    /// them is worth asking again.
    #[test]
    fn a_permanent_5xx_is_not_retried() {
        for code in [501, 505] {
            let refusal = classify(code);
            assert!(
                !refusal.retryable,
                "HTTP {code} describes a fixed capability, so repeating the request repeats \
                 the answer"
            );
            assert!(
                matches!(refusal.error, EmbeddingProviderError::Unavailable { .. }),
                "HTTP {code} still leaves the vector arm down, so it is still unavailable"
            );
        }
    }

    #[test]
    fn the_statuses_a_retry_can_fix_are_still_retried() {
        for code in [408, 429, 500, 502, 503, 504] {
            assert!(
                classify(code).retryable,
                "HTTP {code} is the endpoint saying `not now`, which is what retrying is for"
            );
        }
        for code in [400, 404, 422] {
            assert!(
                !classify(code).retryable,
                "HTTP {code} is about the request, and repeating it repeats the request"
            );
        }
    }
}
