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

/// Readiness reports what the session has shown, and `load` makes sure it has
/// shown something.
///
/// `health()` used to take the session lock and answer `Ok`, which says the
/// weights are resident and nothing more. A session can load and still be
/// unable to run -- a runtime built without the execution provider the graph
/// needs is the usual way -- and every check before this one passes: the
/// hashes match, the tokenizer parses, the session opens. The first evidence
/// then arrived when a producer's ingest failed, long after readiness had
/// called the deployment healthy.
///
/// Running inference on every probe would trade that for the opposite
/// problem: readiness is anonymous and polled on a schedule, so it would
/// become the busiest caller of the model. So the probe happens once, at
/// load, and every `embed` afterwards is evidence of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_reports_a_session_that_has_been_proven_to_run() {
    let Some(provider) = provider().await else {
        return;
    };
    // Loading at all now means one inference already succeeded.
    provider
        .health()
        .await
        .expect("a provider that loaded has run its probe");

    // And the evidence keeps coming from real work rather than from probes:
    // a successful embed leaves readiness healthy.
    embed(&provider, &["something to embed"]).await;
    provider
        .health()
        .await
        .expect("a session that just produced a vector is healthy");
}

/// A model whose width does not match the configuration is refused at load.
///
/// The probe checks the width it got, not only that something came back --
/// otherwise a provider configured for the wrong dimension would load
/// cleanly and produce vectors that no stored space can compare with.
#[tokio::test]
async fn a_session_whose_width_contradicts_the_configuration_does_not_load() {
    if std::env::var("ORT_DYLIB_PATH").is_err() {
        return;
    }
    let (Ok(model), Ok(tokenizer)) = (
        std::env::var("GRAPH_STORAGE_ONNX_MODEL"),
        std::env::var("GRAPH_STORAGE_ONNX_TOKENIZER"),
    ) else {
        return;
    };
    let mut config = OnnxProviderConfig::new(model, tokenizer);
    config.dimension += 1;
    let error = OnnxEmbeddingProvider::load(config)
        .await
        .err()
        .expect("a width the model cannot produce is a load failure");
    assert!(
        error.to_string().contains("configured for"),
        "the refusal says the width disagreed: {error}"
    );
}

/// The same contract on the runtime a deployment actually uses.
///
/// `embed` hands the synchronous inference to `block_in_place` when it finds
/// a multi-threaded runtime, and calls it directly otherwise — and every
/// other test here runs on the current-thread runtime, so the branch a gear
/// takes in production was the one nothing exercised. `block_in_place`
/// panics on a current-thread runtime and requires the guard it holds to
/// behave across the hand-off, so "it compiles" is not evidence about it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_onnx_provider_honours_the_contract_on_a_threaded_runtime() {
    let Some(provider) = provider().await else {
        return;
    };
    graph_storage_sdk::contract::assert_embedding_provider(&provider).await;

    // And two batches at once, which is the shape that makes the session
    // mutex and the blocking hand-off meet.
    let batch = |texts: &[&str]| EmbedRequest {
        inputs: texts.iter().map(|t| (*t).to_owned()).collect(),
        budget: RemainingBudget::starting_now(Duration::from_mins(1)),
        cancel: CancellationToken::new(),
    };
    let (first, second) = tokio::join!(
        provider.embed(batch(&["one sentence", "another"])),
        provider.embed(batch(&["a third"])),
    );
    assert_eq!(first.expect("the first batch embeds").vectors.len(), 2);
    assert_eq!(second.expect("the second batch embeds").vectors.len(), 1);
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
