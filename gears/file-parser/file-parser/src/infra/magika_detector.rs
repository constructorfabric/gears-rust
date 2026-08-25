//! Magika-backed [`ContentTypeDetector`], compiled only behind the `magika`
//! feature.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::domain::detector::{Confidence, ContentTypeDetector, DetectedType};

/// Wraps a single `magika::Session` and restricts Magika's ~200 output labels
/// to the extensions this gear's registered parsers actually declare. Magika's
/// own `TypeInfo` already lists each label's extensions, so we just intersect
/// that with what's registered.
///
/// # Why one session and not a pool
///
/// `magika::Session` exposes only `&mut self` inference methods and keeps its
/// inner `ort::session::Session` private, so every inference needs exclusive
/// access however the sessions are held — `ort`'s session is itself
/// `Send + Sync`, but that is not reachable through `magika`. So extra sessions
/// buy concurrency only at the price of one ONNX intra-op thread pool and one
/// resident model copy each (~6 extra threads per session on a 10-core host).
///
/// Not worth it: detection measures ~3.5 ms, against document extraction costing
/// tens to hundreds of milliseconds on the same request. One serialized session
/// sustains ~285 detections/second. Tune [`Self::with_config`]'s
/// `intra_op_threads` to move detection's CPU use instead.
///
/// Requests queue on a fair FIFO mutex, so at most one inference is in flight
/// and at most one blocking thread is occupied.
pub struct MagikaDetector {
    /// `Option` so an inference that panics leaves the slot empty rather than
    /// leaving behind a session whose ONNX state is unknown; the next caller
    /// rebuilds it. `Arc` so the guard can be moved into `spawn_blocking`.
    session: Arc<Mutex<Option<magika::Session>>>,
    supported_extensions: Arc<HashSet<String>>,
    /// Kept so a session rebuilt after a panic gets the same thread config as
    /// the one built at startup.
    intra_op_threads: Option<NonZeroUsize>,
}

impl MagikaDetector {
    /// Loads the embedded model and builds the session, leaving ONNX
    /// Runtime's own thread defaults in place. See [`Self::with_config`].
    pub fn new(
        supported_extensions: impl IntoIterator<Item = impl Into<String>>,
    ) -> magika::Result<Self> {
        Self::with_config(supported_extensions, None)
    }

    /// Loads the embedded model and builds the session. `intra_op_threads` is
    /// ONNX Runtime's per-inference thread count; `None` keeps its default —
    /// not the host core count, ~6 threads on a 10-core host. See
    /// `FileParserConfig::magika_intra_op_threads`.
    ///
    /// Eager and fallible on purpose: a missing model or runtime must fail gear
    /// startup, not degrade silently.
    ///
    /// Must not be called on the async runtime — against an unloadable ONNX
    /// Runtime this hangs rather than erroring. See `init_magika_detector`.
    ///
    /// `supported_extensions` should be every registered backend's
    /// `supported_extensions()`; a label outside that set is "no detection".
    pub fn with_config(
        supported_extensions: impl IntoIterator<Item = impl Into<String>>,
        intra_op_threads: Option<NonZeroUsize>,
    ) -> magika::Result<Self> {
        Ok(Self {
            session: Arc::new(Mutex::new(Some(Self::build_session(intra_op_threads)?))),
            supported_extensions: Arc::new(
                supported_extensions
                    .into_iter()
                    .map(|ext| ext.into().to_lowercase())
                    .collect(),
            ),
            intra_op_threads,
        })
    }

    fn build_session(intra_op_threads: Option<NonZeroUsize>) -> magika::Result<magika::Session> {
        let mut builder = magika::Session::builder();
        if let Some(threads) = intra_op_threads {
            builder = builder.with_intra_threads(threads.get());
        }
        builder.build()
    }
}

/// Turns a raw inference outcome into a [`DetectedType`], swallowing an `Err`,
/// an unmapped label (the routine "detection did not help" case) and a `NaN`
/// score rather than propagating them. Split out so it can be unit-tested with
/// synthetic values, without a real `Session` or ONNX Runtime.
fn map_inference_result(
    result: magika::Result<magika::FileType>,
    supported_extensions: &HashSet<String>,
) -> Option<DetectedType> {
    let file_type = result
        // Neutral wording: this serves both `detect` (`identify_content_sync`)
        // and `detect_path` (`identify_file_sync`).
        .inspect_err(|e| warn!(error = %e, "magika inference failed"))
        .ok()?;

    let info = file_type.info();
    let Some(extension) = info
        .extensions
        .iter()
        .find(|ext| supported_extensions.contains(&ext.to_lowercase()))
    else {
        debug!(
            label = info.label,
            candidate_extensions = ?info.extensions,
            "magika: detected label maps to no registered extension; falling back to hint"
        );
        return None;
    };

    let score = file_type.score();
    let Some(confidence) = Confidence::new(score) else {
        warn!(
            score,
            label = info.label,
            "magika: inference returned a NaN score"
        );
        return None;
    };

    Some(DetectedType {
        extension: extension.to_lowercase(),
        confidence,
    })
}

