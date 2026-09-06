// Created: 2026-09-06 by Constructor Tech
//! Demo contributor for the Settings Service.
//!
//! Registers a sample catalogue of declarations through the SDK's
//! `SettingsContributionClient` from its own init, the way any gear that ships
//! settings does. Example code: enabled by the example server's
//! `settings-demo` feature and nothing else.

pub mod catalog;
pub mod gear;

pub use gear::SettingsDemo;
