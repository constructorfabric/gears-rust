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
    #[error("model must not be empty")]
    Model,
    #[error("dimension must be positive")]
    Dimension,
    #[error("batch_size must be positive")]
    BatchSize,
    #[error("the HTTP client could not be built: {0}")]
    Client(String),
}

/// What an endpoint that cannot embed an empty string is asked to embed
/// instead. The coordinator composes a node's input from its name and
/// declared payload paths, so an empty input is a node with neither — rare,
/// but the contract requires it to get a vector aligned with its position.
const EMPTY_INPUT_PLACEHOLDER: &str = "(empty)";

/// How many bytes of an error body are worth carrying into a log line.
const ERROR_BODY_LIMIT: usize = 512;

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
        if config.model.trim().is_empty() {
            return Err(RemoteConfigError::Model);
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
    let host = endpoint.host_str().unwrap_or("unknown-host");
    match endpoint.port() {
        Some(port) => format!("{host}:{port}{}", endpoint.path()),
        None => format!("{host}{}", endpoint.path()),
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
            let call = self.embed_chunk(chunk, remaining.min(self.config.timeout));
            let batch = tokio::select! {
                () = req.cancel.cancelled() => return Err(EmbeddingProviderError::Cancelled),
                result = call => result?,
            };
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
    }
}

impl RemoteEmbeddingProvider {
    /// One request for one chunk, aligned to the inputs by the response's
    /// `index` field.
    async fn embed_chunk(
        &self,
        inputs: &[String],
        timeout: Duration,
    ) -> Result<Vec<Vec<f32>>, EmbeddingProviderError> {
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
                EmbeddingProviderError::Deadline
            } else {
                EmbeddingProviderError::Unavailable {
                    reason: format!("{}: {error}", endpoint_name(&self.endpoint)),
                }
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            let excerpt = response
                .text()
                .await
                .map(|text| text.chars().take(ERROR_BODY_LIMIT).collect::<String>())
                .unwrap_or_default();
            warn!(
                endpoint = %endpoint_name(&self.endpoint),
                status = status.as_u16(),
                body = %excerpt,
                "the embeddings endpoint refused the request"
            );
            return Err(classify_status(status));
        }

        let parsed: EmbeddingsResponse = response.json().await.map_err(|error| {
            EmbeddingProviderError::Internal(format!("unparseable embeddings response: {error}"))
        })?;
        self.align(inputs.len(), parsed.data)
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

/// A credential problem and a capacity problem are both "not now, and not
/// because of the input": the caller cannot repair either by changing the
/// batch, and neither says anything about the space.
fn classify_status(status: reqwest::StatusCode) -> EmbeddingProviderError {
    match status.as_u16() {
        401 | 403 => EmbeddingProviderError::Unavailable {
            reason: format!("the endpoint refused the credential (HTTP {status})"),
        },
        408 | 429 | 500..=599 => EmbeddingProviderError::Unavailable {
            reason: format!("HTTP {status}"),
        },
        _ => EmbeddingProviderError::Internal(format!("the endpoint answered HTTP {status}")),
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

    #[test]
    fn statuses_split_into_unavailable_and_internal() {
        let unavailable = |code: u16| {
            matches!(
                classify_status(reqwest::StatusCode::from_u16(code).unwrap_or_default()),
                EmbeddingProviderError::Unavailable { .. }
            )
        };
        assert!(unavailable(401));
        assert!(unavailable(429));
        assert!(unavailable(503));
        assert!(!unavailable(400));
        assert!(!unavailable(404));
    }
}
