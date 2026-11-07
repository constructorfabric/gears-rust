//! `WARN` and `ERROR` capture for the lib test binary, shared by every test that
//! asserts a logged line.

/// Records the `WARN` and `ERROR` events a test's thread emits, as
/// `<LEVEL> <message> <field>=<value>..` lines, while it lives.
///
/// Capture is one global subscriber per test binary, routed by thread, never a scoped
/// `set_default`: while one dispatcher is registered, tracing-core asks the default of
/// whichever thread first hits a call site for its interest, so another test's thread
/// with no default caches `Interest::never` there and this test's line is never seen.
pub(crate) struct WarnCapture {
    lines: SharedWarnLines,
}

type SharedWarnLines = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

thread_local! {
    static THREAD_CAPTURE: std::cell::RefCell<Option<SharedWarnLines>> =
        const { std::cell::RefCell::new(None) };
}

impl WarnCapture {
    /// Installs the global subscriber on first use and routes this thread's `WARN`
    /// and `ERROR` events to the returned capture until it is dropped.
    pub(crate) fn install() -> Self {
        static INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        INSTALLED.get_or_init(|| {
            tracing::subscriber::set_global_default(WarnRouter)
                .expect("no other global subscriber in the lib test binary");
            // A call site another test hit before the install cached the no-op
            // default's interest; this re-asks every call site.
            tracing::callsite::rebuild_interest_cache();
        });
        let lines = SharedWarnLines::default();
        THREAD_CAPTURE.with(|slot| *slot.borrow_mut() = Some(std::sync::Arc::clone(&lines)));
        Self { lines }
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

impl Drop for WarnCapture {
    fn drop(&mut self) {
        THREAD_CAPTURE.with(|slot| slot.borrow_mut().take());
    }
}

/// The global subscriber behind `WarnCapture`: admits `WARN` and `ERROR` only and
/// appends each line to the capture of the thread that emitted it, if any.
struct WarnRouter;

impl tracing::Subscriber for WarnRouter {
    fn register_callsite(
        &self,
        metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        if Self::admits(metadata) {
            tracing::subscriber::Interest::always()
        } else {
            tracing::subscriber::Interest::never()
        }
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::WARN)
    }

    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        Self::admits(metadata)
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        // `try_with`: an event during thread teardown has no capture to reach.
        let capture = THREAD_CAPTURE
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten();
        if let Some(lines) = capture {
            let mut line = WarnLine::default();
            event.record(&mut line);
            lines.lock().unwrap().push(format!(
                "{} {}{}",
                event.metadata().level(),
                line.message,
                line.fields
            ));
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

impl WarnRouter {
    fn admits(metadata: &tracing::Metadata<'_>) -> bool {
        matches!(
            *metadata.level(),
            tracing::Level::WARN | tracing::Level::ERROR
        )
    }
}

#[derive(Default)]
struct WarnLine {
    message: String,
    fields: String,
}

impl tracing::field::Visit for WarnLine {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields
                .push_str(&format!(" {}={value:?}", field.name()));
        }
    }
}
