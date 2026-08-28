//! Executable form of the plugin contracts, behind the `test-support` feature.
//!
//! DESIGN says the gear *publishes* the provider contract
//! (`cpt-cf-graph-storage-contract-embedding-provider`) and ADR-0005 requires
//! that "contract tests run all three plugins against the provider contract".
//! A prose contract cannot be run against anything, and a suite living in one
//! implementation's `tests/` directory cannot be reached by another crate — so
//! the assertions live here, beside the trait they constrain, and every
//! provider (the in-process ONNX default, a remote plugin, the deterministic
//! fake) proves itself against the same code.
//!
//! ```ignore
//! #[tokio::test]
//! async fn it_honours_the_provider_contract() {
//!     graph_storage_sdk::contract::assert_embedding_provider(&MyProvider::new()).await;
//! }
//! ```

#![allow(
    clippy::expect_used,
    reason = "this module is a test harness: a violated clause has to abort \
              the caller's test with the clause named, and there is no other \
              outcome for it to return"
)]

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::models::RemainingBudget;
use crate::plugin_api::{EmbedRequest, EmbeddingProviderError, EmbeddingProviderV1};

fn request(inputs: Vec<String>) -> EmbedRequest {
    EmbedRequest {
        inputs,
        budget: RemainingBudget::starting_now(Duration::from_secs(30)),
        cancel: CancellationToken::new(),
    }
}

/// Assert that `provider` honours [`EmbeddingProviderV1`].
///
/// Panics with a message naming the broken clause. Written as assertions
/// rather than a returned report because a provider that fails any of these
/// cannot be deployed at all: there is nothing to triage.
///
/// # Panics
///
/// Whenever the provider violates the contract.
pub async fn assert_embedding_provider<P: EmbeddingProviderV1 + ?Sized>(provider: &P) {
    assert_declaration(provider);
    assert_alignment_and_width(provider).await;
    assert_determinism(provider).await;
    assert_empty_batch(provider).await;
    assert_budget_and_cancellation(provider).await;

    provider
        .health()
        .await
        .expect("a provider that cannot answer `health` cannot be made ready");
}

/// What the provider says about itself before it is asked to do anything.
fn assert_declaration<P: EmbeddingProviderV1 + ?Sized>(provider: &P) {
    let space = provider.embedding_space();
    assert_eq!(
        space.dimension,
        provider.dimension(),
        "dimension() and embedding_space().dimension describe one space and must agree"
    );
    assert!(
        provider.dimension() > 0,
        "a zero-width space cannot rank anything"
    );
    assert!(
        !space.identity_hash.is_empty(),
        "the identity hash is what readiness compares against; an empty one \
         makes every space look alike"
    );
}

/// Two inputs are never enough: three, including an empty string, is what
/// catches a provider that drops a degenerate input instead of embedding it
/// and thereby shifts every vector after it onto the wrong node.
fn sample_inputs() -> Vec<String> {
    vec![
        "the first input".to_owned(),
        "a second, quite different input".to_owned(),
        String::new(),
    ]
}

async fn assert_alignment_and_width<P: EmbeddingProviderV1 + ?Sized>(provider: &P) {
    let dimension = provider.dimension() as usize;
    let inputs = sample_inputs();
    let response = provider
        .embed(request(inputs.clone()))
        .await
        .expect("a provider must embed a well-formed batch");

    assert_eq!(
        response.vectors.len(),
        inputs.len(),
        "vectors are aligned with inputs by index, so a short answer is a \
         silent mis-assignment of every vector after the gap"
    );
    for (index, vector) in response.vectors.iter().enumerate() {
        assert_eq!(
            vector.len(),
            dimension,
            "vector {index} is {} wide against a declared width of {dimension}",
            vector.len()
        );
        assert!(
            vector.iter().all(|lane| lane.is_finite()),
            "vector {index} carries a NaN or an infinity, which no distance \
             operator can order"
        );
    }
    assert_eq!(
        &response.space,
        provider.embedding_space(),
        "the echoed space must be the declared one, or a mismatch is only \
         discoverable at configuration time"
    );
}

async fn assert_determinism<P: EmbeddingProviderV1 + ?Sized>(provider: &P) {
    let inputs = sample_inputs();
    let first = provider
        .embed(request(inputs.clone()))
        .await
        .expect("a provider must embed a well-formed batch");
    let second = provider
        .embed(request(inputs))
        .await
        .expect("a provider must embed a well-formed batch");

    assert_eq!(
        first.vectors, second.vectors,
        "the same text must embed to the same vector: ingest and query embed \
         at different times, and a drifting provider ranks a document below \
         its own text"
    );
    assert_ne!(
        first.vectors.first(),
        first.vectors.get(1),
        "two unrelated inputs embedded identically; a provider that answers a \
         constant passes every other clause here"
    );
}

async fn assert_empty_batch<P: EmbeddingProviderV1 + ?Sized>(provider: &P) {
    let empty = provider
        .embed(request(Vec::new()))
        .await
        .expect("an empty batch is a no-op, not an error");
    assert!(
        empty.vectors.is_empty(),
        "an empty batch produced {} vectors",
        empty.vectors.len()
    );
}

async fn assert_budget_and_cancellation<P: EmbeddingProviderV1 + ?Sized>(provider: &P) {
    let exhausted = EmbedRequest {
        inputs: vec!["anything".to_owned()],
        budget: RemainingBudget::starting_now(Duration::ZERO),
        cancel: CancellationToken::new(),
    };
    assert!(
        matches!(
            provider.embed(exhausted).await,
            Err(EmbeddingProviderError::Deadline)
        ),
        "an exhausted budget must be refused as `Deadline`, not served late: \
         the caller's deadline is absolute and already spent"
    );

    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = EmbedRequest {
        inputs: vec!["anything".to_owned()],
        budget: RemainingBudget::starting_now(Duration::from_secs(30)),
        cancel,
    };
    assert!(
        matches!(
            provider.embed(cancelled).await,
            Err(EmbeddingProviderError::Cancelled)
        ),
        "a cancelled call must be refused as `Cancelled`"
    );
}