impl MagikaDetector {
    /// Takes exclusive use of the session and runs `infer` on the blocking pool.
    /// Shared by `detect`/`detect_path`, which differ only in which
    /// `identify_*_sync` they call.
    ///
    /// The session never leaves the mutex; the *guard* moves into the closure.
    /// That is what makes cancellation safe — the guard is released by `Drop`
    /// when the blocking task ends, whether or not anyone still awaits the
    /// result, so a dropped future cannot strand or destroy the session.
    #[tracing::instrument(
        name = "magika.inference",
        skip(self, infer),
        fields(intra_op_threads = ?self.intra_op_threads)
    )]
    async fn run_inference(
        &self,
        infer: impl FnOnce(&mut magika::Session) -> magika::Result<magika::FileType> + Send + 'static,
    ) -> Option<DetectedType> {
        // Fair FIFO queueing, on the async side rather than on a blocking
        // thread. `lock_owned` because `spawn_blocking` needs `'static`.
        let mut guard = Arc::clone(&self.session).lock_owned().await;

        if guard.is_none() {
            // A previous inference panicked and left the slot empty.
            *guard = Self::rebuild_session(self.intra_op_threads).await;
            if guard.is_none() {
                return None;
            }
        }

        let supported_extensions = Arc::clone(&self.supported_extensions);
        tokio::task::spawn_blocking(move || {
            let mut guard = guard;
            // `take` so a panic inside `infer` leaves the slot empty instead
            // of leaving a session whose ONNX state is unknown after unwind.
            let mut session = guard.take()?;
            let result = map_inference_result(infer(&mut session), &supported_extensions);
            *guard = Some(session);
            result
        })
        .await
        .unwrap_or_else(|join_err| {
            warn!(error = %join_err, "magika inference task panicked; session will be rebuilt");
            None
        })
    }

    /// Builds a fresh session on the blocking pool, swallowing either failure
    /// mode: detection degrades to "no detection", which the service already
    /// handles by falling back to the caller's hint.
    ///
    /// Safe to build directly here, unlike at startup: the hang only affects a
    /// first, failing runtime load, and reaching this code means one already
    /// succeeded.
    async fn rebuild_session(intra_op_threads: Option<NonZeroUsize>) -> Option<magika::Session> {
        match tokio::task::spawn_blocking(move || Self::build_session(intra_op_threads)).await {
            Ok(Ok(fresh_session)) => Some(fresh_session),
            Ok(Err(e)) => {
                warn!(error = %e, "failed to rebuild the magika session");
                None
            }
            Err(join_err) => {
                warn!(error = %join_err, "rebuilding the magika session panicked");
                None
            }
        }
    }
}

#[async_trait]
impl ContentTypeDetector for MagikaDetector {
    // Cancel-safe with respect to the session: dropping the future does not
    // stop the in-flight inference (its result is simply discarded), but the
    // session is released back for the next caller either way, because the
    // blocking task owns the mutex guard and drops it on completion.
    async fn detect(&self, bytes: bytes::Bytes) -> Option<DetectedType> {
        self.run_inference(move |session| session.identify_content_sync(bytes.as_ref()))
            .await
    }

    // Cancel-safe with respect to the session: see `detect` above.
    async fn detect_path(&self, path: &std::path::Path) -> Option<DetectedType> {
        let path = path.to_path_buf();
        self.run_inference(move |session| session.identify_file_sync(&path))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inferred(content_type: magika::ContentType, score: f32) -> magika::FileType {
        magika::FileType::Inferred(magika::InferredType {
            content_type: None,
            inferred_type: content_type,
            score,
        })
    }

    #[test]
    fn inference_error_is_swallowed_to_none() {
        let err = magika::Error::IOError(std::io::Error::other("simulated inference failure"));

        assert!(map_inference_result(Err(err), &HashSet::new()).is_none());
    }

    #[test]
    fn label_present_in_supported_extensions_maps_to_detected_type() {
        let supported: HashSet<String> = ["pdf", "html"].into_iter().map(str::to_owned).collect();

        let detected =
            map_inference_result(Ok(inferred(magika::ContentType::Pdf, 0.97)), &supported)
                .expect("pdf is in supported_extensions, so this must resolve");

        assert_eq!(detected.extension, "pdf");
        assert!((detected.confidence.get() - 0.97).abs() < f32::EPSILON);
    }

    #[test]
    fn label_absent_from_supported_extensions_returns_none() {
        // No registered backend declares "pdf" here.
        let supported: HashSet<String> = ["html"].into_iter().map(str::to_owned).collect();

        assert!(
            map_inference_result(Ok(inferred(magika::ContentType::Pdf, 0.97)), &supported)
                .is_none()
        );
    }

    #[test]
    fn nan_score_returns_none() {
        let supported: HashSet<String> = ["pdf"].into_iter().map(str::to_owned).collect();

        assert!(
            map_inference_result(Ok(inferred(magika::ContentType::Pdf, f32::NAN)), &supported)
                .is_none()
        );
    }

    #[test]
    fn label_extension_is_lowercased() {
        let supported: HashSet<String> = ["PDF".to_lowercase()].into_iter().collect();

        let detected =
            map_inference_result(Ok(inferred(magika::ContentType::Pdf, 0.5)), &supported)
                .expect("lowercased supported_extensions must still match");

        assert_eq!(detected.extension, "pdf");
    }
}
