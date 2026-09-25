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

/// How far two embeddings of the same text may diverge, as `1 - cosine`.
///
/// Well above the drift of nondeterministic floating-point reduction on
/// unit vectors, which sits around `1e-6`, and well below what separates two
/// different texts: at `1e-4` a provider that answers a different direction
/// the second time still fails.
const DETERMINISM_TOLERANCE: f32 = 1e-4;

/// What is wrong, if anything, with two embeddings of the same batch.
fn determinism_violation(first: &[Vec<f32>], second: &[Vec<f32>]) -> Option<String> {
    if first.len() != second.len() {
        return Some(format!(
            "the same batch answered {} vectors and then {}",
            first.len(),
            second.len()
        ));
    }
    for (index, (a, b)) in first.iter().zip(second).enumerate() {
        if a.len() != b.len() {
            return Some(format!("vector {index} changed width between calls"));
        }
        let similarity = cosine(a, b);
        if similarity < 1.0 - DETERMINISM_TOLERANCE {
            return Some(format!(
                "vector {index} drifted to cosine {similarity} of itself between two calls \
                 with the same input; the same text must embed to the same direction"
            ));
        }
    }
    None
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let denominator = norm(a) * norm(b);
    if denominator == 0.0 {
        // Two zero vectors point nowhere; equal is the only honest reading.
        return if a == b { 1.0 } else { 0.0 };
    }
    dot / denominator
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

    // The same text must embed to the same *direction*: ingest and query
    // embed at different times, and a provider whose second answer points
    // elsewhere ranks a document below its own text. Bit-for-bit equality is
    // more than that asks, and more than ADR-0004 asks -- it wants
    // determinism of the fake, for CI's sake, not of every provider. GPU
    // inference and multi-threaded BLAS reduce in a nondeterministic order
    // and drift in the last few bits, which moves no document relative to
    // its own text; an exact comparison would have refused every such
    // provider on a property the gear never relies on. Nothing downstream
    // compares vectors exactly either: re-embedding is decided by a hash of
    // the input text, not of the vector.
    if let Some(problem) = determinism_violation(&first.vectors, &second.vectors) {
        panic!("{problem}");
    }
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

/// What every provider can be held to: a call that is already over is
/// refused before any work.
///
/// Deliberately not "cancellation is honoured mid-batch", which is not a
/// clause this contract can make universal -- providers do not share a unit of
/// work. The ONNX one turns a whole batch into a single inference and can
/// only check around it; the remote one sends chunks and checks between them;
/// the in-memory one hashes item by item and checks between those. Each
/// boundary is asserted where it exists, in that provider's own tests, because
/// only there is it known where the boundary is.
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

#[cfg(test)]
mod tests {
    use super::determinism_violation;

    fn unit(v: &[f32]) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / norm).collect()
    }

    /// The drift nondeterministic reduction produces -- last-bits noise on a
    /// unit vector -- is not a violation. An exact comparison refused it.
    #[test]
    fn last_bits_drift_is_the_same_embedding() {
        let first = vec![unit(&[0.3, 0.5, 0.8, 0.1])];
        let second: Vec<Vec<f32>> = first
            .iter()
            .map(|v| v.iter().map(|x| x + 1e-6).collect())
            .collect();
        assert_ne!(first, second, "the fixture must actually differ");
        assert_eq!(determinism_violation(&first, &second), None);
    }

    /// A provider whose second answer points somewhere else still fails:
    /// that is the property the clause exists for.
    #[test]
    fn a_different_direction_is_not_the_same_embedding() {
        let first = vec![unit(&[1.0, 0.0, 0.0, 0.0])];
        let second = vec![unit(&[0.9, 0.3, 0.0, 0.0])];
        let problem =
            determinism_violation(&first, &second).expect("a vector that moved is refused");
        assert!(problem.contains("drifted"), "{problem}");
    }

    #[test]
    fn a_changed_shape_is_refused() {
        let one = vec![unit(&[1.0, 2.0])];
        assert!(
            determinism_violation(&one, &[]).is_some(),
            "a vector went missing"
        );
        assert!(
            determinism_violation(&one, &[unit(&[1.0, 2.0, 3.0])]).is_some(),
            "a vector changed width"
        );
    }
}
