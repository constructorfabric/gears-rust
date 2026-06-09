//! Shared test helpers for chat-engine integration tests.
//!
//! `FakePlugin` is a `ChatEngineBackendPlugin` implementation scripted by a
//! pre-built sequence of streaming events (or a hang/pre-error). It records
//! call counts via `Arc<AtomicUsize>` so tests can assert plugin invocation
//! patterns without resorting to mocks-frameworks.
//
// @cpt-cf-chat-engine-e2e-harness:p16

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;

use chat_engine_sdk::{
    ChatEngineBackendPlugin, MessagePluginCtx, PluginError, PluginStream, StreamingEvent,
    stream_from_events,
};

pub mod db;

/// Plugin script — what events (or error / hang) the fake plugin produces on
/// the *next* `on_message` invocation. Each invocation consumes the script;
/// subsequent calls return `empty_stream()`.
pub enum FakePluginScript {
    /// Replay these events in order then close cleanly.
    Events(Vec<StreamingEvent>),
    /// Fail before the stream starts.
    PreError(PluginError),
    /// Return a stream that never yields (forever pending). Used to model
    /// upstream stalls so deadline / cancellation branches can fire.
    Hang,
    /// Replay these events in order then hang (forever pending). The driver
    /// observes the chunks, flushes them through the bounded channel, and
    /// then stalls until the parent cancellation token fires. Used to
    /// exercise the "cancel after partial chunks" persistence path against
    /// a real `MessageService`.
    EventsThenHang(Vec<StreamingEvent>),
}

/// Scriptable fake plugin used across integration tests.
///
/// Wrapped in `Arc` for cheap sharing between the test body and the
/// dyn-dispatched call sites; cloning the outer `Arc` does NOT clone the
/// script (the script is consumed on first call) or the call counter.
pub struct FakePlugin {
    id: String,
    script: Mutex<Option<FakePluginScript>>,
    calls: AtomicUsize,
}

impl FakePlugin {
    #[must_use]
    pub fn new(id: &str, script: FakePluginScript) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_owned(),
            script: Mutex::new(Some(script)),
            calls: AtomicUsize::new(0),
        })
    }

    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ChatEngineBackendPlugin for FakePlugin {
    async fn on_message(
        &self,
        _ctx: MessagePluginCtx,
    ) -> Result<PluginStream, PluginError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let script = self
            .script
            .lock()
            .take()
            .unwrap_or(FakePluginScript::Events(vec![]));
        match script {
            FakePluginScript::Events(events) => Ok(stream_from_events(events)),
            FakePluginScript::PreError(e) => Err(e),
            FakePluginScript::Hang => Ok(forever_pending_stream()),
            FakePluginScript::EventsThenHang(events) => {
                let items: Vec<Result<StreamingEvent, PluginError>> =
                    events.into_iter().map(Ok).collect();
                let head = futures::stream::iter(items);
                let tail = futures::stream::poll_fn(
                    |_cx| -> std::task::Poll<Option<Result<StreamingEvent, PluginError>>> {
                        std::task::Poll::Pending
                    },
                );
                Ok(head.chain(tail).boxed())
            }
        }
    }

    async fn on_message_recreate(
        &self,
        ctx: MessagePluginCtx,
    ) -> Result<PluginStream, PluginError> {
        self.on_message(ctx).await
    }

    fn plugin_instance_id(&self) -> &str {
        &self.id
    }
}

/// Stream that yields `Pending` forever. Used so cancellation/deadline tests
/// can drive the driver-equivalent loop into the cancel branch.
fn forever_pending_stream() -> PluginStream {
    futures::stream::poll_fn(|_cx| std::task::Poll::Pending).boxed()
}
