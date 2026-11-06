//! Capture JSON tracing events and span fields on a current-thread test runtime.

use std::collections::BTreeMap;
use std::io;
use std::str::FromStr as _;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;
use tracing::Level;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;

/// One captured event: its level, its own fields, and its spans' fields, outermost first.
#[derive(Debug, Clone)]
pub struct Captured {
    pub level: Level,
    pub fields: BTreeMap<String, String>,
    /// `span name` → that span's fields.
    pub spans: Vec<(String, BTreeMap<String, String>)>,
}

impl Captured {
    /// The event's own field.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// The field of the innermost span named `span` around the event.
    pub fn span_field(&self, span: &str, name: &str) -> Option<&str> {
        self.spans
            .iter()
            .rev()
            .find(|(n, _)| n == span)
            .and_then(|(_, fields)| fields.get(name))
            .map(String::as_str)
    }

    pub fn log_kind(&self) -> Option<&str> {
        self.field("log_kind")
    }

    /// Every field and message, for asserting what never appears.
    pub fn text(&self) -> String {
        format!("{:?} {:?}", self.fields, self.spans)
    }

    /// One JSON log line.
    fn parse(line: &str) -> Self {
        let line: Value = serde_json::from_str(line).expect("a JSON log line");
        let level = line["level"]
            .as_str()
            .and_then(|level| Level::from_str(level).ok())
            .expect("a level");
        let spans = line["spans"]
            .as_array()
            .map(|spans| {
                spans
                    .iter()
                    .map(|span| {
                        let mut fields = text_fields(span);
                        let name = fields.remove("name").unwrap_or_default();
                        (name, fields)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            level,
            fields: text_fields(&line["fields"]),
            spans,
        }
    }
}

/// A JSON object's fields as text: strings as they are, other values as JSON.
fn text_fields(object: &Value) -> BTreeMap<String, String> {
    object
        .as_object()
        .map(|fields| {
            fields
                .iter()
                .map(|(name, value)| {
                    let text = value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned);
                    (name.clone(), text)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The in-memory log the subscriber writes to.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl io::Write for Log {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Log {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

/// The installed capture; uninstalled when dropped.
pub struct Capture {
    log: Log,
    _guard: DefaultGuard,
}

impl Capture {
    /// Capture events and spans enabled at `level`, as a production filter would.
    pub fn install(level: LevelFilter) -> Self {
        keep_interest_shared();
        let log = Log::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_current_span(false)
            .with_span_list(true)
            .with_max_level(level)
            .with_writer(log.clone())
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        Self { log, _guard: guard }
    }

    pub fn events(&self) -> Vec<Captured> {
        String::from_utf8_lossy(&self.log.0.lock())
            .lines()
            .map(Captured::parse)
            .collect()
    }

    /// The captured events whose `log_kind` field is `name`.
    pub fn named(&self, name: &str) -> Vec<Captured> {
        self.events()
            .into_iter()
            .filter(|e| e.log_kind() == Some(name))
            .collect()
    }

    /// The one captured event named `name`.
    pub fn one(&self, name: &str) -> Captured {
        let mut found = self.named(name);
        assert_eq!(found.len(), 1, "exactly one {name}: {found:#?}");
        found.remove(0)
    }
}

/// Keeps a second dispatcher registered for the whole binary. With only one, tracing-core
/// computes a callsite's interest from the registering thread's default, so a test without a
/// capture can cache `never` for a callsite another test's capture needs.
fn keep_interest_shared() {
    static SHARED: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
}
