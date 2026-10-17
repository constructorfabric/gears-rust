use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::domain::error::DomainError;
use crate::domain::ports::telemetry_sink::TelemetrySink;
use crate::domain::sync::TelemetryLine;

const QUEUED_LINES: usize = 4096;

#[derive(Debug)]
enum Message {
    Line(PathBuf, String),
    Flush(oneshot::Sender<()>),
}

#[derive(Debug)]
pub struct JsonlTelemetrySink {
    queue: mpsc::Sender<Message>,
}

impl JsonlTelemetrySink {
    /// # Errors
    /// The writer thread cannot be started.
    pub fn start() -> std::io::Result<Self> {
        let (queue, mut messages) = mpsc::channel(QUEUED_LINES);
        std::thread::Builder::new()
            .name("github-mirror-telemetry".to_owned())
            .spawn(move || {
                let mut failed = HashSet::new();
                while let Some(message) = messages.blocking_recv() {
                    match message {
                        Message::Line(file, text) => {
                            if let Err(e) = append(&file, &text)
                                && !failed.contains(&file)
                            {
                                tracing::warn!(
                                    file = %file.display(),
                                    error = %e,
                                    "could not write the telemetry file; the sync goes on without it"
                                );
                                failed.insert(file);
                            }
                        }
                        Message::Flush(done) => {
                            done.send(()).ok();
                        }
                    }
                }
            })?;
        Ok(Self { queue })
    }
}

fn append(file: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?
        .write_all(text.as_bytes())
}

#[async_trait]
impl TelemetrySink for JsonlTelemetrySink {
    fn record(&self, file: &Path, line: &TelemetryLine<'_>) -> Result<(), DomainError> {
        let mut text = serde_json::to_string(line)
            .map_err(|e| DomainError::internal(format!("telemetry line: {e}")))?;
        text.push('\n');

        self.queue
            .try_send(Message::Line(file.to_path_buf(), text))
            .map_err(|e| DomainError::internal(format!("telemetry file {}: {e}", file.display())))
    }

    async fn flush(&self) {
        let (done, written) = oneshot::channel();
        if self.queue.send(Message::Flush(done)).await.is_ok() {
            written.await.ok();
        }
    }
}

#[cfg(test)]
#[path = "telemetry_sink_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod telemetry_sink_tests;
