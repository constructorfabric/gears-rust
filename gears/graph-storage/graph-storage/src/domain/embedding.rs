//! The Embedding Coordinator (`cpt-cf-graph-storage-component-embedding-coordinator`).
//!
//! One component owns the embedding lifecycle so model identity, batching and
//! dimension guarantees hold across ingest and query alike. It composes the
//! text a node embeds from, hashes that text canonically, calls the provider
//! once per batch, and verifies what comes back.
//!
//! It implements no model — providers are plugins — and it does not decide
//! which attributes are vectorizable: the `vector_search` trait a type
//! declares does, which is what finally makes that trait load-bearing rather
//! than decorative.

use std::sync::Arc;

use aws_lc_rs::digest::{SHA256, digest as sha256};
use graph_storage_sdk::models::{EmbeddingSpaceId, NodeSpec, RemainingBudget, TypeRecord};
use graph_storage_sdk::plugin_api::{
    EmbedRequest, EmbeddingProviderError, EmbeddingProviderV1, NodeEmbedding,
};
use tokio_util::sync::CancellationToken;

use crate::domain::error::DomainError;

/// Whether this deployment can serve vectors at all, and under which epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceState {
    /// Vectors written and read under this epoch.
    Active { epoch: i64 },
    /// Stored vectors belong to a space the active provider is not. Writing
    /// new vectors would mix two spaces in one column and searching would
    /// rank across them, so both are refused until re-embedding reconciles.
    Blocked,
}

/// Composes, hashes and embeds. Holds the one active provider.
pub struct EmbeddingCoordinator {
    provider: Arc<dyn EmbeddingProviderV1>,
    state: SpaceState,
    /// Ceiling on the bytes of composed text handed to the provider.
    input_max_bytes: usize,
}

impl EmbeddingCoordinator {
    #[must_use]
    pub fn new(
        provider: Arc<dyn EmbeddingProviderV1>,
        state: SpaceState,
        input_max_bytes: u32,
    ) -> Self {
        Self {
            provider,
            state,
            input_max_bytes: input_max_bytes as usize,
        }
    }

    #[must_use]
    pub fn state(&self) -> SpaceState {
        self.state
    }

    #[must_use]
    pub fn space(&self) -> &EmbeddingSpaceId {
        self.provider.embedding_space()
    }

    /// The epoch new vectors are stamped with, if any may be written.
    #[must_use]
    pub fn active_epoch(&self) -> Option<i64> {
        match self.state {
            SpaceState::Active { epoch } => Some(epoch),
            SpaceState::Blocked => None,
        }
    }

