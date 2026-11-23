use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::infra::parsers::ParseLimits;
use crate::infra::parsers::backend::{DEFAULT_MAX_MB, MIB};
use crate::infra::parsers::image_parser::DEFAULT_MAX_IMAGE_MB;
use crate::infra::parsers::pdf::DEFAULT_MAX_QUEUED;

/// Largest PDF engine queue accepted; each queued document holds its bytes.
const MAX_PDF_QUEUE: usize = 4096;
/// `pdf_timeout` range accepted.
const MIN_PDF_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_PDF_TIMEOUT: Duration = Duration::from_hours(1);
/// `parse_route_timeout` range accepted.
const MIN_ROUTE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_ROUTE_TIMEOUT: Duration = Duration::from_hours(4);
/// Added to the PDF engine's longest wait for the default parse route
/// deadline: structure, rendering and the upload itself.
const PARSE_ROUTE_MARGIN: Duration = Duration::from_secs(10);

/// Configuration for the `file_parser` gear
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileParserConfig {
    /// Largest file read, local or uploaded, in MiB. Default 100.
    #[serde(default = "default_max_file_size_mb")]
    pub max_file_size_mb: u64,

    /// Largest image the image parser reads, in MiB. Default 50, or
    /// `max_file_size_mb` when that is smaller; at most `max_file_size_mb`.
    #[serde(default)]
    pub max_image_size_mb: Option<u64>,

    /// Base directory for local file parsing (**required at runtime**). Only
    /// files under this directory (after symlink resolution / canonicalization)
    /// are allowed.  The gear will fail to start if this field is missing or
    /// the path cannot be resolved.
    pub allowed_local_base_dir: PathBuf,

    /// ONNX Runtime intra-op threads for the detector's single
    /// `magika::Session`. Only used with the `magika` feature.
    ///
    /// `None` (default, recommended) keeps ONNX Runtime's own default: measured
    /// ~6 threads and ~3.5 ms per detection on a 10-core host. Lower it only to
    /// stop detection contending with other CPU-bound work, at a real latency
    /// cost — ~5.6 ms at `2`, ~9.3 ms at `1`.
    ///
    /// There is no session-*count* setting: `magika::Session` needs `&mut self`
    /// per inference and each session carries its own thread pool and model
    /// copy, so N sessions multiply both. See `infra::magika_detector`.
    #[serde(default)]
    pub magika_intra_op_threads: Option<NonZeroUsize>,

    /// Minimum detector confidence, in `[0.0, 1.0]`, at which content detection
    /// may override the caller's filename / `Content-Type` hint. Below it the
    /// hint wins. Default `0.90`.
    ///
    /// Configurable because the default is an initial guess, not a tuned value.
    /// Only has effect when a detector is registered. `Gear::init` rejects
    /// out-of-range values and `NaN` at startup rather than clamping.
    #[serde(default = "default_detection_confidence_threshold")]
    pub detection_confidence_threshold: f32,

    /// Limits on the work one document may cause; every key is optional.
    #[serde(default)]
    pub limits: ParseLimitsConfig,

    /// The PDF engine.
    #[serde(default)]
    pub pdf: PdfEngineConfig,

    /// Request deadline of the parse routes, humantime (`5m`). It replaces
    /// the gateway's default request timeout for those routes, also when that
    /// is longer. From 1 second to 4 hours. Default: twice
    /// `limits.pdf_timeout` plus 12 s (132 s), enough for a PDF to wait for
    /// the engine and then be read.
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    pub parse_route_timeout: Option<Duration>,
}

