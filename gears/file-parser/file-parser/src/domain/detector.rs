//! Optional content-based file type detection.
//!
//! `FileParserService` accepts an `Option<Arc<dyn ContentTypeDetector>>`.
//! With none registered (the default, and the only possibility without the
//! `magika` feature), routing behaves exactly as before this trait existed:
//! filename extension, then `Content-Type`.

use std::path::Path;

use async_trait::async_trait;
use toolkit_macros::domain_model;

/// Detector confidence, validated to lie in `[0.0, 1.0]` at construction.
///
/// Values within [`Self::CLAMP_EPSILON`] of the range are clamped as
/// floating-point drift; anything further out is rejected, so a detector with a
/// scoring bug cannot gain the power to override a correct hint. `NaN` is
/// rejected too.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(f32);

impl Confidence {
    /// How far outside `[0.0, 1.0]` a value may drift and still count as
    /// floating-point noise rather than a broken detector. Public because it is
    /// the whole difference between a clamped score and a rejected one.
    pub const CLAMP_EPSILON: f32 = 1e-4;

    /// Builds a `Confidence` from `value`. Returns `None` if `value` is
    /// `NaN`, or lies more than [`Self::CLAMP_EPSILON`] outside
    /// `[0.0, 1.0]`; values within that margin are clamped into range.
    #[must_use]
    pub fn new(value: f32) -> Option<Self> {
        // Rejecting `NaN` here is load-bearing, not incidental: comparisons
        // against `NaN` are all false, so `contains` excludes it. A separate
        // `is_nan()` check would say so more plainly but trips
        // `clippy::manual_range_contains`.
        if !(-Self::CLAMP_EPSILON..=1.0 + Self::CLAMP_EPSILON).contains(&value) {
            return None;
        }
        Some(Self(value.clamp(0.0, 1.0)))
    }

    /// The underlying confidence value, guaranteed to lie in `[0.0, 1.0]`.
    #[must_use]
    pub fn get(self) -> f32 {
        self.0
    }
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod confidence_tests {
    use super::Confidence;

    #[test]
    fn in_range_values_pass_through() {
        assert!((Confidence::new(0.0).unwrap().get() - 0.0).abs() < f32::EPSILON);
        assert!((Confidence::new(0.5).unwrap().get() - 0.5).abs() < f32::EPSILON);
        assert!((Confidence::new(1.0).unwrap().get() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn tiny_drift_outside_the_range_is_clamped() {
        assert!((Confidence::new(-0.000_01).unwrap().get() - 0.0).abs() < f32::EPSILON);
        assert!((Confidence::new(1.000_01).unwrap().get() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn far_out_of_range_values_are_rejected_not_clamped() {
        assert!(Confidence::new(5.0).is_none());
        assert!(Confidence::new(-5.0).is_none());
    }

    #[test]
    fn nan_is_rejected() {
        assert!(Confidence::new(f32::NAN).is_none());
    }
}

/// Result of inspecting file content to determine its type.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedType {
    /// File extension implied by the detected content type (e.g. `"pdf"`),
    /// without the leading dot.
    pub extension: String,
    /// Detector confidence in `[0.0, 1.0]`.
    pub confidence: Confidence,
}

/// Inspects raw file content and identifies its type, independent of any
/// filename or `Content-Type` hint the caller supplied.
///
/// Implementations are expected to be CPU-bound and are therefore async so
/// they can offload inference to a blocking thread pool internally (e.g. via
/// `tokio::task::spawn_blocking`) rather than blocking the calling task.
///
/// # Contract
///
/// `detect` must return `None` — not a low-confidence [`DetectedType`] — for
/// any content whose identified type has no corresponding registered
/// [`FileParserBackend::supported_extensions`](crate::domain::parser::FileParserBackend::supported_extensions)
/// entry. `FileParserService` does not re-validate this; it treats every
/// returned [`DetectedType::extension`] as routable and falls back to the
/// caller-supplied hint only when `detect` itself returns `None` or a
/// confidence below its threshold. `MagikaDetector` upholds this by
/// intersecting Magika's own label-to-extension list with the registered
/// extensions it's constructed with.
#[async_trait]
pub trait ContentTypeDetector: Send + Sync {
    /// Identify the type of `bytes`. Returns `None` if detection could not
    /// produce any usable result — either because it found nothing, or
    /// because what it found has no registered parser (see the contract
    /// above) — as opposed to a low-confidence result, which is still
    /// returned so the caller can apply its own threshold.
    ///
    /// Takes an owned, cheaply-cloneable [`bytes::Bytes`] rather than `&[u8]`
    /// since the documented `spawn_blocking` offload strategy needs `'static`
    /// data; cloning a `Bytes` is a ref-count bump, not a buffer copy.
    async fn detect(&self, bytes: bytes::Bytes) -> Option<DetectedType>;

    /// Identify the file at `path` without the caller reading it into memory
    /// first. Same semantics as [`Self::detect`] otherwise.
    async fn detect_path(&self, path: &Path) -> Option<DetectedType>;
}
