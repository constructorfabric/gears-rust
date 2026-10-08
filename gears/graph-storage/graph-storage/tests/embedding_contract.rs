#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Every embedding provider this repository ships, against the one published
//! contract (`graph_storage_sdk::contract`).
//!
//! ADR-0005's Confirmation asks for exactly this. The in-process ONNX default
//! lives in its own crate and runs the same assertions there, where its model
//! artifacts are available; what runs here is the provider that needs none.

use graph_storage::infra::embedding::fake::FakeEmbeddingProvider;

#[tokio::test]
async fn the_deterministic_fake_honours_the_provider_contract() {
    graph_storage_sdk::contract::assert_embedding_provider(&FakeEmbeddingProvider::new(384)).await;
}

/// The fake is the only provider whose vectors a test may predict, so this is
/// where the property the acceptance test leans on is pinned: a document's own
/// text embeds to its own vector, hence cosine distance zero.
#[tokio::test]
async fn a_text_embeds_to_itself() {
    use graph_storage_sdk::models::RemainingBudget;
    use graph_storage_sdk::plugin_api::{EmbedRequest, EmbeddingProviderV1};

    let provider = FakeEmbeddingProvider::new(384);
    let embed = |text: &str| {
        let text = text.to_owned();
        async {
            provider
                .embed(EmbedRequest {
                    inputs: vec![text],
                    budget: RemainingBudget::starting_now(std::time::Duration::from_secs(5)),
                    cancel: tokio_util::sync::CancellationToken::new(),
                })
                .await
                .expect("the fake embeds")
                .vectors
                .swap_remove(0)
        }
    };

    let stored = embed("Hardcoded credential in deploy script").await;
    let queried = embed("Hardcoded credential in deploy script").await;
    let cosine_distance = 1.0
        - stored
            .iter()
            .zip(&queried)
            .map(|(a, b)| f64::from(*a) * f64::from(*b))
            .sum::<f64>();
    assert!(
        cosine_distance.abs() < 1e-6,
        "a text must retrieve itself at distance zero, got {cosine_distance}"
    );
}
