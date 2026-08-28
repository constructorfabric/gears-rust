#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The ONNX provider against the published contract, and against the one
//! property no deterministic fake can stand in for.
//!
//! This lane needs three things the standard test environment does not have:
//! the ONNX Runtime shared library (`ORT_DYLIB_PATH`), a model, and its
//! tokenizer. It skips with a named reason when they are absent rather than
//! failing, the way the gear's `PostgreSQL` 19 lane does — and fails loudly
//! when `GRAPH_STORAGE_ONNX_REQUIRED` says the environment should have them.
//!
//! ```sh
//! ORT_DYLIB_PATH=/path/to/libonnxruntime.so \
//! GRAPH_STORAGE_ONNX_MODEL=/path/to/model.onnx \
//! GRAPH_STORAGE_ONNX_TOKENIZER=/path/to/tokenizer.json \
//!   cargo test -p cf-gears-graph-storage-onnx-embedding-plugin
//! ```

use std::time::Duration;

use graph_storage_sdk::models::RemainingBudget;
use graph_storage_sdk::plugin_api::{EmbedRequest, EmbeddingProviderV1};
use onnx_embedding_plugin::{OnnxEmbeddingProvider, OnnxProviderConfig};
use tokio_util::sync::CancellationToken;

/// Load the provider, or explain why this lane is not running.
async fn provider() -> Option<OnnxEmbeddingProvider> {
    let required = std::env::var("GRAPH_STORAGE_ONNX_REQUIRED").is_ok();
    let missing = |what: &str| {
        assert!(
            !required,
            "GRAPH_STORAGE_ONNX_REQUIRED is set but {what} is not available"
        );
        eprintln!("skipping the ONNX lane: {what} is not set");
        None::<OnnxEmbeddingProvider>
    };

    if std::env::var("ORT_DYLIB_PATH").is_err() {
        return missing("ORT_DYLIB_PATH");
    }
    let Ok(model) = std::env::var("GRAPH_STORAGE_ONNX_MODEL") else {
        return missing("GRAPH_STORAGE_ONNX_MODEL");
    };
    let Ok(tokenizer) = std::env::var("GRAPH_STORAGE_ONNX_TOKENIZER") else {
        return missing("GRAPH_STORAGE_ONNX_TOKENIZER");
    };

    match OnnxEmbeddingProvider::load(OnnxProviderConfig::new(model, tokenizer)).await {
        Ok(provider) => Some(provider),
        Err(error) => {
            assert!(
                !required,
                "GRAPH_STORAGE_ONNX_REQUIRED is set but the provider did not load: {error}"
            );
            eprintln!("skipping the ONNX lane: {error}");
            None
        }
    }
}

/// The same provider, with one knob turned.
async fn provider_with(
    adjust: impl FnOnce(&mut OnnxProviderConfig),
) -> Option<OnnxEmbeddingProvider> {
    let model = std::env::var("GRAPH_STORAGE_ONNX_MODEL").ok()?;
    let tokenizer = std::env::var("GRAPH_STORAGE_ONNX_TOKENIZER").ok()?;
    let mut config = OnnxProviderConfig::new(model, tokenizer);
    adjust(&mut config);
    OnnxEmbeddingProvider::load(config).await.ok()
}

async fn embed(provider: &OnnxEmbeddingProvider, texts: &[&str]) -> Vec<Vec<f32>> {
    provider
        .embed(EmbedRequest {
            inputs: texts.iter().map(|t| (*t).to_owned()).collect(),
            budget: RemainingBudget::starting_now(Duration::from_mins(1)),
            cancel: CancellationToken::new(),
        })
        .await
        .expect("the model embeds")
        .vectors
}

fn cosine(one: &[f32], other: &[f32]) -> f64 {
    one.iter()
        .zip(other)
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum()
}

#[tokio::test]
async fn the_onnx_provider_honours_the_contract() {
    let Some(provider) = provider().await else {
        return;
    };
    graph_storage_sdk::contract::assert_embedding_provider(&provider).await;
}

/// The one thing a deterministic fake cannot demonstrate: that the vectors
/// mean something.
///
/// The thresholds are not decoration. Ordering alone (`related > unrelated`)
/// passes even when mean pooling ignores the attention mask, which is a real
/// bug this crate had: measured against the `MiniLM` artifacts, masked pooling
/// scores 0.72 against 0.02, and unmasked pooling 0.85 against **0.52** --
/// the padding dominates the average, so everything resembles everything and
/// the ordering survives by a hair. A ceiling on the unrelated pair is what
/// tells the two apart.
#[tokio::test]
async fn related_sentences_sit_closer_than_unrelated_ones() {
    let Some(provider) = provider().await else {
        return;
    };
    let vectors = embed(
        &provider,
        &[
            "A hardcoded password was committed to the deployment script.",
            "Someone checked a plaintext credential into the deploy config.",
            "The kitchen renovation is scheduled for next spring.",
        ],
    )
    .await;

    let related = cosine(&vectors[0], &vectors[1]);
    let unrelated = cosine(&vectors[0], &vectors[2]);
    assert!(
        related > 0.5,
        "paraphrases should be plainly similar, scored {related}"
    );
    assert!(
        unrelated < 0.3,
        "unrelated sentences scored {unrelated}; a high floor here means the \
         pooling is averaging in padding rather than tokens"
    );
}

/// Ingest and query embed at different moments; a vector that moved between
/// them would rank a document below its own text.
#[tokio::test]
async fn one_text_embeds_to_one_vector() {
    let Some(provider) = provider().await else {
        return;
    };
    let text = "Hardcoded credential in deploy script";
    let first = embed(&provider, &[text]).await;
    let second = embed(&provider, &[text]).await;
    assert_eq!(first, second);
}

/// A text's vector must not depend on how much padding follows it.
///
/// Two providers over one model, differing only in their token ceiling, give
/// the same short text very different amounts of padding -- ten positions
/// against a hundred and twenty. Masked pooling ignores both and agrees;
/// pooling that does not, disagrees by more than any threshold would forgive.
/// A same-batch comparison cannot show this: the `MiniLM` tokenizer pads to a
/// fixed width, so a text alone and a text beside a longer one are padded
/// identically and the assertion holds however the mask is treated.
#[tokio::test]
async fn a_vector_does_not_depend_on_how_much_padding_follows_it() {
    let Some(provider) = provider().await else {
        return;
    };
    let Some(tight) = provider_with(|config| config.max_tokens = 16).await else {
        return;
    };

    let text = "A short sentence.";
    let roomy = embed(&provider, &[text]).await;
    let cramped = embed(&tight, &[text]).await;

    let agreement = cosine(&roomy[0], &cramped[0]);
    assert!(
        (agreement - 1.0).abs() < 1e-4,
        "the same text embedded differently under a different amount of \
         padding: cosine {agreement}"
    );
}
