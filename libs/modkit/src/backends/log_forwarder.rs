//! Log forwarding for `OoP` module stdout/stderr
//!
//! This module provides utilities for capturing stdout/stderr from child processes
//! and forwarding each line to the parent's tracing system with proper context.

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Level;
use uuid::Uuid;

/// Stream type identifier for logging
#[derive(Debug, Clone, Copy)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

impl std::fmt::Display for StreamKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamKind::Stdout => write!(f, "stdout"),
            StreamKind::Stderr => write!(f, "stderr"),
        }
    }
}

/// Detect log level from a tracing-subscriber formatted line.
///
/// Supports two formats:
///
/// 1. Plain text format (tracing-subscriber default):
/// ```text
/// 2025-12-08T00:10:18.2852399Z  INFO module_name: message
/// 2025-12-08T00:10:18.2852399Z DEBUG module_name: message
/// ```
///
/// 2. JSON format (tracing-subscriber with json layer):
/// ```json
/// {"timestamp":"2025-12-09T21:09:40Z","level":"INFO","fields":{"message":"..."},"target":"..."}
/// {"timestamp":"2025-12-09T21:09:40Z","level":"DEBUG","fields":{"message":"..."},"target":"..."}
/// ```
///
/// Returns INFO as the default for unrecognized formats.
fn detect_log_level(line: &str) -> Level {
    if let Some(level) = detect_json_level(line) {
        return level;
    }
    if let Some(level) = detect_plain_level(line) {
        return level;
    }
    Level::INFO
}

fn detect_plain_level(line: &str) -> Option<Level> {
    let mut parts = line.split_whitespace();
    let _timestamp = parts.next()?;
    let level_str = parts.next()?;

    match level_str {
        "ERROR" | "error" => Some(Level::ERROR),
        "WARN" | "warn" => Some(Level::WARN),
        "INFO" | "info" => Some(Level::INFO),
        "DEBUG" | "debug" => Some(Level::DEBUG),
        "TRACE" | "trace" => Some(Level::TRACE),
        _ => None,
    }
}

fn detect_json_level(line: &str) -> Option<Level> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('{') || !trimmed.contains("\"level\"") {
        return None;
    }

    let v: Value = serde_json::from_str(trimmed).ok()?;
    let level = v.get("level")?.as_str()?.to_ascii_lowercase();

    match level.as_str() {
        "error" => Some(Level::ERROR),
        "warn" => Some(Level::WARN),
        "info" => Some(Level::INFO),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => None,
    }
}

/// Forward a single line to tracing with the detected level.
///
/// Uses dynamic dispatch via `tracing::event!` macro with appropriate level.
fn forward_line(module: &str, instance_id: Uuid, stream: StreamKind, line: &str) {
    let level = detect_log_level(line);

    match level {
        Level::ERROR => {
            tracing::error!(
                oop_module = %module,
                oop_instance_id = %instance_id,
                stream = %stream,
                "{line}"
            );
        }
        Level::WARN => {
            tracing::warn!(
                oop_module = %module,
                oop_instance_id = %instance_id,
                stream = %stream,
                "{line}"
            );
        }
        Level::INFO => {
            tracing::info!(
                oop_module = %module,
                oop_instance_id = %instance_id,
                stream = %stream,
                "{line}"
            );
        }
        Level::DEBUG => {
            tracing::debug!(
                oop_module = %module,
                oop_instance_id = %instance_id,
                stream = %stream,
                "{line}"
            );
        }
        Level::TRACE => {
            tracing::trace!(
                oop_module = %module,
                oop_instance_id = %instance_id,
                stream = %stream,
                "{line}"
            );
        }
    }
}

/// Spawn a task that reads lines from stdout and forwards them to tracing.
///
/// The task will run until either:
/// - The stream is closed (child process exits)
/// - The cancellation token is triggered
pub fn spawn_stream_forwarder<S>(
    stream: S,
    module: String,
    instance_id: Uuid,
    cancel: CancellationToken,
    kind: StreamKind,
) -> JoinHandle<()>
where
    S: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let reader = BufReader::new(stream);
        let mut lines = reader.lines();

        loop {
            tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    tracing::debug!(
                        oop_module = %module,
                        oop_instance_id = %instance_id,
                        stream = ?kind,
                        "log forwarder cancelled"
                    );
                    break;
                }

                result = lines.next_line() => {
                    match result {
                        Ok(Some(line)) => {
                            forward_line(&module, instance_id, kind, &line);
                        }
                        Ok(None) => {
                            tracing::debug!(
                                oop_module = %module,
                                oop_instance_id = %instance_id,
                                stream = ?kind,
                                "log stream closed"
                            );
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(
                                oop_module = %module,
                                oop_instance_id = %instance_id,
                                stream = ?kind,
                                error = %e,
                                "log stream read error"
                            );
                            break;
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
#[path = "log_forwarder_tests.rs"]
mod tests;
