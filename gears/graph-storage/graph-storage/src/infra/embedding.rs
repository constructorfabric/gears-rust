//! Embedding providers: implementations of
//! [`graph_storage_sdk::plugin_api::EmbeddingProviderV1`].
//!
//! The in-process ONNX provider ADR-0005 makes the default lives in its own
//! crate, because it drags ONNX Runtime and model weights behind it and most
//! deployments of this gear should not pay for that at link time. What lives
//! here is the deterministic fake the same ADR mandates for CI: contract
//! tests have to run with no model download and no `libonnxruntime`, and the
//! acceptance test — a document ranked first by its own text — needs a
//! provider whose output is reproducible across processes.

pub mod fake;
