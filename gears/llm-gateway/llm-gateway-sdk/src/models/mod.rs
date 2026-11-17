// Created: 2026-07-09 by Constructor Tech
//! Public models for the LLM Gateway, one submodule per schema domain.
//!
//! All types derive `serde::Serialize + serde::Deserialize + schemars::JsonSchema`
//! to match the SDK serde policy used across the gears (see
//! `model-registry-sdk`). Layout mirrors `llm-gateway-sdk/schemas/`:
//! - [`core`] — request/response bodies, embeddings, usage, shared config enums.
//! - [`items`] — input/output item families.
//! - [`content`] — input/output content parts.
//! - [`tools`] — tool definitions.
//! - [`streaming`] — server-sent streaming events.
//! - [`plugin`] — provider-plugin call context and capabilities.
//! - [`passthrough`] — pass-through requests, native responses and events,
//!   and the endpoint allowlist; the in-process request and response types
//!   carry streams and are not serializable.

pub mod content;
pub mod core;
pub mod extension;
pub mod items;
pub mod passthrough;
pub mod plugin;
pub mod role;
pub mod streaming;
pub mod tools;
