//! Deterministic hash-based embedding provider.
//!
//! ADR-0005 requires one: *"a deterministic hash-based fake serves CI"*. It
//! exists so the provider contract, the four vector states and the
//! ingest-then-query-by-own-text acceptance test can be exercised with no
//! model artifacts, no `libonnxruntime` and no network — none of which the
//! standard test lane has.
//!
//! It carries no semantics. Two texts that mean the same thing get unrelated
//! vectors, so it can never stand in for a real model in a *quality*
//! assertion. What it does guarantee is the property those tests actually
//! depend on: identical input yields a bit-identical vector, in this process
//! and the next, on any target. That makes a document's own text retrieve it
//! at cosine distance zero.

use async_trait::async_trait;
use aws_lc_rs::digest::{Context, SHA256};
use graph_storage_sdk::models::EmbeddingSpaceId;
use graph_storage_sdk::plugin_api::{
    EmbedRequest, EmbedResponse, EmbeddingProviderError, EmbeddingProviderV1,
};

/// Bytes of hash material consumed per vector lane.
const BYTES_PER_LANE: usize = 4;

/// A provider whose vectors are a keyed hash of the input text.
pub struct FakeEmbeddingProvider {
    space: EmbeddingSpaceId,
}

impl FakeEmbeddingProvider {
    /// A provider for a deployment of the given vector width.
    ///
    /// The width is part of the declared identity, so a fake built for 384
    /// and a fake built for 768 are different spaces and readiness sees the
    /// difference — the same way two real models would.
    #[must_use]
    pub fn new(dimension: u32) -> Self {
        Self {
            space: EmbeddingSpaceId::new(
                "deterministic-hash-fake/v1",
                "none",
                serde_json::json!({ "kind": "raw-utf8" }),
                serde_json::json!({ "strategy": "sha256-counter" }),
                serde_json::json!({ "l2": true }),
                dimension,
            ),
        }
    }

    /// One vector for one input.
    ///
    /// Counter-mode hashing: block `i` is `SHA256(input || i)`, and each
    /// four-byte window of it becomes one lane. Everything before the final
    /// normalization is integer arithmetic, so the lanes are byte-exact
    /// wherever the code runs; the normalization that follows is IEEE-754
    /// division by a correctly-rounded square root, which is exact too.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "narrowing to f32 is the destination, not an accident: pgvector \
                  stores single precision, so a vector that did not round here \
                  would round on the way into the column instead"
    )]
    fn vector(&self, input: &str) -> Vec<f32> {
        let dimension = self.space.dimension as usize;
        let mut lanes: Vec<f32> = Vec::with_capacity(dimension);
        let mut block: u64 = 0;
        while lanes.len() < dimension {
            let mut hasher = Context::new(&SHA256);
            hasher.update(input.as_bytes());
            hasher.update(&block.to_be_bytes());
            let digest = hasher.finish();
            for lane in digest.as_ref().chunks_exact(BYTES_PER_LANE) {
                if lanes.len() == dimension {
                    break;
                }
                let raw = u32::from_be_bytes([lane[0], lane[1], lane[2], lane[3]]);
                // Map to [-1, 1) through f64 so the conversion is exact for
                // every u32 before it is narrowed.
                let unit = f64::from(raw) / f64::from(u32::MAX);
                lanes.push((unit.mul_add(2.0, -1.0)) as f32);
            }
            block += 1;
        }

        // Cosine distance is what the index serves, so hand back unit
        // vectors: a real sentence-embedding provider normalizes too, and a
        // test that passes here should pass there.
        let norm = lanes
            .iter()
            .map(|lane| f64::from(*lane) * f64::from(*lane))
            .sum::<f64>()
            .sqrt();
        if norm == 0.0 {
            // Unreachable for any real digest, but a zero vector has no
            // direction and would sort arbitrarily. Point it somewhere.
            let mut fallback = vec![0.0_f32; dimension];
            if let Some(first) = fallback.first_mut() {
                *first = 1.0;
            }
            return fallback;
        }
        for lane in &mut lanes {
            *lane = (f64::from(*lane) / norm) as f32;
        }
        lanes
    }
}

#[async_trait]
impl EmbeddingProviderV1 for FakeEmbeddingProvider {
    fn embedding_space(&self) -> &EmbeddingSpaceId {
        &self.space
    }

    fn dimension(&self) -> u32 {
        self.space.dimension
    }

    async fn embed(&self, req: EmbedRequest) -> Result<EmbedResponse, EmbeddingProviderError> {
        // Checked before the work, not after: a provider that answers a
        // cancelled call has spent the caller's budget for nothing.
        if req.cancel.is_cancelled() {
            return Err(EmbeddingProviderError::Cancelled);
        }
        if req.budget.is_exhausted() {
            return Err(EmbeddingProviderError::Deadline);
        }
        let vectors = req
            .inputs
            .iter()
            .map(|input| self.vector(input))
            .collect::<Vec<_>>();
        Ok(EmbedResponse {
            vectors,
            space: self.space.clone(),
        })
    }

    async fn health(&self) -> Result<(), EmbeddingProviderError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vectors_of(provider: &FakeEmbeddingProvider, texts: &[&str]) -> Vec<Vec<f32>> {
        texts.iter().map(|text| provider.vector(text)).collect()
    }

    #[test]
    fn the_same_text_gives_the_same_vector() {
        let provider = FakeEmbeddingProvider::new(384);
        assert_eq!(provider.vector("hello"), provider.vector("hello"));
    }

    /// Pinned literally, not recomputed: a recomputed expectation would move
    /// with the implementation and prove nothing about reproducibility across
    /// builds, which is the only property this provider is for.
    #[test]
    fn the_vector_is_pinned_across_builds() {
        let provider = FakeEmbeddingProvider::new(4);
        let vector = provider.vector("hello");
        let expected = [-0.023_096_137_f32, -0.270_348_46, 0.865_328_5, 0.421_408_24];
        for (lane, want) in vector.iter().zip(expected) {
            assert!(
                (lane - want).abs() < 1e-6,
                "lane drifted: got {vector:?}, want {expected:?}"
            );
        }
    }

    #[test]
    fn different_texts_give_different_vectors() {
        let provider = FakeEmbeddingProvider::new(384);
        let [one, other] = <[Vec<f32>; 2]>::try_from(vectors_of(&provider, &["hello", "world"]))
            .expect("two inputs, two vectors");
        assert_ne!(one, other);
    }

    #[test]
    fn vectors_are_unit_length() {
        let provider = FakeEmbeddingProvider::new(384);
        let norm = provider
            .vector("anything at all")
            .iter()
            .map(|lane| f64::from(*lane) * f64::from(*lane))
            .sum::<f64>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    /// A width that is not a multiple of the block size exercises the partial
    /// final block, where an off-by-one would silently shorten the vector.
    #[test]
    fn an_odd_width_is_filled_exactly() {
        for dimension in [1_u32, 7, 32, 33, 384] {
            let provider = FakeEmbeddingProvider::new(dimension);
            assert_eq!(provider.vector("x").len(), dimension as usize);
        }
    }

    #[test]
    fn the_declared_width_is_part_of_the_identity() {
        assert_ne!(
            FakeEmbeddingProvider::new(384).space.identity_hash,
            FakeEmbeddingProvider::new(768).space.identity_hash
        );
    }
}
