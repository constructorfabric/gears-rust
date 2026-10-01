//! The command line, and the backend settings it resolves to.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use serde::Serialize;

/// How the backend is configured before `--settings` is merged over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// Small segments and a fast rescan: a segment lives through every state
    /// in minutes.
    Fast,
    /// The plugin's defaults.
    Default,
}

#[derive(Debug, Clone, Parser, Serialize)]
#[allow(clippy::struct_excessive_bools, reason = "command-line switches")]
#[command(
    name = "longhaul",
    about = "Runs the PostgreSQL event-broker backend under load until interrupted (longhaul/README.md)"
)]
pub struct Args {
    /// Events per second over all writers and topics.
    #[arg(long, default_value_t = 2000)]
    pub rate: u32,

    /// Events per append, all for one partition.
    #[arg(long, default_value_t = 10)]
    pub batch: usize,

    /// Bytes of padding in each event's payload.
    #[arg(long, default_value_t = 512)]
    pub payload_bytes: usize,

    /// Distinct partition keys writers draw from.
    #[arg(long, default_value_t = 1000)]
    pub keys: u32,

    /// Writers per topic, each with a fence key of its own.
    #[arg(long, default_value_t = 4)]
    pub writers: u32,

    /// Partitions of each topic.
    #[arg(long, default_value_t = 16)]
    pub partitions: u32,

    /// How often the rate bursts.
    #[arg(long, value_parser = humantime::parse_duration, default_value = "5m")]
    #[serde(with = "humantime_str")]
    pub burst_every: Duration,

    /// How long a burst lasts.
    #[arg(long, value_parser = humantime::parse_duration, default_value = "30s")]
    #[serde(with = "humantime_str")]
    pub burst_for: Duration,

    /// The rate during a burst, as a multiple of `--rate`.
    #[arg(long, default_value_t = 3.0)]
    pub burst_factor: f64,

    /// Retention of the `age` topic.
    #[arg(long, value_parser = humantime::parse_duration, default_value = "2m")]
    #[serde(with = "humantime_str")]
    pub max_age: Duration,

    /// Retention of the `bytes` topic, per partition.
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    pub max_bytes: u64,

    /// Events each history read asks for.
    #[arg(long, default_value_t = 100)]
    pub history_batch: usize,

    #[arg(long, value_enum, default_value_t = Profile::Fast)]
    pub profile: Profile,

    /// A JSON object merged over the profile's settings, key by key.
    #[arg(long, default_value = "{}")]
    pub settings: String,

    /// The schema the backend writes; it must not exist unless `--reset`.
    #[arg(long, default_value = "evbk_longhaul")]
    pub schema: String,

    /// Drop the schema first if it exists.
    #[arg(long)]
    pub reset: bool,

    /// How often the live invariants are checked.
    #[arg(long, value_parser = humantime::parse_duration, default_value = "30s")]
    #[serde(with = "humantime_str")]
    pub check_every: Duration,

    /// How often the status line is printed.
    #[arg(long, value_parser = humantime::parse_duration, default_value = "5s")]
    #[serde(with = "humantime_str")]
    pub status_every: Duration,

    /// How far past `--max-age` the oldest event of the `age` topic may be.
    #[arg(long, value_parser = humantime::parse_duration, default_value = "60s")]
    #[serde(with = "humantime_str")]
    pub retention_grace: Duration,

    /// End after this long instead of waiting for Ctrl-C.
    #[arg(long, value_parser = humantime::parse_duration)]
    #[serde(skip)]
    pub duration: Option<Duration>,

    /// End at the first violation.
    #[arg(long)]
    pub stop_on_violation: bool,

    /// Where the JSON report goes.
    #[arg(long, default_value = "target/eb-longhaul/report.json")]
    pub out: PathBuf,

    /// Passed by `cargo bench`; ignored.
    #[arg(long, hide = true)]
    #[serde(skip)]
    pub bench: bool,
}

impl Args {
    /// The backend's settings: the profile, `--settings` merged over it, then
    /// `url` and `schema`.
    pub fn backend_settings(&self, url: &str) -> Result<serde_json::Value, String> {
        let mut settings = match self.profile {
            Profile::Fast => serde_json::json!({
                "segment": { "events": 2000 },
                "maintenance": { "rescan_interval_ms": 5000, "crosscheck_every": 3 },
            }),
            Profile::Default => serde_json::json!({}),
        };
        let given: serde_json::Value = serde_json::from_str(&self.settings)
            .map_err(|err| format!("--settings is not JSON: {err}"))?;
        if !given.is_object() {
            return Err("--settings must be a JSON object".to_owned());
        }
        merge(&mut settings, given);
        merge(
            &mut settings,
            serde_json::json!({ "url": url, "schema": self.schema }),
        );
        Ok(settings)
    }
}

/// Merges `over` into `base`: objects key by key, anything else replaced.
fn merge(base: &mut serde_json::Value, over: serde_json::Value) {
    match (base, over) {
        (serde_json::Value::Object(base), serde_json::Value::Object(over)) => {
            for (key, value) in over {
                merge(base.entry(key).or_insert(serde_json::Value::Null), value);
            }
        }
        (base, over) => *base = over,
    }
}

/// `settings` as printed: `url` redacted.
pub fn redacted(settings: &serde_json::Value) -> serde_json::Value {
    let mut shown = settings.clone();
    if let Some(url) = shown.get_mut("url") {
        *url = serde_json::Value::String("<redacted>".to_owned());
    }
    shown
}

/// The rescan interval in effect: the plugin's default unless overridden.
pub fn rescan_interval(settings: &serde_json::Value) -> Duration {
    Duration::from_millis(
        settings
            .pointer("/maintenance/rescan_interval_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(60_000),
    )
}

/// How a segment is sized, as far as the warmup estimate needs it.
pub enum SegmentSize {
    Events(u64),
    Period(Duration),
}

/// The segment rule in effect: the plugin's default unless overridden.
pub fn segment_size(settings: &serde_json::Value) -> SegmentSize {
    if let Some(period) = settings
        .pointer("/segment/period")
        .and_then(serde_json::Value::as_str)
        .and_then(iso_seconds)
    {
        return SegmentSize::Period(period);
    }
    SegmentSize::Events(
        settings
            .pointer("/segment/events")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1_000_000),
    )
}

/// `PT<n>H`, `PT<n>M` or `PT<n>S`; anything else is not estimated.
fn iso_seconds(period: &str) -> Option<Duration> {
    let rest = period.strip_prefix("PT")?;
    let (number, unit) = rest.split_at(rest.len().checked_sub(1)?);
    let n: u64 = number.parse().ok()?;
    match unit {
        "H" => Some(Duration::from_secs(n * 3600)),
        "M" => Some(Duration::from_secs(n * 60)),
        "S" => Some(Duration::from_secs(n)),
        _ => None,
    }
}

/// Durations in the JSON report read as `5m`, not as a seconds/nanos pair.
mod humantime_str {
    use std::time::Duration;

    use serde::Serializer;

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&humantime::format_duration(*value))
    }
}
