//! In-process ONNX embedding provider for the graph-storage gear.
//!
//! ADR-0005 makes this the default: a `MiniLM`-class sentence-embedding model
//! run through ONNX Runtime, in the gear's own process, so a small deployment
//! needs no inference service to use vector search at all.
//!
//! # Artifacts are supplied, never fetched
//!
//! The model and tokenizer are read from paths the operator configures. The
//! crate downloads nothing. That is not caution about the network: the
//! embedding-space identity has to be *verifiable*, and the only identity a
//! downloader can offer is the name it asked for. Reading a file lets the
//! identity be the SHA-256 of the bytes actually loaded, so two deployments
//! claiming one space either agree on that hash or are visibly different.
//!
//! # The runtime is loaded, not linked
//!
//! `ort` is pinned with `load-dynamic`, so ONNX Runtime is resolved by
//! `dlopen` at first use through `ORT_DYLIB_PATH`. Building this crate needs
//! no runtime headers; running it needs the shared library. See
//! [`OnnxEmbeddingProvider::load`] for what happens when that path is wrong,
//! which is worse than an error.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use aws_lc_rs::digest::{SHA256, digest as sha256};
use graph_storage_sdk::models::EmbeddingSpaceId;
use graph_storage_sdk::plugin_api::{
    EmbedRequest, EmbedResponse, EmbeddingProviderError, EmbeddingProviderV1,
};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use thiserror::Error;
use tokenizers::Tokenizer;
use tokio::sync::Mutex;
use tracing::warn;

/// How the model turns a sequence of token vectors into one sentence vector.
///
/// Part of the embedding-space identity rather than a tuning knob: the same
/// weights pooled two ways produce vectors of the same width that must never
/// be compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pooling {
    /// Average over the tokens the attention mask keeps. What the
    /// `sentence-transformers` `MiniLM` models are trained with.
    #[default]
    Mean,
    /// The first token's vector.
    Cls,
}

impl Pooling {
    fn as_str(self) -> &'static str {
        match self {
            Self::Mean => "mean",
            Self::Cls => "cls",
        }
    }
}

/// What a deployment declares about its model.
#[derive(Clone, Debug)]
pub struct OnnxProviderConfig {
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
    /// Vector width the model emits. Checked against the model's own output
    /// on the first batch, not taken on trust.
    pub dimension: u32,
    pub pooling: Pooling,
    /// L2-normalize the pooled vector. On for cosine similarity, which is
    /// what the gear's index serves.
    pub normalize: bool,
    /// Longest token sequence handed to the model; longer inputs are
    /// truncated.
    pub max_tokens: usize,
    /// ONNX Runtime intra-op threads. `None` leaves the runtime's default.
    pub intra_op_threads: Option<usize>,
}

impl OnnxProviderConfig {
    /// The `MiniLM-L6-v2` defaults ADR-0005 describes: 384 dimensions, mean
    /// pooling, L2-normalized.
    #[must_use]
    pub fn new(model_path: impl Into<PathBuf>, tokenizer_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            tokenizer_path: tokenizer_path.into(),
            dimension: 384,
            pooling: Pooling::Mean,
            normalize: true,
            max_tokens: 256,
            intra_op_threads: None,
        }
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OnnxLoadError {
    #[error("cannot read {what} at {path}: {source}")]
    Artifact {
        what: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("tokenizer at {path} is not loadable: {reason}")]
    Tokenizer { path: PathBuf, reason: String },
    #[error("ONNX session could not be created: {0}")]
    Session(String),
    /// The one failure a caller cannot recover from in-process.
    #[error(
        "ONNX Runtime did not load within {seconds}s. `ort` 2.0.0-rc.12 hangs \
         instead of erroring on an unloadable library, so the thread that \
         tried is abandoned rather than killed: check ORT_DYLIB_PATH and \
         restart the process"
    )]
    RuntimeHung { seconds: u64 },
}

/// How long to wait for the runtime before deciding it has hung.
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// A `MiniLM`-class sentence-embedding model, in this process.
pub struct OnnxEmbeddingProvider {
    /// `ort`'s `Session::run` takes `&mut self`, so inference is serialized
    /// whatever the sharing. A fair mutex over one session is then the honest
    /// shape: extra sessions would each hold a resident copy of the weights
    /// and their own intra-op thread pool, which is the wrong trade for a
    /// component called once per ingest batch.
    session: Arc<Mutex<Session>>,
    tokenizer: Tokenizer,
    space: EmbeddingSpaceId,
    config: OnnxProviderConfig,
}

