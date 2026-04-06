//! Scenario-based tests demonstrating worker infrastructure behaviour in
//! realistic settings. Each test tells a story — the name describes the
//! situation, the body shows how the worker handles it.
//!
//! All tests use `start_paused = true` (tokio virtual time) so durations
//! are realistic (hours, seconds) yet tests complete instantly.

#[cfg(test)]
#[path = "showcase_tests.rs"]
mod tests;