    /// Compose, hash and (unless `embed` is off) embed one batch of nodes.
    ///
    /// `paths_for` yields the `vector_search` trait of a node's type. Nodes
    /// whose type resolves no paths still embed their name: a node with a name
    /// and no vectorizable attributes is a legitimate, searchable thing.
    ///
    /// # Errors
    ///
    /// A provider failure fails the whole batch. It is never downgraded to an
    /// unembedded write, because a node stored without its vector is invisible
    /// to vector search while looking present on every other path.
    pub async fn plan<'a, F>(
        &self,
        nodes: &'a [NodeSpec],
        embed: bool,
        paths_for: F,
        budget: RemainingBudget,
        cancel: CancellationToken,
    ) -> Result<Vec<NodeEmbedding>, DomainError>
    where
        F: Fn(&'a NodeSpec) -> &'a [String],
    {
        let inputs: Vec<String> = nodes
            .iter()
            .map(|node| compose_input(node, paths_for(node), self.input_max_bytes))
            .collect();
        let hashes: Vec<String> = inputs.iter().map(|input| input_hash(input)).collect();

        // Blocked is not an ingest failure: writes continue, they simply
        // record no vector. Refusing the write would take the whole gear down
        // over an arm nobody may have asked for.
        if !embed || self.state == SpaceState::Blocked {
            return Ok(hashes.into_iter().map(NodeEmbedding::skipped).collect());
        }

        let vectors = self.embed(inputs, budget, cancel).await?;
        Ok(vectors
            .into_iter()
            .zip(hashes)
            .map(|(vector, hash)| NodeEmbedding::computed(vector, hash))
            .collect())
    }

    /// Embed one query text with the same provider ingest used.
    ///
    /// # Errors
    ///
    /// [`DomainError::VectorSearchUnavailable`] when no comparable space is in
    /// force; a provider failure otherwise.
    pub async fn embed_query(
        &self,
        query: &str,
        budget: RemainingBudget,
        cancel: CancellationToken,
    ) -> Result<Vec<f32>, DomainError> {
        if self.state == SpaceState::Blocked {
            return Err(DomainError::VectorSearchUnavailable {
                reason: "stored vectors belong to a different embedding space than the \
                         active provider; re-embedding is required before similarity \
                         search can rank them"
                    .to_owned(),
            });
        }
        let mut vectors = self
            .embed(vec![query.to_owned()], budget, cancel)
            .await?;
        // One input, one vector: `embed` has already refused a short answer.
        Ok(vectors.swap_remove(0))
    }

    async fn embed(
        &self,
        inputs: Vec<String>,
        budget: RemainingBudget,
        cancel: CancellationToken,
    ) -> Result<Vec<Vec<f32>>, DomainError> {
        let expected = inputs.len();
        if expected == 0 {
            return Ok(Vec::new());
        }
        let declared = self.provider.dimension() as usize;
        let response = self
            .provider
            .embed(EmbedRequest {
                inputs,
                budget,
                cancel,
            })
            .await
            .map_err(provider_failure)?;

        // The contract says a provider fails rather than returning a short
        // answer, and this is the gear's own check that it did: a silent
        // shortfall would assign every later vector to the wrong node.
        if response.vectors.len() != expected {
            return Err(DomainError::Unavailable {
                detail: format!(
                    "provider returned {} vectors for {expected} inputs",
                    response.vectors.len()
                ),
            });
        }
        if response.space != *self.provider.embedding_space() {
            return Err(DomainError::Unavailable {
                detail: "provider echoed an embedding space other than the one it declares"
                    .to_owned(),
            });
        }
        if let Some(wrong) = response.vectors.iter().find(|v| v.len() != declared) {
            return Err(DomainError::Unavailable {
                detail: format!(
                    "provider returned a {}-dimensional vector; this deployment's space is {declared}",
                    wrong.len()
                ),
            });
        }
        Ok(response.vectors)
    }
}

fn provider_failure(error: EmbeddingProviderError) -> DomainError {
    match error {
        EmbeddingProviderError::Cancelled => DomainError::Cancelled,
        EmbeddingProviderError::Deadline => DomainError::Deadline,
        // DESIGN is explicit: a provider failure maps to `unavailable` and
        // fails the batch. It is never downgraded to an unembedded write.
        other => DomainError::Unavailable {
            detail: other.to_string(),
        },
    }
}

/// The payload paths a node's type declares vectorizable.
///
/// Shared rather than inlined at each call site: the domain service and the
/// conformance suite each resolved this, and while they resolved it
/// separately the service's version was covered by nothing -- a service that
/// passed no paths at all would have left every test green.
///
/// A type the batch does not resolve yields no paths rather than an error:
/// validation has already refused unknown types by the time this runs, and a
/// node with a name and no vectorizable attributes is a legitimate thing to
/// embed.
#[must_use]
pub fn declared_paths<'a>(
    records: &'a std::collections::BTreeMap<String, TypeRecord>,
    node: &NodeSpec,
) -> &'a [String] {
    records
        .get(&node.type_id)
        .map_or(&[], |record| record.effective_traits.vector_search.as_slice())
}

/// What a store already holds for a node, in the only terms the decision
/// below needs. Deliberately not the row: the built-in store keeps a
/// `PgVector` and the fake a `Vec<f32>`, and neither difference matters here.
#[derive(Clone, Copy, Debug)]
pub struct StoredVector<'a> {
    pub has_vector: bool,
    /// Hash of the text the stored vector was made from.
    pub input_hash: Option<&'a str>,
}

/// The four vector states of `fr-embedding-pipeline`, decided once for every
/// store rather than re-derived in each.
///
/// The FR names these states; how a store spells them on its columns is its
/// own business, but *which* state applies must not be. A check that lives in
/// one implementation is a check the conformance suite cannot see.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorOutcome {
    /// Embedded and current: store this vector under this epoch.
    Store {
        vector: Vec<f32>,
        epoch: Option<i64>,
        input_hash: String,
    },
    /// Absent: no vector. The hash of the current input is still recorded, so
    /// a later embedding pass can tell what this node would embed from.
    Absent { input_hash: String },
    /// Preserved: embedding was skipped and the input is unchanged, so the
    /// stored vector still describes the node. Nothing moves.
    Preserve,
    /// Stale: embedding was skipped and the input changed. The vector stays
    /// (re-embedding will replace it) but stops being rankable, because it
    /// describes text the node no longer carries.
    Stale,
}

