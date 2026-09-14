use std::num::NonZeroUsize;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for the `file_parser` gear
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileParserConfig {
    #[serde(default = "default_max_file_size_mb")]
    pub max_file_size_mb: u64,

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
}

fn default_detection_confidence_threshold() -> f32 {
    crate::domain::service::DEFAULT_DETECTION_CONFIDENCE_THRESHOLD
}

fn default_max_file_size_mb() -> u64 {
    100
}