impl OnnxEmbeddingProvider {
    /// Load the artifacts and open a session.
    ///
    /// # The hang this guards against
    ///
    /// `ort` 2.0.0-rc.12 **hangs forever instead of erroring** when the
    /// library at `ORT_DYLIB_PATH` cannot be loaded — measured in this
    /// repository against a nonexistent path, where neither `Session::builder`
    /// nor `ort::init_from` returns in 45 seconds. No pre-flight validation
    /// exists; every entry point funnels through the same lazy init. Since the
    /// hang cannot be interrupted from inside, the blocked thread is
    /// **abandoned**: a raw `std::thread` rather than `spawn_blocking`,
    /// because Tokio joins blocking threads at shutdown and a wedged one would
    /// hang that too. A caller receiving [`OnnxLoadError::RuntimeHung`] has
    /// leaked one thread and must terminate the process rather than retry.
    ///
    /// See `gears/file-parser/file-parser/src/gear.rs` for the same mitigation
    /// and the measurements behind it.
    ///
    /// # Errors
    ///
    /// Unreadable artifacts, an unloadable tokenizer, a session that refuses
    /// to open, or the runtime hang above.
    pub async fn load(config: OnnxProviderConfig) -> Result<Self, OnnxLoadError> {
        let model_digest = file_digest("model", &config.model_path)?;
        let tokenizer_digest = file_digest("tokenizer", &config.tokenizer_path)?;

        let tokenizer = Tokenizer::from_file(&config.tokenizer_path).map_err(|error| {
            OnnxLoadError::Tokenizer {
                path: config.tokenizer_path.clone(),
                reason: error.to_string(),
            }
        })?;

        let session = open_session(&config).await?;

        // The identity is the bytes actually loaded plus how they are used.
        // A deployment that swaps the file under one configured name gets a
        // different identity and is caught at boot rather than in ranking.
        let space = EmbeddingSpaceId::new(
            artifact_name(&config.model_path, &model_digest),
            artifact_name(&config.tokenizer_path, &tokenizer_digest),
            serde_json::json!({
                "tokenizer": "file",
                "max_tokens": config.max_tokens,
                "truncation": "longest_first",
            }),
            serde_json::json!({ "strategy": config.pooling.as_str() }),
            serde_json::json!({ "l2": config.normalize }),
            config.dimension,
        );

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer,
            space,
            config,
        })
    }
}

fn artifact_name(path: &Path, digest: &str) -> String {
    let name = path.file_name().map_or_else(
        || "unnamed".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    format!("{name}@sha256:{digest}")
}

fn file_digest(what: &'static str, path: &Path) -> Result<String, OnnxLoadError> {
    let bytes = std::fs::read(path).map_err(|source| OnnxLoadError::Artifact {
        what,
        path: path.to_path_buf(),
        source,
    })?;
    Ok(hex::encode(sha256(&SHA256, &bytes)))
}

/// Open the session on an abandonable thread. See [`OnnxEmbeddingProvider::load`].
async fn open_session(config: &OnnxProviderConfig) -> Result<Session, OnnxLoadError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let model_path = config.model_path.clone();
    let threads = config.intra_op_threads;

    std::thread::Builder::new()
        .name("graph-storage-onnx-init".to_owned())
        .spawn(move || {
            let result = build_session(&model_path, threads);
            // The receiver is gone when the timeout already fired; that is the
            // abandoned case, and dropping the session here is correct.
            drop(tx.send(result));
        })
        .map_err(|error| OnnxLoadError::Session(error.to_string()))?;

    match tokio::time::timeout(LOAD_TIMEOUT, rx).await {
        Ok(Ok(result)) => result,
        // The sender was dropped without sending: the init thread panicked.
        Ok(Err(_)) => Err(OnnxLoadError::Session(
            "the ONNX init thread ended without a result".to_owned(),
        )),
        Err(_) => {
            warn!(
                path = %config.model_path.display(),
                "ONNX Runtime did not load in time; leaking the init thread deliberately"
            );
            Err(OnnxLoadError::RuntimeHung {
                seconds: LOAD_TIMEOUT.as_secs(),
            })
        }
    }
}