/// What the coordinator decided for one node, as a store receives it: the
/// per-node decision and the epoch new vectors are stamped with. One value
/// because neither means anything without the other.
#[derive(Clone, Copy, Debug)]
pub struct PlannedVector<'a> {
    pub decided: &'a NodeEmbedding,
    pub active_epoch: Option<i64>,
}

/// Decide the vector state of one upsert.
#[must_use]
pub fn decide_vector(
    current: Option<StoredVector<'_>>,
    planned: PlannedVector<'_>,
) -> VectorOutcome {
    let decided = planned.decided;
    if let Some(vector) = &decided.vector {
        return VectorOutcome::Store {
            vector: vector.clone(),
            epoch: planned.active_epoch,
            input_hash: decided.input_hash.clone(),
        };
    }
    match current {
        Some(stored) if stored.has_vector => {
            if stored.input_hash == Some(decided.input_hash.as_str()) {
                VectorOutcome::Preserve
            } else {
                VectorOutcome::Stale
            }
        }
        // No row, or a row that never had a vector: there is nothing to
        // preserve and nothing to make stale.
        _ => VectorOutcome::Absent {
            input_hash: decided.input_hash.clone(),
        },
    }
}

/// The canonical hash of an embedding input.
///
/// Stored beside the vector so a later ingest can tell whether the text the
/// vector was made from is still the text the node carries — the difference
/// between a *preserved* vector and a *stale* one.
#[must_use]
pub fn input_hash(input: &str) -> String {
    hex::encode(sha256(&SHA256, input.as_bytes()))
}

