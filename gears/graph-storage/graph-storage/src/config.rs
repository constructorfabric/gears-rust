//! Typed configuration for the graph-storage gear.
//!
//! Every bound below is one row of DESIGN § Capacity and Admission Contract:
//! a default plus a hard range, and a value outside the hard range is
//! rejected at startup rather than clamped — a deployment that asks for the
//! impossible should not boot into something else silently.

use serde::Deserialize;

/// Which embedding provider a deployment runs.
///
/// One per deployment, per the single-embedding-space constraint: the choice
/// is a deployment fact, not a per-request option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingProviderKind {
    /// The deterministic hash-based provider. Reproducible and free, and its
    /// ranking carries no meaning whatsoever -- for tests, and for a
    /// deployment that wants the write path exercised before a model exists.
    /// Never chosen implicitly: it has to be spelled out in configuration.
    Fake,
    /// ADR-0004's default: a `MiniLM`-class model in this process. Needs the
    /// `onnx` feature at compile time and artifact paths at run time.
    Onnx,
    /// ADR-0004's alternative: an `OpenAI`-compatible `/embeddings` endpoint.
    /// Needs the `remote` feature at compile time and an endpoint, a model and
    /// a credential at run time.
    Remote,
}

/// Which backend serves one-hop expansion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HopStrategy {
    /// One-statement SQL/PGQ `GRAPH_TABLE` hop (requires `PostgreSQL` 19+).
    #[default]
    Pgq,
    /// Two scoped queries; the universal fallback every deployment can serve.
    TwoQuery,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphStorageConfig {
    /// Traversal backend. SQL/PGQ falls back to `two_query` per request when
    /// the scope defeats it, always with a logged reason.
    pub traversal_hop: HopStrategy,

    /// Vector width of the deployment's single embedding space. Fixed at
    /// migration time; readiness verifies configured == column definition.
    pub embedding_dimension: u32,

    /// Ceiling on the composed text one node embeds from. Embedding cost and
    /// provider input limits both scale with length, and a node whose payload
    /// happens to carry a megabyte of prose should cost the same as any
    /// other.
    pub embedding_input_max_bytes: u32,

    /// Which provider computes this deployment's vectors. There is no default:
    /// a deployment names one of `fake | onnx | remote`, or the boot fails.
    /// Falling back to the fake would fill the graph with vectors that rank
    /// nothing meaningfully while every health signal stayed green — the
    /// quiet quality loss ADR-0004 is written to prevent.
    pub embedding_provider: Option<EmbeddingProviderKind>,
    /// Path to the ONNX model artifact. Required by the `onnx` provider; the
    /// gear reads it and never fetches it, so the embedding-space identity can
    /// be the hash of the bytes actually loaded (ADR-0005).
    pub embedding_model_path: Option<String>,
    /// Path to the matching tokenizer artifact.
    pub embedding_tokenizer_path: Option<String>,

    // --- the `remote` provider ----------------------------------------------
    /// API root of the `OpenAI`-compatible endpoint, e.g.
    /// `https://api.openai.com/v1`. Required by the `remote` provider.
    pub embedding_remote_base_url: Option<String>,
    /// Model name as the endpoint knows it. Required by the `remote` provider.
    pub embedding_remote_model: Option<String>,
    /// Name of the environment variable holding the bearer credential. The
    /// value never enters the configuration, so a config dump cannot leak it.
    /// Unset means the endpoint takes no credential.
    pub embedding_remote_api_key_env: Option<String>,
    /// Send the `dimensions` request field, so a Matryoshka model returns
    /// exactly `embedding_dimension` lanes. Off for a fixed-width model.
    pub embedding_remote_request_dimensions: bool,
    /// Inputs per request to the endpoint.
    pub embedding_remote_batch_size: u32,
    /// Per-request timeout, seconds. The caller's deadline shortens it.
    pub embedding_remote_timeout_secs: u64,

    // --- limits (graph-storage.limits.*) ----------------------------------
    pub ingest_max_nodes: u32,
    pub ingest_max_edges: u32,
    pub payload_max_bytes: u32,
    /// Ceiling on a producer-supplied `node_key` and on a node's `name`.
    ///
    /// Both are caller-controlled, both are stored in indexed columns, and
    /// both are echoed back by every surface that returns the node — so an
    /// oversized one is paid for on every later read of that row, by every
    /// consumer, not only by the request that wrote it.
    pub identifier_max_bytes: u32,
    /// Ceiling on a search query's text. The lexical arm parses it and the
    /// vector arm embeds it; neither is work a caller should be able to ask
    /// for in unbounded quantity.
    pub search_query_max_bytes: u32,
    /// Ceiling on one whole element -- payload, name, keys and discriminator
    /// together -- as the caller submits it.
    ///
    /// `payload_max_bytes` bounds the largest field of an item and this bounds
    /// the item, which is not the same number: an element also carries
    /// identifiers, a name and a type. It exists so that a count limit can be
    /// reasoned about as a size limit, which is what makes the combination
    /// checks below possible at all.
    pub item_max_bytes: u32,
    /// Ceiling on one whole ingest request, summed over every element in it.
    ///
    /// Per-item bounds do not bound a batch: fifty thousand items each just
    /// under the item ceiling is a request no per-item check refuses and no
    /// process survives. The count limits and this one bound it from two
    /// directions, and the smaller of the two wins.
    pub ingest_max_bytes: u64,
    /// Ceiling on one hydrated response, summed over the elements in it.
    ///
    /// Counts alone are not a memory bound: `traversal_max_nodes` elements of
    /// `item_max_bytes` each is gigabytes at the hard limits. Where a count
    /// ceiling multiplied by the item ceiling already fits inside this one --
    /// the projection page and the search arms -- that is checked at startup
    /// and nothing needs to be measured at run time. Traversal is the arm
    /// whose count ceiling does not fit, so it measures as it hydrates and
    /// reports the cut.
    pub response_max_bytes: u64,
    pub node_read_max_adjacency: u32,
    pub traversal_max_depth: u8,
    pub traversal_max_nodes: u32,
    pub traversal_max_frontier: u32,
    pub traversal_max_edges_scanned: u64,
    pub search_max_arm_limit: u32,
    pub projection_max_page: u32,
    /// Absolute deadline for interactive operations, seconds.
    pub deadline_interactive_secs: u64,
    /// Idempotency receipt retention, days.
    pub idempotency_retention_days: u32,

    /// Longest derivation chain a registered type may have, counted in
    /// segments (`base ~ family ~ producer` is 3). The platform GTS guideline
    /// recommends two derivations, and 3 keeps that posture by default; a
    /// deployment whose ontology mirrors a deeper domain hierarchy (a domain
    /// model with `managed_object ~ document ~ requirement` under the family)
    /// raises it. Nothing in the gear depends on the depth: chain walking,
    /// trait resolution, chain validation and pattern matching all work on
    /// any length, so this is a policy knob rather than a capability.
    pub ontology_max_chain_depth: u8,

    /// Live rows a synchronous type update may re-validate or rewrite.
    ///
    /// Only the paths that cannot be decided from the schemas read a row at
    /// all; a provably backward-compatible change touches none, whatever this
    /// says. The ceiling is what keeps a re-validating update inside one
    /// interactive request: above it the honest answer is an asynchronous
    /// migration with progress, which the gear does not have.
    pub type_update_max_rows: u32,
    /// Live rows a synchronous *migration* may rewrite.
    ///
    /// A separate bound from `type_update_max_rows`, because the two passes run
    /// at different rates and only one of them writes. Measured on a stand
    /// (2026-09-11): re-validation reads ~19 000 rows/s, a migration rewrites
    /// ~1 900 rows/s — it is one statement per changed row. Since `api-gateway`
    /// kills any synchronous request at 30 s whatever this gear is configured
    /// with, 100 000 rows is ~5 s of re-validation and ~52 s of migration: the
    /// shared bound would admit a migration that does all of its work and is
    /// then killed, rolling back. 25 000 is ~13 s at the measured rate, which
    /// leaves the margin a bigger payload or a busier server needs.
    pub type_migration_max_rows: u32,
    /// Rows per batch while re-validating a type.
    pub type_update_batch: u32,
    /// Offending node keys a refusal lists. Enough to see the pattern, not
    /// enough to make the refusal itself a data export.
    pub type_update_max_reported_rows: u32,
}

