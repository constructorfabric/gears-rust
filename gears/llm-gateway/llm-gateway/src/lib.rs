//! LLM Gateway gear.
//!
//! Hosts the Open Responses API (REST and the in-process `LlmGatewayClientV1`
//! client) in front of provider plugins. The core owns authorization, model
//! resolution, plugin selection, credential resolution, and in-flight limits;
//! plugins own everything provider specific.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod config;
pub mod domain;
pub(crate) mod gts;
pub mod infra;
pub mod module;
