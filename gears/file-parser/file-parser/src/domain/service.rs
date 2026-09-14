use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use file_parser_sdk::Detection;
use toolkit_macros::domain_model;
use tracing::{debug, info, instrument, warn};

use crate::domain::detector::{Confidence, ContentTypeDetector, DetectedType};
use crate::domain::error::DomainError;
use crate::domain::ir::ParsedDocument;
use crate::domain::mime_table;
use crate::domain::parser::FileParserBackend;

/// Default minimum confidence at which a detected type overrides the
/// caller-supplied filename extension / `Content-Type`.
///
/// An initial guess rather than a tuned value, so it is overridable per
/// deployment — see `FileParserConfig::detection_confidence_threshold`.
pub const DEFAULT_DETECTION_CONFIDENCE_THRESHOLD: f32 = 0.90;

/// File parser service that routes to appropriate backends
#[domain_model]
#[derive(Clone)]
pub struct FileParserService {
    parsers: Vec<Arc<dyn FileParserBackend>>,
    config: ServiceConfig,
    /// Optional content-based type detector. `None` unless a detector was
    /// registered via [`FileParserService::with_detector`] — in particular,
    /// always `None` when the gear is built without the `magika` feature, in
    /// which case routing behaves exactly as it did before this field
    /// existed.
    detector: Option<Arc<dyn ContentTypeDetector>>,
    /// Minimum confidence at which a detection may override the caller's hint.
    /// Defaults to [`DEFAULT_DETECTION_CONFIDENCE_THRESHOLD`]; irrelevant when
    /// no detector is registered.
    detection_confidence_threshold: Confidence,
}

/// Configuration for the file parser service
#[domain_model]
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub max_file_size_bytes: usize,
    /// Canonicalized base directory for local file access. Only paths that
    /// start with this prefix are allowed by `parse_local`.
    pub allowed_local_base_dir: PathBuf,
}

/// Information about available parsers
#[domain_model]
#[derive(Debug, Clone)]
pub struct FileParserInfo {
    pub supported_extensions: std::collections::HashMap<String, Vec<String>>,
}

impl FileParserService {
    /// Create a new service with the given parsers.
    ///
    /// No content-type detector is registered; routing relies solely on the
    /// filename extension / `Content-Type` hint. Use
    /// [`FileParserService::with_detector`] to opt in.
    #[must_use]
    pub fn new(parsers: Vec<Arc<dyn FileParserBackend>>, config: ServiceConfig) -> Self {
        Self {
            parsers,
            config,
            detector: None,
            #[allow(clippy::expect_used)] // A const in [0.0, 1.0]; cannot fail.
            detection_confidence_threshold: Confidence::new(
                DEFAULT_DETECTION_CONFIDENCE_THRESHOLD,
            )
            .expect("the default detection threshold is a valid confidence"),
        }
    }

    /// Register a content-based type detector, used to resolve or correct
    /// the routing extension when the caller-supplied hint is missing or
    /// wrong.
    #[must_use]
    pub fn with_detector(mut self, detector: Arc<dyn ContentTypeDetector>) -> Self {
        self.detector = Some(detector);
        self
    }

    /// Override the minimum confidence at which a detection takes routing
    /// priority over the caller's hint.
    ///
    /// Takes a [`Confidence`], not an `f32`, so out-of-range and `NaN` are
    /// rejected before they get here.
    #[must_use]
    pub fn with_detection_confidence_threshold(mut self, threshold: Confidence) -> Self {
        self.detection_confidence_threshold = threshold;
        self
    }

    /// Get information about available parsers
    #[instrument(skip(self))]
    pub fn info(&self) -> FileParserInfo {
        debug!("Getting parser info");

        let mut supported_extensions = std::collections::HashMap::new();

        for parser in &self.parsers {
            let id = parser.id();
            let extensions: Vec<String> = parser
                .supported_extensions()
                .iter()
                .map(ToString::to_string)
                .collect();
            supported_extensions.insert(id.to_owned(), extensions);
        }

        FileParserInfo {
            supported_extensions,
        }
    }