impl Default for GraphStorageConfig {
    fn default() -> Self {
        Self {
            traversal_hop: HopStrategy::default(),
            embedding_dimension: 384,
            embedding_input_max_bytes: 8 * 1024,
            embedding_provider: None,
            embedding_model_path: None,
            embedding_tokenizer_path: None,
            embedding_remote_base_url: None,
            embedding_remote_model: None,
            embedding_remote_api_key_env: None,
            embedding_remote_request_dimensions: true,
            embedding_remote_batch_size: 64,
            embedding_remote_timeout_secs: 60,
            ingest_max_nodes: 10_000,
            ingest_max_edges: 20_000,
            payload_max_bytes: 64 * 1024,
            identifier_max_bytes: 2 * 1024,
            search_query_max_bytes: 8 * 1024,
            item_max_bytes: 256 * 1024,
            ingest_max_bytes: 64 * 1024 * 1024,
            response_max_bytes: 64 * 1024 * 1024,
            node_read_max_adjacency: 100,
            traversal_max_depth: 5,
            traversal_max_nodes: 1_000,
            traversal_max_frontier: 10_000,
            traversal_max_edges_scanned: 100_000,
            search_max_arm_limit: 50,
            projection_max_page: 200,
            deadline_interactive_secs: 10,
            idempotency_retention_days: 7,
            ontology_max_chain_depth: 3,
            type_update_max_rows: 100_000,
            type_migration_max_rows: 25_000,
            type_update_batch: 2_000,
            type_update_max_reported_rows: 50,
        }
    }
}