/// [`ParseLimits`] as configured. Defaults are those of
/// `ParseLimits::default()`; see the gear's DESIGN.md for what each bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ParseLimitsConfig {
    pub max_zip_entries: usize,
    pub max_entry_bytes: u64,
    pub max_total_uncompressed_bytes: u64,
    pub max_compression_ratio: u64,
    pub ratio_check_floor_bytes: u64,
    pub max_nesting_depth: usize,
    pub max_table_cells: usize,
    pub max_table_columns: usize,
    pub max_sheets: usize,
    pub max_html_nodes: usize,
    pub max_pdf_pages: usize,
    pub max_pdf_chars_per_page: usize,
    /// Humantime (`60s`, `2m`). From 1 second to 1 hour.
    #[serde(with = "toolkit_utils::humantime_serde")]
    pub pdf_timeout: Duration,
    /// Output text plus a fixed cost per block, inline run and PDF line
    /// (see `ParseLimits::max_text_bytes`).
    pub max_text_bytes: usize,
}

impl Default for ParseLimitsConfig {
    fn default() -> Self {
        let d = ParseLimits::default();
        Self {
            max_zip_entries: d.max_zip_entries,
            max_entry_bytes: d.max_entry_bytes,
            max_total_uncompressed_bytes: d.max_total_uncompressed_bytes,
            max_compression_ratio: d.max_compression_ratio,
            ratio_check_floor_bytes: d.ratio_check_floor_bytes,
            max_nesting_depth: d.max_nesting_depth,
            max_table_cells: d.max_table_cells,
            max_table_columns: d.max_table_columns,
            max_sheets: d.max_sheets,
            max_html_nodes: d.max_html_nodes,
            max_pdf_pages: d.max_pdf_pages,
            max_pdf_chars_per_page: d.max_pdf_chars_per_page,
            pdf_timeout: d.pdf_timeout,
            max_text_bytes: d.max_text_bytes,
        }
    }
}

impl From<&ParseLimitsConfig> for ParseLimits {
    fn from(c: &ParseLimitsConfig) -> Self {
        Self {
            max_zip_entries: c.max_zip_entries,
            max_entry_bytes: c.max_entry_bytes,
            max_total_uncompressed_bytes: c.max_total_uncompressed_bytes,
            max_compression_ratio: c.max_compression_ratio,
            ratio_check_floor_bytes: c.ratio_check_floor_bytes,
            max_nesting_depth: c.max_nesting_depth,
            max_table_cells: c.max_table_cells,
            max_table_columns: c.max_table_columns,
            max_sheets: c.max_sheets,
            max_html_nodes: c.max_html_nodes,
            max_pdf_pages: c.max_pdf_pages,
            max_pdf_chars_per_page: c.max_pdf_chars_per_page,
            pdf_timeout: c.pdf_timeout,
            max_text_bytes: c.max_text_bytes,
        }
    }
}

/// The PDF engine: one process-wide thread running libpdfium.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PdfEngineConfig {
    /// Documents that may wait for the engine; more are refused as busy.
    /// Each waiting document holds its bytes. From 1 to 4096. Default 64.
    pub max_queued: usize,
    /// Bind libpdfium during startup, so a broken deployment shows in the
    /// startup log. A failure only warns. Default true.
    pub check_on_startup: bool,
}

impl Default for PdfEngineConfig {
    fn default() -> Self {
        Self {
            max_queued: DEFAULT_MAX_QUEUED,
            check_on_startup: true,
        }
    }
}