fn build_session(
    model_path: &Path,
    intra_op_threads: Option<usize>,
) -> Result<Session, OnnxLoadError> {
    let mut builder = Session::builder()
        .map_err(|error| OnnxLoadError::Session(error.to_string()))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|error| OnnxLoadError::Session(error.to_string()))?;
    if let Some(threads) = intra_op_threads {
        builder = builder
            .with_intra_threads(threads)
            .map_err(|error| OnnxLoadError::Session(error.to_string()))?;
    }
    builder
        .commit_from_file(model_path)
        .map_err(|error| OnnxLoadError::Session(error.to_string()))
}

#[async_trait]
impl EmbeddingProviderV1 for OnnxEmbeddingProvider {
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
        if req.inputs.is_empty() {
            return Ok(EmbedResponse {
                vectors: Vec::new(),
                space: self.space.clone(),
            });
        }

        let encoded = self.encode(&req.inputs)?;
        let mut session = self.session.lock().await;
        // Checked again after the queue: a batch that waited out its deadline
        // behind another one should not then spend CPU on it.
        if req.budget.is_exhausted() {
            return Err(EmbeddingProviderError::Deadline);
        }
        let vectors = self.run(&mut session, &encoded)?;

        Ok(EmbedResponse {
            vectors,
            space: self.space.clone(),
        })
    }

    async fn health(&self) -> Result<(), EmbeddingProviderError> {
        // A session that cannot be locked is one whose holder panicked; the
        // model is otherwise resident and has nothing to report.
        drop(self.session.lock().await);
        Ok(())
    }
}

/// One tokenized batch, padded to its own longest sequence.
struct Encoded {
    ids: Vec<i64>,
    mask: Vec<i64>,
    type_ids: Vec<i64>,
    rows: usize,
    columns: usize,
}

impl OnnxEmbeddingProvider {
    fn encode(&self, inputs: &[String]) -> Result<Encoded, EmbeddingProviderError> {
        let encodings = self
            .tokenizer
            .encode_batch(inputs.to_vec(), true)
            .map_err(|error| EmbeddingProviderError::Internal(error.to_string()))?;

        // The tokenizer's own configuration decides the sequence length, and
        // for the `MiniLM` artifacts this crate targets that is a fixed 128
        // whatever the text -- measured, not assumed. `max_tokens` is a
        // ceiling on top of it, so it shortens sequences and never lengthens
        // them. Either way most positions are padding, which is why the
        // attention mask below is load-bearing rather than tidy: pooling over
        // the padding as well makes every text resemble every other one.
        let columns = encodings
            .iter()
            .map(|e| e.get_ids().len().min(self.config.max_tokens))
            .max()
            .unwrap_or(1)
            .max(1);
        let rows = encodings.len();

        let mut ids = vec![0_i64; rows * columns];
        let mut mask = vec![0_i64; rows * columns];
        let type_ids = vec![0_i64; rows * columns];
        for (row, encoding) in encodings.iter().enumerate() {
            let take = encoding.get_ids().len().min(columns);
            for column in 0..take {
                ids[row * columns + column] = i64::from(encoding.get_ids()[column]);
                mask[row * columns + column] = i64::from(encoding.get_attention_mask()[column]);
            }
        }
        Ok(Encoded {
            ids,
            mask,
            type_ids,
            rows,
            columns,
        })
    }

    fn run(
        &self,
        session: &mut Session,
        encoded: &Encoded,
    ) -> Result<Vec<Vec<f32>>, EmbeddingProviderError> {
        let internal = |what: String| EmbeddingProviderError::Internal(what);
        let shape = [encoded.rows, encoded.columns];
        let tensor = |data: &[i64]| {
            Tensor::from_array((shape, data.to_vec().into_boxed_slice()))
                .map_err(|error| internal(error.to_string()))
        };

        let outputs = session
            .run(ort::inputs![
                "input_ids" => tensor(&encoded.ids)?,
                "attention_mask" => tensor(&encoded.mask)?,
                "token_type_ids" => tensor(&encoded.type_ids)?,
            ])
            .map_err(|error| EmbeddingProviderError::Unavailable {
                reason: error.to_string(),
            })?;

        // The first output is the token-level hidden state whatever the export
        // named it; `MiniLM` exports vary between `last_hidden_state` and
        // `output_0`, and a provider that insisted on one name would refuse
        // half the artifacts an operator might reasonably supply.
        let (_, first) = outputs
            .iter()
            .next()
            .ok_or_else(|| internal("the model produced no output".to_owned()))?;
        let (out_shape, values) = first
            .try_extract_tensor::<f32>()
            .map_err(|error| internal(error.to_string()))?;

        if out_shape.len() != 3 {
            return Err(internal(format!(
                "expected a [batch, tokens, hidden] output, got {out_shape:?}"
            )));
        }
        let hidden = out_shape
            .last()
            .copied()
            .and_then(|width| usize::try_from(width).ok())
            .ok_or_else(|| internal(format!("the model reported a shape of {out_shape:?}")))?;
        // The declared width against the width the model actually emits.
        // A configuration that names one and loads another would write
        // vectors nothing can rank, and the column would refuse them anyway.
        if hidden != self.config.dimension as usize {
            return Err(EmbeddingProviderError::SpaceMismatch);
        }

        Ok(self.pool(values, encoded, hidden))
    }