/// One hard range violated => one line naming the key, the value and the
/// permitted range, so the boot failure is actionable without reading code.
macro_rules! check_range {
    ($errors:ident, $cfg:ident, $field:ident, $min:expr, $max:expr) => {
        #[allow(unused_comparisons)]
        if $cfg.$field < $min || $cfg.$field > $max {
            $errors.push(format!(
                concat!(
                    "graph-storage.limits.",
                    stringify!($field),
                    " = {} is outside the hard range {}..={}"
                ),
                $cfg.$field, $min, $max
            ));
        }
    };
}

/// A [`GraphStorageConfig`] whose ranges have been checked.
///
/// The only way to obtain one is [`GraphStorageConfig::validated`], so a
/// constructor that asks for this cannot be handed a config nobody checked.
/// That used to depend on every call site remembering to call `validate`
/// first, which held only because there was one production call site: a second
/// path -- another gear variant, a test helper promoted to production -- would
/// have run with out-of-range limits and byte budgets and said nothing, since
/// the ranges are refused at startup precisely so a deployment that asks for
/// the impossible does not boot into something else.
///
/// It is a newtype rather than a parse into a different shape because the
/// checked and unchecked values are the same value; what differs is whether
/// anything has looked at it.
#[derive(Clone, Debug)]
pub struct ValidatedConfig(GraphStorageConfig);

impl ValidatedConfig {
    /// The checked configuration.
    #[must_use]
    pub fn into_inner(self) -> GraphStorageConfig {
        self.0
    }

    /// Carry a configuration past the ranges, for tests only.
    ///
    /// Several cases exist to prove what happens *at* a limit, and the
    /// cheapest way to reach one is to set it low: a response budget of a
    /// kilobyte, or an interactive deadline of zero seconds. Those are not
    /// configurations a deployment may have -- which is the point of the
    /// ranges -- and the zero deadline has no legal equivalent at all, so
    /// rewriting the cases against the floors would not have been possible
    /// even at the price of multi-megabyte fixtures.
    ///
    /// This is `cfg`-gated to tests and the `test-support` feature, so no
    /// production path can reach it. The guarantee `ValidatedConfig` carries
    /// is that nothing *shipped* constructs a store or a service from a
    /// configuration nobody checked; it was never that the type is
    /// unconstructible.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn unchecked(config: GraphStorageConfig) -> Self {
        Self(config)
    }
}