impl FileParserConfig {
    /// Checks values that would break parsing or make every document fail.
    ///
    /// # Errors
    /// The first invalid field, by name.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_file_size_mb == 0 {
            return Err("max_file_size_mb must be at least 1".to_owned());
        }
        if self.max_image_size_mb == Some(0) {
            return Err("max_image_size_mb must be at least 1".to_owned());
        }
        // Uploads over max_file_size_mb are refused before any parser runs.
        if self
            .max_image_size_mb
            .is_some_and(|mb| mb > self.max_file_size_mb)
        {
            return Err("max_image_size_mb must not exceed max_file_size_mb".to_owned());
        }
        let l = &self.limits;
        let counts = [
            ("max_zip_entries", l.max_zip_entries as u64),
            ("max_entry_bytes", l.max_entry_bytes),
            (
                "max_total_uncompressed_bytes",
                l.max_total_uncompressed_bytes,
            ),
            ("max_compression_ratio", l.max_compression_ratio),
            ("max_nesting_depth", l.max_nesting_depth as u64),
            ("max_table_cells", l.max_table_cells as u64),
            ("max_table_columns", l.max_table_columns as u64),
            ("max_sheets", l.max_sheets as u64),
            ("max_html_nodes", l.max_html_nodes as u64),
            ("max_pdf_pages", l.max_pdf_pages as u64),
            ("max_pdf_chars_per_page", l.max_pdf_chars_per_page as u64),
            ("max_text_bytes", l.max_text_bytes as u64),
        ];
        if let Some((name, _)) = counts.iter().find(|(_, value)| *value == 0) {
            return Err(format!("limits.{name} must be at least 1"));
        }
        if l.max_entry_bytes > l.max_total_uncompressed_bytes {
            return Err(
                "limits.max_entry_bytes must not exceed limits.max_total_uncompressed_bytes"
                    .to_owned(),
            );
        }
        if !(MIN_PDF_TIMEOUT..=MAX_PDF_TIMEOUT).contains(&l.pdf_timeout) {
            return Err("limits.pdf_timeout must be between 1s and 1h".to_owned());
        }
        if self
            .parse_route_timeout
            .is_some_and(|t| !(MIN_ROUTE_TIMEOUT..=MAX_ROUTE_TIMEOUT).contains(&t))
        {
            return Err("parse_route_timeout must be between 1s and 4h".to_owned());
        }
        if !(1..=MAX_PDF_QUEUE).contains(&self.pdf.max_queued) {
            return Err(format!(
                "pdf.max_queued must be between 1 and {MAX_PDF_QUEUE}"
            ));
        }
        Ok(())
    }

    /// The parse routes' request deadline: `parse_route_timeout`, else the
    /// PDF engine's longest wait for `limits.pdf_timeout` plus a margin.
    #[must_use]
    pub fn parse_route_timeout(&self) -> Duration {
        self.parse_route_timeout
            .unwrap_or_else(|| default_parse_route_timeout(self.limits.pdf_timeout))
    }

    /// `max_file_size_mb` in bytes.
    #[must_use]
    pub fn max_file_size_bytes(&self) -> u64 {
        self.max_file_size_mb.saturating_mul(MIB)
    }

    /// The image cap in bytes: `max_image_size_mb`, else the image default
    /// or the file cap, whichever is smaller.
    #[must_use]
    pub fn max_image_size_bytes(&self) -> u64 {
        self.max_image_size_mb
            .unwrap_or(DEFAULT_MAX_IMAGE_MB.min(self.max_file_size_mb))
            .saturating_mul(MIB)
    }
}

/// Default deadline of the parse routes for `pdf_timeout`.
fn default_parse_route_timeout(pdf_timeout: Duration) -> Duration {
    crate::infra::parsers::pdf::longest_wait(pdf_timeout).saturating_add(PARSE_ROUTE_MARGIN)
}

fn default_detection_confidence_threshold() -> f32 {
    crate::domain::service::DEFAULT_DETECTION_CONFIDENCE_THRESHOLD
}