    /// Parse a file from a local path.
    ///
    /// The requested path is validated before any file-system access:
    /// 1. `..` path components are rejected outright.
    /// 2. The path is canonicalized (resolving symlinks).
    /// 3. The canonical path must fall under `allowed_local_base_dir`.
    #[instrument(skip(self), fields(path = %path.display()))]
    pub async fn parse_local(&self, path: &Path) -> Result<ParsedDocument, DomainError> {
        info!("Parsing file from local path");

        // --- Path traversal protection ---
        // Order matters: validate before any filesystem probe so that
        // unauthorised paths never leak existence information.
        Self::validate_local_path(path)?;

        // Canonicalize to resolve symlinks. This also serves as the
        // existence check — canonicalize fails with NotFound on missing paths.
        let canonical = path.canonicalize().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DomainError::file_not_found(path.display().to_string())
            } else {
                DomainError::io_error(format!(
                    "Cannot canonicalize path '{}': {e}",
                    path.display()
                ))
            }
        })?;

        // Enforce base directory (after symlink resolution).
        // This runs before any content is read, so an attacker probing
        // paths outside the base dir gets a uniform 403 regardless of
        // whether the path exists.
        if !canonical.starts_with(&self.config.allowed_local_base_dir) {
            warn!(
                requested = %path.display(),
                canonical = %canonical.display(),
                base_dir = %self.config.allowed_local_base_dir.display(),
                "Path traversal blocked: canonical path outside allowed base directory"
            );
            return Err(DomainError::path_traversal_blocked(format!(
                "Access denied: '{}' is outside the allowed base directory",
                path.display()
            )));
        }

        // Unconditional, so local files can't bypass the limit. Rejects early
        // and names the actual size; the memory bound is the backends' own
        // bounded reads, since metadata-then-read is not atomic.
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .map_err(|e| DomainError::io_error(format!("Cannot read file metadata: {e}")))?;
        self.check_size_limit(metadata.len())?;

        // Extract the filename-hint extension, then apply the same
        // detection precedence `parse_bytes` uses: run content-based
        // detection whenever a detector is registered — not only when the
        // extension is missing — so a confident detection can also correct
        // a present-but-wrong extension. With no detector registered, this
        // reads no file content and behaves exactly as before detection
        // existed.
        let extension_from_name = canonical
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_owned);

        let extension = if self.detector.is_some() {
            let detected = self.detect_from_path(&canonical).await;
            match self.reconcile_extension(extension_from_name.as_deref(), detected.as_ref()) {
                Some(ext) => ext,
                None => return Err(DomainError::unsupported_file_type("no extension")),
            }
        } else if let Some(ext) = extension_from_name {
            ext
        } else {
            return Err(DomainError::unsupported_file_type("no extension"));
        };

        // Find parser
        let parser = self
            .find_parser_by_extension(&extension)
            .ok_or_else(|| DomainError::no_parser_available(&extension))?;

        // Hand the backend the resolved MIME, not just use it to pick the
        // backend: otherwise a wrongly-named file still fails once the backend
        // re-derives its own MIME from the on-disk extension.
        let resolved_content_type = mime_table::mime_for_extension(&extension);

        // Parse the file
        let document = parser
            .parse_local_path(&canonical, resolved_content_type)
            .await
            .map_err(|e| {
                tracing::error!(?e, "FileParserService: parse_local failed");
                e
            })?;

        debug!("Successfully parsed file from local path");
        Ok(document)
    }

    /// Reject paths that contain `..` components (before any file-system call).
    fn validate_local_path(path: &Path) -> Result<(), DomainError> {
        for component in path.components() {
            if matches!(component, std::path::Component::ParentDir) {
                warn!(
                    path = %path.display(),
                    "Path traversal blocked: '..' component detected"
                );
                return Err(DomainError::path_traversal_blocked(format!(
                    "Access denied: path '{}' contains '..' traversal component",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    /// Parse a file from bytes.
    ///
    /// `detection` controls whether content-based detection runs even when
    /// a detector is registered — `Detection::Skip` pins routing to
    /// `filename_hint` / `content_type` for a caller that already knows the
    /// exact type it's passing — see
    /// [`FileParserService::reconcile_extension`].
    #[instrument(
        skip(self, bytes),
        fields(
            filename_hint = ?filename_hint,
            content_type = ?content_type,
            size = bytes.len(),
            ?detection
        )
    )]
    pub async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        content_type: Option<&str>,
        bytes: Bytes,
        detection: Detection,
    ) -> Result<ParsedDocument, DomainError> {
        info!("Parsing uploaded file");

        // Check file size
        self.check_size_limit(bytes.len() as u64)?;

        // Determine the caller-supplied hint by priority:
        // 1. From filename (if provided and has extension)
        // 2. From Content-Type (if provided and recognized)
        // `no_hint_reason` preserves today's exact error text for the case
        // where hint resolution fails outright and detection doesn't save it.
        let extension_from_name = filename_hint
            .and_then(|name| Path::new(name).extension())
            .and_then(|s| s.to_str())
            .map(str::to_owned);

        let (hint_extension, no_hint_reason) = if let Some(ext) = extension_from_name {
            (Some(ext), None)
        } else if let Some(ct) = content_type {
            match Self::extension_from_content_type(ct) {
                Some(ext) => (Some(ext), None),
                None => (None, Some("no extension and unknown content-type")),
            }
        } else {
            (None, Some("no extension and no content-type"))
        };

        // Run content-based detection on every request when a detector is
        // registered — this is what lets a confident detection correct a
        // wrong hint, not just fill in a missing one. With no detector
        // registered (including always, when built without the `magika`
        // feature), or when the caller opted out via `Detection::Skip`,
        // this is a no-op and behavior is unchanged.
        let detected = match &self.detector {
            Some(detector) if detection == Detection::Auto => {
                Self::detect_and_log("uploaded_bytes", detector.detect(bytes.clone())).await
            }
            _ => None,
        };

        let Some(extension) =
            self.reconcile_extension(hint_extension.as_deref(), detected.as_ref())
        else {
            return Err(DomainError::unsupported_file_type(
                no_hint_reason.unwrap_or("no extension and no content-type"),
            ));
        };

        // Find parser
        let parser = self
            .find_parser_by_extension(&extension)
            .ok_or_else(|| DomainError::no_parser_available(&extension))?;

        // True when detection replaced the caller's hint, so the caller's
        // `Content-Type` describes a different type than the selected parser.
        let detection_won = hint_extension
            .as_deref()
            .is_none_or(|hint| !hint.eq_ignore_ascii_case(&extension));

        // Prefer the caller's explicit Content-Type while the routing
        // extension still comes from the caller's own hint. Once a confident
        // detection has overridden that hint, the caller's Content-Type
        // describes the wrong type, so the detected extension's canonical
        // MIME wins instead.
        let resolved_content_type = if detection_won {
            mime_table::mime_for_extension(&extension).or(content_type)
        } else {
            content_type.or_else(|| mime_table::mime_for_extension(&extension))
        };

        // Parse the file
        let document = parser
            .parse_bytes(filename_hint, resolved_content_type, bytes)
            .await
            .map_err(|e| {
                tracing::error!(?e, "FileParserService: parse_bytes failed");
                e
            })?;

        debug!("Successfully parsed uploaded file");
        Ok(document)
    }

    /// Extract file extension from Content-Type header
    #[must_use]
    pub fn extension_from_content_type(ct: &str) -> Option<String> {
        let mime: mime::Mime = ct.parse().ok()?;
        mime_table::extension_for_mime(mime.essence_str()).map(ToOwned::to_owned)
    }

    /// Reconciles a caller-supplied extension hint with a content-detection
    /// result. A detection at or above `detection_confidence_threshold` with a
    /// matching registered parser wins routing, logging any disagreement;
    /// otherwise the hint is used unchanged.
    ///
    /// The parser check matters because `with_detector` is public API: a
    /// third-party detector reporting an unregistered extension must not turn a
    /// hint-satisfiable request into `no_parser_available`.
    fn reconcile_extension(
        &self,
        hint: Option<&str>,
        detected: Option<&DetectedType>,
    ) -> Option<String> {
        if let Some(detected) = detected
            && detected.confidence >= self.detection_confidence_threshold
            && self.find_parser_by_extension(&detected.extension).is_some()
        {
            if let Some(hint) = hint
                && !hint.eq_ignore_ascii_case(&detected.extension)
            {
                warn!(
                    hinted_extension = hint,
                    detected_extension = %detected.extension,
                    confidence = %detected.confidence,
                    "file-parser: caller-supplied type hint disagrees with detected content \
                     type; routing by detected type"
                );
            }
            return Some(detected.extension.clone());
        }
        hint.map(str::to_owned)
    }

    /// Times `run` and logs its outcome at `debug`. Generic over the future so
    /// both detection paths share it; `source` is what tells them apart in logs.
    ///
    /// Spanned because `debug` is normally off in production, and detection is a
    /// synchronous inference on the request path whose latency has to be
    /// attributable without one.
    #[instrument(name = "file_parser.detect", skip(run))]
    async fn detect_and_log<F>(source: &'static str, run: F) -> Option<DetectedType>
    where
        F: std::future::Future<Output = Option<DetectedType>>,
    {
        let started = std::time::Instant::now();
        let result = run.await;
        let elapsed_ms = started.elapsed().as_millis();

        if let Some(detected) = &result {
            debug!(
                source,
                extension = %detected.extension,
                confidence = %detected.confidence,
                elapsed_ms,
                "file-parser: content detection ran"
            );
        } else {
            debug!(
                source,
                elapsed_ms, "file-parser: content detection ran, no result"
            );
        }

        result
    }

    /// Detects via [`ContentTypeDetector::detect_path`] rather than reading the
    /// file in and calling `detect`, which would buffer the whole file and
    /// duplicate the backend's own read. `None` if no detector is registered.
    //
    // NOT cancel-safe: see `MagikaDetector::detect_path`.
    async fn detect_from_path(&self, path: &Path) -> Option<DetectedType> {
        let detector = self.detector.as_ref()?;
        Self::detect_and_log("local_path", detector.detect_path(path)).await
    }

    /// Rejects `len` if it exceeds the configured `max_file_size_bytes`.
    ///
    /// Shared by `parse_bytes` and `parse_local` so the limit and its message
    /// cannot drift between entry points, or with the `magika` feature.
    fn check_size_limit(&self, len: u64) -> Result<(), DomainError> {
        if len > self.config.max_file_size_bytes as u64 {
            return Err(DomainError::invalid_request(format!(
                "File size {len} exceeds maximum of {} bytes",
                self.config.max_file_size_bytes
            )));
        }
        Ok(())
    }

    /// Find a parser by file extension
    fn find_parser_by_extension(&self, ext: &str) -> Option<Arc<dyn FileParserBackend>> {
        let ext_lower = ext.to_lowercase();
        self.parsers
            .iter()
            .find(|p| {
                p.supported_extensions()
                    .iter()
                    .any(|e| e.to_lowercase() == ext_lower)
            })
            .cloned()
    }
}