impl std::ops::Deref for ValidatedConfig {
    type Target = GraphStorageConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl GraphStorageConfig {
    /// Check the hard ranges and carry the proof in the type.
    ///
    /// # Errors
    /// Every range violation at once, not the first -- an operator fixing a
    /// configuration file should see the whole list.
    pub fn validated(self) -> anyhow::Result<ValidatedConfig> {
        self.validate()?;
        Ok(ValidatedConfig(self))
    }

    /// Enforce the hard ranges of the Capacity and Admission Contract.
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut errors: Vec<String> = Vec::new();
        self.check_embedding_ranges(&mut errors);
        self.check_write_ranges(&mut errors);
        self.check_graph_ranges(&mut errors);
        self.check_limit_combinations(&mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "invalid graph-storage configuration:\n  {}",
                errors.join("\n  ")
            )
        }
    }

    /// The provider's own bounds.
    fn check_embedding_ranges(&self, errors: &mut Vec<String>) {
        check_range!(errors, self, embedding_dimension, 1u32, 4_096u32);
        check_range!(errors, self, embedding_input_max_bytes, 64u32, 262_144u32);
        check_range!(errors, self, embedding_remote_batch_size, 1u32, 2_048u32);
        check_range!(errors, self, embedding_remote_timeout_secs, 1u64, 600u64);
        self.check_api_key_env(errors);
    }

    /// The name of the environment variable the credential is read from.
    ///
    /// The value never enters the configuration -- that is the point of naming
    /// a variable rather than carrying a key -- but the *name* does, and it is
    /// used twice: as the key `std::env::var` is called with, and interpolated
    /// into the boot error when the variable is missing or empty. An
    /// unvalidated name therefore reaches the operator's log, where `tracing`
    /// does not escape a `Display` field and a newline is written as one. The
    /// gear already refuses a control character in a source namespace and in a
    /// remote model name for that reason; this is the same field class and was
    /// the one left unchecked.
    ///
    /// The shape is not this gear's invention. POSIX says an environment
    /// variable name is `[A-Za-z_][A-Za-z0-9_]*`, and a name outside that
    /// cannot be exported by an ordinary shell, so refusing it costs a
    /// deployment nothing it could have used -- and says at configuration time
    /// what it would otherwise learn from a lookup that silently never matches.
    /// `get_node` returns outgoing and incoming adjacency, each bounded by
    /// `node_read_max_adjacency` in its own right.
    const ADJACENCY_DIRECTIONS: u64 = 2;

    fn check_api_key_env(&self, errors: &mut Vec<String>) {
        let Some(variable) = &self.embedding_remote_api_key_env else {
            return;
        };
        let shaped = !variable.is_empty()
            && variable.len() <= 128
            && variable
                .chars()
                .next()
                .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && variable
                .chars()
                .all(|c| c == '_' || c.is_ascii_alphanumeric());
        if !shaped {
            errors.push(format!(
                "graph-storage.embedding_remote_api_key_env = {variable:?} is not an environment \
                 variable name: it must match [A-Za-z_][A-Za-z0-9_]* and be at most 128 characters"
            ));
        }
    }

    /// What one request may ask the graph to write.
    fn check_write_ranges(&self, errors: &mut Vec<String>) {
        check_range!(errors, self, ingest_max_nodes, 1u32, 50_000u32);
        check_range!(errors, self, ingest_max_edges, 1u32, 100_000u32);
        check_range!(errors, self, payload_max_bytes, 1_024u32, 1_048_576u32);
        check_range!(errors, self, identifier_max_bytes, 64u32, 65_536u32);
        check_range!(errors, self, search_query_max_bytes, 64u32, 1_048_576u32);
        check_range!(errors, self, item_max_bytes, 4_096u32, 4_194_304u32);
        check_range!(
            errors,
            self,
            ingest_max_bytes,
            1_048_576u64,
            1_073_741_824u64
        );
    }

    /// Limits that are each in range and wrong together.
    ///
    /// A count ceiling is only a memory bound in company with a size ceiling,
    /// so the two have to be checked as a product rather than one at a time.
    /// Every one of these combinations is reachable with values the ranges
    /// above accept -- a thousand-row page of four-megabyte items is four
    /// gigabytes, and every individual number in it is legal.
    fn check_limit_combinations(&self, errors: &mut Vec<String>) {
        check_range!(
            errors,
            self,
            response_max_bytes,
            1_048_576u64,
            1_073_741_824u64
        );
        if u64::from(self.payload_max_bytes) > u64::from(self.item_max_bytes) {
            errors.push(format!(
                "payload_max_bytes ({}) exceeds item_max_bytes ({}): an item could never \
                 carry a payload that large",
                self.payload_max_bytes, self.item_max_bytes
            ));
        }
        if u64::from(self.item_max_bytes) > self.ingest_max_bytes {
            errors.push(format!(
                "item_max_bytes ({}) exceeds ingest_max_bytes ({}): a batch could never \
                 carry even one item that large",
                self.item_max_bytes, self.ingest_max_bytes
            ));
        }
        // A node read is one element plus its adjacency, and the adjacency is
        // the half that grows: every entry carries an edge key, an edge type,
        // a neighbour key and a neighbour type, each of them a
        // caller-controlled identifier. Bounded by a count alone it is the
        // same shape of gap as a page bounded by a count alone, and it has no
        // runtime measure behind it -- the ceiling is small and fixed, which
        // is exactly when a startup check is the right instrument.
        //
        // **Twice the limit, because `get_node` applies it per direction.**
        // Adjacency is bidirectional and the limit bounds each side, which is
        // what makes a node read useful: a combined budget spent on the
        // outgoing side would hide the incoming one entirely, and the caller
        // could not tell an unreferenced node from a truncated answer. So the
        // worst case a single read returns is two full sides, and sizing this
        // for one was the check quietly guaranteeing half of what it claimed.
        let entry = 4u64.saturating_mul(u64::from(self.identifier_max_bytes));
        let entries = u64::from(self.node_read_max_adjacency)
            .saturating_mul(Self::ADJACENCY_DIRECTIONS)
            .saturating_mul(entry);
        let node_read = u64::from(self.item_max_bytes).saturating_add(entries);
        if node_read > self.response_max_bytes {
            errors.push(format!(
                "one node read is up to {node_read} bytes -- item_max_bytes ({}) plus \
                 node_read_max_adjacency ({}) entries of four identifiers each ({}) in \
                 each of two directions -- above response_max_bytes ({})",
                self.item_max_bytes,
                self.node_read_max_adjacency,
                self.identifier_max_bytes,
                self.response_max_bytes
            ));
        }
        for (what, count) in [
            ("projection_max_page", u64::from(self.projection_max_page)),
            // Both arms of a hybrid search, fused.
            (
                "search_max_arm_limit x 2",
                u64::from(self.search_max_arm_limit) * 2,
            ),
        ] {
            let worst = count.saturating_mul(u64::from(self.item_max_bytes));
            if worst > self.response_max_bytes {
                errors.push(format!(
                    "{what} ({count}) x item_max_bytes ({}) is {worst} bytes, above \
                     response_max_bytes ({}): this read is bounded by its count alone, so \
                     the product is the response it can actually return",
                    self.item_max_bytes, self.response_max_bytes
                ));
            }
        }
    }

    /// What one request may ask the graph to read.
    fn check_graph_ranges(&self, errors: &mut Vec<String>) {
        check_range!(errors, self, node_read_max_adjacency, 1u32, 1_000u32);
        check_range!(errors, self, traversal_max_depth, 1u8, 8u8);
        check_range!(errors, self, traversal_max_nodes, 1u32, 10_000u32);
        check_range!(errors, self, traversal_max_frontier, 1u32, 100_000u32);
        check_range!(
            errors,
            self,
            traversal_max_edges_scanned,
            1u64,
            10_000_000u64
        );
        check_range!(errors, self, search_max_arm_limit, 1u32, 500u32);
        check_range!(errors, self, projection_max_page, 1u32, 1_000u32);
        check_range!(errors, self, deadline_interactive_secs, 1u64, 300u64);
        check_range!(errors, self, idempotency_retention_days, 1u32, 365u32);
        check_range!(errors, self, ontology_max_chain_depth, 3u8, 16u8);
        check_range!(errors, self, type_update_max_rows, 1u32, 5_000_000u32);
        check_range!(errors, self, type_migration_max_rows, 1u32, 1_000_000u32);
        check_range!(errors, self, type_update_batch, 100u32, 10_000u32);
        check_range!(errors, self, type_update_max_reported_rows, 1u32, 1_000u32);
    }

    /// The interactive deadline as a duration.
    #[must_use]
    pub fn deadline_interactive(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.deadline_interactive_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pass_validation() {
        GraphStorageConfig::default()
            .validate()
            .unwrap_or_else(|e| panic!("defaults must validate: {e}"));
    }

    /// The checked type cannot be made out of a config that fails the check,
    /// which is the whole point of it: `PgGraphStore::new` and
    /// `GraphServices::new` ask for `ValidatedConfig`, so there is no
    /// construction path that skips the ranges. This test is the assertion
    /// that `validated` refuses; that the constructors demand the type is
    /// enforced by the compiler, and the test call sites that had to be
    /// changed to call `validated` are the evidence.
    #[test]
    fn an_out_of_range_config_cannot_become_a_validated_one() {
        let refused = GraphStorageConfig {
            embedding_dimension: 0,
            ..GraphStorageConfig::default()
        }
        .validated();
        assert!(
            refused.is_err(),
            "a zero embedding width is outside the hard range and must not validate"
        );

        assert!(
            GraphStorageConfig::default().validated().is_ok(),
            "and the defaults still produce one"
        );
    }

    /// The credential's variable *name* is configuration, and it reaches the
    /// operator's log through the boot error that quotes it. `tracing` does
    /// not escape a `Display` field, so a newline in it is written as one --
    /// the same shape already refused for a source namespace and a remote
    /// model name.
    #[test]
    fn a_credential_variable_that_is_not_a_variable_name_is_refused() {
        for name in [
            "",
            "1PASSWORD",            // a digit cannot start one
            "OPENAI KEY",           // nor a space appear in one
            "OPENAI\nKEY=injected", // the shape that forges a log line
            "OPENAI\u{0}KEY",
            "OPENAI-KEY", // a hyphen is not in the set
            &"K".repeat(129),
        ] {
            let cfg = GraphStorageConfig {
                embedding_remote_api_key_env: Some(name.to_owned()),
                ..GraphStorageConfig::default()
            };
            let message = match cfg.validate() {
                Err(error) => error.to_string(),
                Ok(()) => panic!("{name:?} must not pass as a variable name"),
            };
            assert!(
                message.contains("embedding_remote_api_key_env"),
                "the refusal must name the field, got: {message}"
            );
        }
    }

    #[test]
    fn the_variable_names_a_shell_can_export_are_accepted() {
        for name in ["OPENAI_API_KEY", "_KEY", "K", "a1_B2"] {
            let cfg = GraphStorageConfig {
                embedding_remote_api_key_env: Some(name.to_owned()),
                ..GraphStorageConfig::default()
            };
            assert!(
                cfg.validate().is_ok(),
                "{name:?} is an ordinary environment variable name"
            );
        }
        // Unset is the deployment whose endpoint takes no credential.
        assert!(
            GraphStorageConfig::default().validate().is_ok(),
            "no variable named is not a malformed name"
        );
    }

    /// The node-read invariant is sized for what a node read actually
    /// returns, which is two directions of adjacency and not one.
    ///
    /// `get_node` bounds outgoing and incoming separately, so the worst case
    /// is twice `node_read_max_adjacency`. Sizing the check for one side let
    /// a deployment pass startup and then answer a node read at up to double
    /// the size the check had just guaranteed -- and that check is the only
    /// guard there is, since nothing measures a node read at run time.
    ///
    /// The budget here sits between the two: comfortably above one side's
    /// worth of adjacency, below both.
    #[test]
    fn the_node_read_invariant_counts_both_directions_of_adjacency() {
        let defaults = GraphStorageConfig::default();
        let entry = 4 * u64::from(defaults.identifier_max_bytes);
        let one_side = u64::from(defaults.item_max_bytes)
            + u64::from(defaults.node_read_max_adjacency) * entry;
        let both_sides = one_side + u64::from(defaults.node_read_max_adjacency) * entry;
        let between = u64::midpoint(one_side, both_sides);
        assert!(
            one_side < between && between < both_sides,
            "the fixture is the gap"
        );

        let cfg = GraphStorageConfig {
            response_max_bytes: between,
            // Held out of the way: this budget is about adjacency, and the
            // other surfaces sized against response_max_bytes would refuse
            // it for their own reasons and prove nothing.
            traversal_max_nodes: 1,
            search_max_arm_limit: 1,
            projection_max_page: 1,
            ..defaults
        };
        let message = match cfg.validate() {
            Err(error) => error.to_string(),
            Ok(()) => panic!("a budget under two directions of adjacency must be refused"),
        };
        assert!(message.contains("node_read_max_adjacency"), "{message}");
        assert!(message.contains("two directions"), "{message}");
    }

    /// Every number legal on its own, and the product is four gigabytes.
    ///
    /// This is the check the count ceilings needed and did not have: a page
    /// limit is a memory bound only in company with a size limit, and neither
    /// range check can see the other.
    #[test]
    fn limits_that_are_each_in_range_and_wrong_together_are_refused() {
        let cfg = GraphStorageConfig {
            item_max_bytes: 4 * 1024 * 1024,
            projection_max_page: 1_000,
            ..GraphStorageConfig::default()
        };
        let message = match cfg.validate() {
            Err(error) => error.to_string(),
            Ok(()) => panic!("a 4 GiB page must be refused"),
        };
        assert!(message.contains("projection_max_page"), "{message}");
        assert!(message.contains("response_max_bytes"), "{message}");
    }

    #[test]
    fn a_payload_ceiling_above_the_item_ceiling_is_refused() {
        let cfg = GraphStorageConfig {
            payload_max_bytes: 1_048_576,
            item_max_bytes: 4_096,
            ..GraphStorageConfig::default()
        };
        let message = match cfg.validate() {
            Err(error) => error.to_string(),
            Ok(()) => panic!("an item that could never hold its own payload must be refused"),
        };
        assert!(message.contains("payload_max_bytes"), "{message}");
    }

    /// One legal item larger than the whole legal batch.
    #[test]
    fn an_item_ceiling_above_the_batch_ceiling_is_refused() {
        let cfg = GraphStorageConfig {
            item_max_bytes: 4 * 1024 * 1024,
            ingest_max_bytes: 1_048_576,
            ..GraphStorageConfig::default()
        };
        let message = match cfg.validate() {
            Err(error) => error.to_string(),
            Ok(()) => panic!("a batch that could never carry one item must be refused"),
        };
        assert!(message.contains("ingest_max_bytes"), "{message}");
    }

    /// A node read is an element *plus its adjacency*, and the adjacency is
    /// what grows. Bounded by a count alone it is the same gap as a page
    /// bounded by a count alone -- and unlike traversal it has no runtime
    /// measure behind it, so the startup check is the whole guard.
    #[test]
    fn an_unbounded_node_read_is_refused_at_startup() {
        let cfg = GraphStorageConfig {
            identifier_max_bytes: 65_536,
            response_max_bytes: 1_048_576,
            node_read_max_adjacency: 1_000,
            ..GraphStorageConfig::default()
        };
        let message = match cfg.validate() {
            Err(error) => error.to_string(),
            Ok(()) => panic!("a quarter-gigabyte node read must be refused"),
        };
        assert!(message.contains("node_read_max_adjacency"), "{message}");
        assert!(message.contains("response_max_bytes"), "{message}");
    }

    #[test]
    fn a_value_outside_the_hard_range_is_rejected_by_name() {
        let cfg = GraphStorageConfig {
            traversal_max_depth: 9,
            ..GraphStorageConfig::default()
        };
        let message = match cfg.validate() {
            Err(error) => error.to_string(),
            Ok(()) => panic!("depth 9 must be rejected"),
        };
        assert!(message.contains("traversal_max_depth"), "{message}");
        assert!(message.contains("1..=8"), "{message}");
    }
}