fn default_max_file_size_mb() -> u64 {
    DEFAULT_MAX_MB
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::infra::parsers::image_parser::DEFAULT_MAX_IMAGE_BYTES;

    fn parse(json: serde_json::Value) -> Result<FileParserConfig, serde_json::Error> {
        let mut config = serde_json::json!({ "allowed_local_base_dir": "/tmp" });
        if let (Some(base), serde_json::Value::Object(extra)) = (config.as_object_mut(), json) {
            base.extend(extra);
        }
        serde_json::from_value(config)
    }

    #[test]
    fn an_empty_config_gives_the_defaults() {
        let config = parse(serde_json::json!({})).unwrap();
        assert_eq!(ParseLimits::from(&config.limits), ParseLimits::default());
        assert_eq!(config.pdf, PdfEngineConfig::default());
        assert_eq!(config.max_image_size_bytes(), DEFAULT_MAX_IMAGE_BYTES);
        assert_eq!(config.parse_route_timeout(), Duration::from_secs(132));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn the_parse_route_deadline_follows_pdf_timeout_unless_set() {
        let config = parse(serde_json::json!({ "limits": { "pdf_timeout": "30s" } })).unwrap();
        assert_eq!(config.parse_route_timeout(), Duration::from_secs(72));
        let config = parse(serde_json::json!({
            "limits": { "pdf_timeout": "30s" },
            "parse_route_timeout": "10m",
        }))
        .unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.parse_route_timeout(), Duration::from_mins(10));
        assert!(parse(serde_json::json!({ "parse_route_timeout": "ten" })).is_err());
    }

    #[test]
    fn a_small_file_cap_also_caps_the_default_image_size() {
        let config = parse(serde_json::json!({ "max_file_size_mb": 20 })).unwrap();
        assert!(
            config.validate().is_ok(),
            "a config valid before stays valid"
        );
        assert_eq!(config.max_image_size_bytes(), 20 * MIB);
        assert_eq!(config.max_file_size_bytes(), 20 * MIB);
    }

    #[test]
    fn limits_override_one_key_at_a_time() {
        let config = parse(serde_json::json!({
            "limits": { "max_pdf_pages": 10, "pdf_timeout": "90s" },
            "pdf": { "max_queued": 8, "check_on_startup": false },
            "max_image_size_mb": 5,
        }))
        .unwrap();
        let limits = ParseLimits::from(&config.limits);
        assert_eq!(limits.max_pdf_pages, 10);
        assert_eq!(limits.pdf_timeout, Duration::from_secs(90));
        assert_eq!(limits.max_text_bytes, ParseLimits::default().max_text_bytes);
        assert_eq!(config.pdf.max_queued, 8);
        assert!(!config.pdf.check_on_startup);
        assert_eq!(config.max_image_size_bytes(), 5 * MIB);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        for json in [
            serde_json::json!({ "limits": { "max_pages": 1 } }),
            serde_json::json!({ "pdf": { "queue": 1 } }),
        ] {
            assert!(parse(json).is_err());
        }
    }

    #[test]
    fn invalid_values_name_their_field() {
        let cases = [
            (
                serde_json::json!({ "max_file_size_mb": 0 }),
                "max_file_size_mb",
            ),
            (
                serde_json::json!({ "max_image_size_mb": 0 }),
                "max_image_size_mb",
            ),
            (
                serde_json::json!({ "max_image_size_mb": 200, "max_file_size_mb": 100 }),
                "max_image_size_mb",
            ),
            (
                serde_json::json!({ "limits": { "max_text_bytes": 0 } }),
                "limits.max_text_bytes",
            ),
            (
                serde_json::json!({ "limits": { "max_entry_bytes": 10, "max_total_uncompressed_bytes": 5 } }),
                "limits.max_entry_bytes",
            ),
            (
                serde_json::json!({ "limits": { "pdf_timeout": "500ms" } }),
                "limits.pdf_timeout",
            ),
            (
                serde_json::json!({ "limits": { "pdf_timeout": "2h" } }),
                "limits.pdf_timeout",
            ),
            (
                serde_json::json!({ "parse_route_timeout": "500ms" }),
                "parse_route_timeout",
            ),
            (
                serde_json::json!({ "parse_route_timeout": "5h" }),
                "parse_route_timeout",
            ),
            (
                serde_json::json!({ "pdf": { "max_queued": 0 } }),
                "pdf.max_queued",
            ),
            (
                serde_json::json!({ "pdf": { "max_queued": 5000 } }),
                "pdf.max_queued",
            ),
        ];
        for (json, field) in cases {
            let err = parse(json).unwrap().validate().expect_err(field);
            assert!(err.contains(field), "{err}");
        }
    }
}