    fn pool(&self, values: &[f32], encoded: &Encoded, hidden: usize) -> Vec<Vec<f32>> {
        let mut out = Vec::with_capacity(encoded.rows);
        for row in 0..encoded.rows {
            let base = row * encoded.columns * hidden;
            let mut pooled = vec![0.0_f64; hidden];
            match self.config.pooling {
                Pooling::Cls => {
                    for (lane, slot) in pooled.iter_mut().enumerate() {
                        *slot = f64::from(values[base + lane]);
                    }
                }
                Pooling::Mean => {
                    // Masked mean: padding contributes nothing, or a batch's
                    // vectors would depend on the longest text beside them.
                    let mut kept = 0.0_f64;
                    for column in 0..encoded.columns {
                        if encoded.mask[row * encoded.columns + column] == 0 {
                            continue;
                        }
                        kept += 1.0;
                        let offset = base + column * hidden;
                        for (lane, slot) in pooled.iter_mut().enumerate() {
                            *slot += f64::from(values[offset + lane]);
                        }
                    }
                    if kept > 0.0 {
                        for slot in &mut pooled {
                            *slot /= kept;
                        }
                    }
                }
            }
            if self.config.normalize {
                let norm = pooled.iter().map(|x| x * x).sum::<f64>().sqrt();
                if norm > 0.0 {
                    for slot in &mut pooled {
                        *slot /= norm;
                    }
                }
            }
            out.push(narrow(&pooled));
        }
        out
    }
}

/// Narrowing to f32 is the destination, not an accident: pgvector stores
/// single precision, so a vector that did not round here would round on the
/// way into the column instead.
#[expect(
    clippy::cast_possible_truncation,
    reason = "see the function's own documentation"
)]
fn narrow(values: &[f64]) -> Vec<f32> {
    values.iter().map(|v| *v as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooling_is_part_of_the_identity() {
        // Two spaces that differ only in how they pool must not be confused:
        // same weights, same width, incomparable vectors.
        let one = EmbeddingSpaceId::new(
            "m@sha256:a",
            "t@sha256:b",
            serde_json::json!({}),
            serde_json::json!({ "strategy": Pooling::Mean.as_str() }),
            serde_json::json!({ "l2": true }),
            384,
        );
        let other = EmbeddingSpaceId::new(
            "m@sha256:a",
            "t@sha256:b",
            serde_json::json!({}),
            serde_json::json!({ "strategy": Pooling::Cls.as_str() }),
            serde_json::json!({ "l2": true }),
            384,
        );
        assert_ne!(one.identity_hash, other.identity_hash);
    }

    #[test]
    fn a_missing_artifact_is_named_in_the_error() {
        let error = file_digest("model", Path::new("/nonexistent/model.onnx"))
            .expect_err("a missing file cannot be digested");
        let rendered = error.to_string();
        assert!(rendered.contains("model"), "{rendered}");
        assert!(rendered.contains("/nonexistent/model.onnx"), "{rendered}");
    }

    #[test]
    fn the_artifact_name_carries_the_digest_rather_than_the_path() {
        // The path is a deployment detail; the bytes are the identity. Two
        // hosts with the same model in different directories must agree.
        assert_eq!(
            artifact_name(Path::new("/opt/models/model.onnx"), "abc"),
            artifact_name(Path::new("/srv/other/model.onnx"), "abc")
        );
    }
}