/// Compose what a node embeds from: its name, then the payload values at the
/// JSON pointers its type declares in the `vector_search` trait.
///
/// Bounded, because embedding cost and provider limits both scale with input
/// length. The cut respects UTF-8 boundaries: a provider handed a truncated
/// code point would either reject the batch or tokenize something the hash
/// does not describe.
#[must_use]
pub fn compose_input(node: &NodeSpec, paths: &[String], max_bytes: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = &node.name {
        parts.push(name.clone());
    }
    if let Some(payload) = &node.payload {
        for path in paths {
            // `/name` names the node's own name, already included above.
            if path == "/name" {
                continue;
            }
            let pointer = path.strip_prefix("/payload").unwrap_or(path);
            if let Some(value) = payload.pointer(pointer) {
                match value {
                    serde_json::Value::String(text) => parts.push(text.clone()),
                    other => parts.push(other.to_string()),
                }
            }
        }
    }
    let joined = parts.join(" ");
    if joined.len() <= max_bytes {
        return joined;
    }
    let mut cut = max_bytes;
    while cut > 0 && !joined.is_char_boundary(cut) {
        cut -= 1;
    }
    joined[..cut].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_storage_sdk::models::NodeSpec;

    fn node(name: &str, payload: serde_json::Value) -> NodeSpec {
        NodeSpec {
            node_key: "k".to_owned(),
            type_id: "t".to_owned(),
            name: Some(name.to_owned()),
            payload: Some(payload),
            expected_version: None,
        }
    }

    fn record_with(paths: &[&str]) -> TypeRecord {
        TypeRecord {
            type_id: "t".to_owned(),
            type_uuid: uuid::Uuid::nil(),
            kind: graph_storage_sdk::models::TypeKind::Node,
            is_abstract: false,
            schema: serde_json::json!({}),
            effective_traits: graph_storage_sdk::models::EffectiveTraits {
                vector_search: paths.iter().map(|p| (*p).to_owned()).collect(),
                ..graph_storage_sdk::models::EffectiveTraits::default()
            },
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn declared_paths_come_from_the_node_s_own_type() {
        let mut records = std::collections::BTreeMap::new();
        records.insert("t".to_owned(), record_with(&["/payload/summary"]));
        let node = node("n", serde_json::json!({}));
        assert_eq!(declared_paths(&records, &node), ["/payload/summary"]);
    }

    #[test]
    fn an_unresolved_type_declares_no_paths_rather_than_failing() {
        let records = std::collections::BTreeMap::new();
        let node = node("n", serde_json::json!({}));
        assert!(declared_paths(&records, &node).is_empty());
    }

    #[test]
    fn a_declared_path_reaches_the_embedding_input() {
        let node = node("Finding", serde_json::json!({ "summary": "leaked key" }));
        let without = compose_input(&node, &[], 1024);
        let with = compose_input(&node, &["/payload/summary".to_owned()], 1024);
        assert_eq!(without, "Finding");
        assert_eq!(with, "Finding leaked key");
    }

    /// The whole point of the trait: declaring a different path must change
    /// what the node embeds, and therefore its hash.
    #[test]
    fn declaring_a_different_path_changes_the_hash() {
        let node = node(
            "Finding",
            serde_json::json!({ "summary": "leaked key", "rule": "SEC-014" }),
        );
        let one = input_hash(&compose_input(&node, &["/payload/summary".to_owned()], 1024));
        let other = input_hash(&compose_input(&node, &["/payload/rule".to_owned()], 1024));
        assert_ne!(one, other);
    }

    #[test]
    fn a_path_that_resolves_to_nothing_is_skipped_not_rendered_as_null() {
        let node = node("Finding", serde_json::json!({ "summary": "x" }));
        assert_eq!(
            compose_input(&node, &["/payload/absent".to_owned()], 1024),
            "Finding"
        );
    }

    #[test]
    fn a_non_string_value_is_rendered_rather_than_dropped() {
        let node = node("Finding", serde_json::json!({ "severity": 9 }));
        assert_eq!(
            compose_input(&node, &["/payload/severity".to_owned()], 1024),
            "Finding 9"
        );
    }

    #[test]
    fn the_bound_cuts_on_a_character_boundary() {
        // U+00E9, two bytes in UTF-8, so an odd ceiling lands mid-character
        // and a naive slice would panic rather than truncate.
        let wide = '\u{e9}';
        let node = node(
            wide.to_string().repeat(10).as_str(),
            serde_json::json!({}),
        );
        let cut = compose_input(&node, &[], 5);
        assert_eq!(cut.len(), 4, "cut {cut:?} did not fall back to a boundary");
        assert!(cut.chars().all(|c| c == wide));
    }

    fn planned(decided: &NodeEmbedding) -> PlannedVector<'_> {
        PlannedVector {
            decided,
            active_epoch: Some(7),
        }
    }

    fn stored(has_vector: bool, hash: Option<&str>) -> StoredVector<'_> {
        StoredVector {
            has_vector,
            input_hash: hash,
        }
    }

    #[test]
    fn an_embedded_node_is_current_under_the_active_epoch() {
        let decided = NodeEmbedding::computed(vec![1.0], "h".to_owned());
        assert_eq!(
            decide_vector(None, planned(&decided)),
            VectorOutcome::Store {
                vector: vec![1.0],
                epoch: Some(7),
                input_hash: "h".to_owned(),
            }
        );
    }

    #[test]
    fn a_skipped_new_node_has_no_vector_but_records_its_input() {
        let decided = NodeEmbedding::skipped("h".to_owned());
        assert_eq!(
            decide_vector(None, planned(&decided)),
            VectorOutcome::Absent {
                input_hash: "h".to_owned()
            }
        );
    }

    /// The reason `embed = false` exists: a metadata-only re-sync must not
    /// cost a re-embedding pass, and must not empty the vector arm either.
    #[test]
    fn a_skipped_node_whose_input_is_unchanged_keeps_its_vector() {
        let decided = NodeEmbedding::skipped("h".to_owned());
        assert_eq!(
            decide_vector(Some(stored(true, Some("h"))), planned(&decided)),
            VectorOutcome::Preserve
        );
    }

    /// "A stored vector can never rank content that is no longer stored."
    #[test]
    fn a_skipped_node_whose_input_changed_goes_stale() {
        let decided = NodeEmbedding::skipped("new".to_owned());
        assert_eq!(
            decide_vector(Some(stored(true, Some("old"))), planned(&decided)),
            VectorOutcome::Stale
        );
    }

    #[test]
    fn a_row_that_never_had_a_vector_has_nothing_to_preserve() {
        let decided = NodeEmbedding::skipped("h".to_owned());
        assert_eq!(
            decide_vector(Some(stored(false, Some("h"))), planned(&decided)),
            VectorOutcome::Absent {
                input_hash: "h".to_owned()
            }
        );
    }

    #[test]
    fn the_hash_follows_the_text_and_nothing_else() {
        assert_eq!(input_hash("same"), input_hash("same"));
        assert_ne!(input_hash("same"), input_hash("other"));
    }
}
