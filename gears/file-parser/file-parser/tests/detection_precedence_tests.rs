#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Exercises `FileParserService`'s content-detection precedence logic against
//! a fake `ContentTypeDetector`, independent of the `magika` feature — no
//! real model/ONNX Runtime needed, so these run in every build.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;

use file_parser::Detection;
use file_parser::domain::detector::{Confidence, ContentTypeDetector, DetectedType};
use file_parser::domain::error::DomainError;
use file_parser::domain::parser::FileParserBackend;
use file_parser::domain::service::{FileParserService, ServiceConfig};
use file_parser::infra::parsers::{ImageParser, KreuzbergParser, PlainTextParser, StubParser};

/// Always returns the same canned detection result.
struct FixedDetector(Option<DetectedType>);

#[async_trait]
impl ContentTypeDetector for FixedDetector {
    async fn detect(&self, _bytes: Bytes) -> Option<DetectedType> {
        self.0.clone()
    }

    async fn detect_path(&self, _path: &std::path::Path) -> Option<DetectedType> {
        self.0.clone()
    }
}

/// One `parse_bytes` invocation, as the backend saw it.
struct ReceivedHints {
    #[allow(dead_code)] // captured for debugging; assertions target content_type
    filename: Option<String>,
    content_type: Option<String>,
}

/// Records the hints the service handed it, so a test can assert on what the
/// backend actually received rather than inferring it from parsed output.
struct RecordingParser {
    extensions: &'static [&'static str],
    received: std::sync::Mutex<Vec<ReceivedHints>>,
}

impl RecordingParser {
    fn new(extensions: &'static [&'static str]) -> Self {
        Self {
            extensions,
            received: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The `content_type` handed to the most recent `parse_bytes` call.
    fn last_content_type(&self) -> Option<String> {
        self.received
            .lock()
            .expect("no test panics while holding this lock")
            .last()
            .and_then(|hints| hints.content_type.clone())
    }
}

#[async_trait]
impl FileParserBackend for RecordingParser {
    fn id(&self) -> &'static str {
        "recording"
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        self.extensions
    }

    async fn parse_local_path(
        &self,
        _path: &std::path::Path,
        _resolved_content_type: Option<&str>,
    ) -> Result<file_parser::domain::ir::ParsedDocument, DomainError> {
        unimplemented!("this fake is only used on the parse_bytes path")
    }

    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        content_type: Option<&str>,
        _bytes: Bytes,
    ) -> Result<file_parser::domain::ir::ParsedDocument, DomainError> {
        self.received
            .lock()
            .expect("no test panics while holding this lock")
            .push(ReceivedHints {
                filename: filename_hint.map(ToOwned::to_owned),
                content_type: content_type.map(ToOwned::to_owned),
            });

        Ok(file_parser::domain::ir::DocumentBuilder::new(
            file_parser::domain::ir::ParsedSource::Uploaded {
                original_name: filename_hint.unwrap_or("unknown").to_owned(),
            },
        )
        .blocks(Vec::new())
        .build())
    }
}

fn detected(extension: &str, confidence: f32) -> DetectedType {
    DetectedType {
        extension: extension.to_owned(),
        confidence: Confidence::new(confidence).expect("test confidence must not be NaN"),
    }
}

/// Registers `PlainTextParser` (txt/log/md) and `StubParser` (doc/rtf/...)
/// so tests can tell which one handled a request from `meta.is_stub`.
fn service_with_detector(
    detected: Option<DetectedType>,
    base_dir: std::path::PathBuf,
) -> FileParserService {
    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![
        Arc::new(PlainTextParser::new()),
        Arc::new(StubParser::new()),
    ];
    let config = ServiceConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        allowed_local_base_dir: base_dir,
    };
    FileParserService::new(parsers, config).with_detector(Arc::new(FixedDetector(detected)))
}

fn service_without_detector(base_dir: std::path::PathBuf) -> FileParserService {
    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![Arc::new(PlainTextParser::new())];
    let config = ServiceConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        allowed_local_base_dir: base_dir,
    };
    FileParserService::new(parsers, config)
}

// ---------------------------------------------------------------------
// parse_bytes
// ---------------------------------------------------------------------

#[tokio::test]
async fn confident_detection_fills_in_missing_hint() {
    let svc = service_with_detector(Some(detected("txt", 0.99)), std::env::temp_dir());

    let doc = svc
        .parse_bytes(None, None, Bytes::from_static(b"hello"), Detection::Auto)
        .await
        .expect("confident detection should resolve a missing hint");

    assert!(
        !doc.meta.is_stub,
        "should have routed to PlainTextParser via detection, not the stub"
    );
}

#[tokio::test]
async fn confident_detection_overrides_a_wrong_extension_hint() {
    let svc = service_with_detector(Some(detected("txt", 0.99)), std::env::temp_dir());

    // The filename says `.doc` (routes to StubParser by extension alone),
    // but detection confidently says `txt`.
    let doc = svc
        .parse_bytes(
            Some("report.doc"),
            None,
            Bytes::from_static(b"hello"),
            Detection::Auto,
        )
        .await
        .expect("confident detection should override the wrong extension");

    assert!(
        !doc.meta.is_stub,
        "a confident detection must win over a present-but-wrong extension hint"
    );
}

#[tokio::test]
async fn a_raised_threshold_makes_a_previously_confident_detection_fall_back() {
    // Pin that the configurable threshold takes effect: a 0.95 detection wins
    // at the default and loses once the bar is raised above it.
    let detection = detected("txt", 0.95);

    let permissive = service_with_detector(Some(detection.clone()), std::env::temp_dir());
    let doc = permissive
        .parse_bytes(
            Some("report.doc"),
            None,
            Bytes::from_static(b"hello"),
            Detection::Auto,
        )
        .await
        .expect("0.95 clears the default 0.90 threshold");
    assert!(
        !doc.meta.is_stub,
        "at the default threshold, detection wins"
    );

    let strict = service_with_detector(Some(detection), std::env::temp_dir())
        .with_detection_confidence_threshold(
            Confidence::new(0.99).expect("0.99 is a valid confidence"),
        );
    let doc = strict
        .parse_bytes(
            Some("report.doc"),
            None,
            Bytes::from_static(b"hello"),
            Detection::Auto,
        )
        .await
        .expect("falling back to the .doc hint must still parse, via StubParser");
    assert!(
        doc.meta.is_stub,
        "raising the threshold above the detection's confidence must fall back to the hint"
    );
}

#[tokio::test]
async fn confidence_exactly_at_threshold_is_treated_as_confident() {
    // Mirrors the default threshold (`DEFAULT_DETECTION_CONFIDENCE_THRESHOLD`,
    // 0.90); the comparison is `>=`, so this exact value must win.
    let svc = service_with_detector(Some(detected("txt", 0.90)), std::env::temp_dir());

    let doc = svc
        .parse_bytes(
            Some("report.doc"),
            None,
            Bytes::from_static(b"hello"),
            Detection::Auto,
        )
        .await
        .expect("confidence exactly at the threshold should be treated as confident");

    assert!(
        !doc.meta.is_stub,
        "a detection exactly at the threshold must override the extension hint"
    );
}

#[tokio::test]
async fn low_confidence_detection_falls_back_to_the_hint() {
    let svc = service_with_detector(Some(detected("txt", 0.50)), std::env::temp_dir());

    let doc = svc
        .parse_bytes(
            Some("report.doc"),
            None,
            Bytes::from_static(b"hello"),
            Detection::Auto,
        )
        .await
        .expect("should fall back to the extension hint");

    assert!(
        doc.meta.is_stub,
        "below the confidence threshold, the caller-supplied hint must win"
    );
}

#[tokio::test]
async fn low_confidence_detection_with_no_hint_errors_like_before_detection_existed() {
    let svc = service_with_detector(Some(detected("txt", 0.10)), std::env::temp_dir());

    let err = svc
        .parse_bytes(None, None, Bytes::from_static(b"hello"), Detection::Auto)
        .await
        .expect_err("no usable hint and a low-confidence detection must still fail");

    assert!(matches!(
        err,
        DomainError::UnsupportedFileType { extension } if extension == "no extension and no content-type"
    ));
}

#[tokio::test]
async fn no_detection_result_falls_back_to_hint() {
    // Detectors return `None` rather than an extension with no matching
    // registered parser — simulate that directly here.
    let svc = service_with_detector(None, std::env::temp_dir());

    let doc = svc
        .parse_bytes(
            Some("notes.txt"),
            None,
            Bytes::from_static(b"hello"),
            Detection::Auto,
        )
        .await
        .expect("should fall back to the extension hint");

    assert!(!doc.meta.is_stub);
}

#[tokio::test]
async fn no_detection_result_and_unsupported_hint_yields_no_parser_available() {
    // An unmapped detection degrades to today's behavior: if the hint
    // itself isn't supported either, it's the pre-existing error, not a new one.
    let svc = service_with_detector(None, std::env::temp_dir());

    let err = svc
        .parse_bytes(
            Some("archive.zip"),
            None,
            Bytes::from_static(b"hello"),
            Detection::Auto,
        )
        .await
        .expect_err("an unsupported hint with no rescuing detection must still fail");

    assert!(matches!(
        err,
        DomainError::NoParserAvailable { extension } if extension == "zip"
    ));
}

#[tokio::test]
async fn no_detector_registered_behaves_exactly_as_before() {
    let svc = service_without_detector(std::env::temp_dir());

    let err = svc
        .parse_bytes(None, None, Bytes::from_static(b"hello"), Detection::Auto)
        .await
        .expect_err("no hint and no detector must fail exactly as before detection existed");

    assert!(matches!(
        err,
        DomainError::UnsupportedFileType { extension } if extension == "no extension and no content-type"
    ));
}

#[tokio::test]
async fn content_type_only_txt_upload_routes_without_detector() {
    // Regression test: the canonical MIME table must map `text/plain` to
    // `txt` so a `Content-Type`-only upload (no filename, no detector)
    // still resolves to `PlainTextParser`.
    let svc = service_without_detector(std::env::temp_dir());

    let doc = svc
        .parse_bytes(
            None,
            Some("text/plain"),
            Bytes::from_static(b"hello"),
            Detection::Auto,
        )
        .await
        .expect("a text/plain Content-Type with no filename should resolve to PlainTextParser");

    assert!(!doc.meta.is_stub);
}

#[tokio::test]
async fn content_type_only_xlsx_upload_routes_without_detector() {
    // Regression test: collapsing the three MIME tables into one canonical
    // table intentionally extends Content-Type-only routing to formats
    // (xlsx/xls/xlsm/xlsb/pptx) that were previously only resolvable via
    // filename extension at the gateway. This must keep working even with
    // no detector registered (i.e. without the `magika` feature).
    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![Arc::new(StubParser::new())];
    let config = ServiceConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        allowed_local_base_dir: std::env::temp_dir(),
    };
    let svc = FileParserService::new(parsers, config);

    let doc = svc
        .parse_bytes(
            None,
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            Bytes::from_static(b"hello"),
            Detection::Auto,
        )
        .await
        .expect("a Content-Type-only xlsx upload should resolve without a detector");

    assert!(doc.meta.is_stub);
}

#[tokio::test]
async fn detected_type_is_passed_to_the_backend_not_the_stale_hint() {
    // Regression test: the backend selected via the *detected* extension
    // must receive a `content_type` consistent with that extension, not the
    // original (missing/wrong) hint. `ImageParser::parse_bytes` requires a
    // `content_type` starting with `image/` or a filename extension to
    // determine its MIME type; with no filename and no `Content-Type`
    // hint, it fails unless the resolved extension's canonical MIME is
    // threaded through.
    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![Arc::new(ImageParser::new())];
    let config = ServiceConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        allowed_local_base_dir: std::env::temp_dir(),
    };
    let svc = FileParserService::new(parsers, config)
        .with_detector(Arc::new(FixedDetector(Some(detected("png", 0.99)))));

    let doc = svc
        .parse_bytes(
            None,
            None,
            Bytes::from_static(b"fake-png-bytes"),
            Detection::Auto,
        )
        .await
        .expect(
            "a confident png detection with no hint must route to ImageParser AND supply it \
             a usable content_type",
        );

    assert_eq!(doc.meta.content_type.as_deref(), Some("image/png"));
}

#[tokio::test]
async fn detected_html_overrides_a_wrong_extension_and_still_parses_via_kreuzberg() {
    // Real KreuzbergParser, not the hint-agnostic backends the other tests use:
    // detection must leave it able to extract, not just able to be selected.
    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![Arc::new(KreuzbergParser::new())];
    let config = ServiceConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        allowed_local_base_dir: std::env::temp_dir(),
    };
    let svc = FileParserService::new(parsers, config)
        .with_detector(Arc::new(FixedDetector(Some(detected("html", 0.99)))));

    let doc = svc
        .parse_bytes(
            Some("report.doc"),
            None,
            Bytes::from_static(b"<html><body><p>hello</p></body></html>"),
            Detection::Auto,
        )
        .await
        .expect("detection should route to KreuzbergParser and supply an extractable MIME");

    assert_eq!(doc.meta.content_type.as_deref(), Some("text/html"));
}

#[tokio::test]
async fn detected_extension_override_replaces_the_stale_caller_content_type() {
    // Regression test: when a confident detection overrides the caller's
    // hint extension, the caller's `Content-Type` describes the *old*
    // (wrong) type and must not be threaded through to the backend — the
    // canonical MIME for the *detected* extension must win instead. This is
    // the mirror case of `explicit_content_type_survives_a_conflicting_canonical_mime_for_the_extension`,
    // which only covers the no-detector / hint-wins path.
    let recorder = Arc::new(RecordingParser::new(&["html", "htm"]));
    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![recorder.clone()];
    let config = ServiceConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        allowed_local_base_dir: std::env::temp_dir(),
    };
    let svc = FileParserService::new(parsers, config)
        .with_detector(Arc::new(FixedDetector(Some(detected("html", 0.99)))));

    svc.parse_bytes(
        Some("notes.txt"),
        Some("text/plain"),
        Bytes::from_static(b"<html></html>"),
        Detection::Auto,
    )
    .await
    .expect("the detected html extension must route to the recording backend");

    assert_eq!(
        recorder.last_content_type().as_deref(),
        Some("text/html"),
        "once detection overrides the .txt hint, the stale text/plain Content-Type must not \
         reach the backend, it must be replaced by the detected extension's canonical MIME"
    );
}

#[tokio::test]
async fn explicit_content_type_survives_a_conflicting_canonical_mime_for_the_extension() {
    // An explicitly supplied Content-Type must win over a value derived from
    // the filename's extension — the filename is the weaker signal. The
    // table only fills in a *missing* Content-Type (see
    // `content_type_only_txt_upload_routes_without_detector` et al.). No
    // detector here, so this is the default build.
    let recorder = Arc::new(RecordingParser::new(&["html", "htm"]));
    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![recorder.clone()];
    let config = ServiceConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        allowed_local_base_dir: std::env::temp_dir(),
    };
    let svc = FileParserService::new(parsers, config);

    svc.parse_bytes(
        Some("report.html"),
        Some("text/plain"),
        Bytes::from_static(b"<html></html>"),
        Detection::Auto,
    )
    .await
    .expect("the html extension must route to the recording backend");

    assert_eq!(
        recorder.last_content_type().as_deref(),
        Some("text/plain"),
        "an explicitly supplied Content-Type must not be silently replaced by the canonical \
         MIME for `html`"
    );
}

#[tokio::test]
async fn caller_content_type_survives_when_the_extension_is_not_in_the_canonical_table() {
    // The other half: with no canonical entry for `rtf`, the caller's
    // Content-Type passes through untouched regardless.
    let recorder = Arc::new(RecordingParser::new(&["rtf"]));
    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![recorder.clone()];
    let config = ServiceConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        allowed_local_base_dir: std::env::temp_dir(),
    };
    let svc = FileParserService::new(parsers, config);

    svc.parse_bytes(
        Some("notes.rtf"),
        Some("application/rtf"),
        Bytes::from_static(b"{\\rtf1}"),
        Detection::Auto,
    )
    .await
    .expect("the rtf extension must route to the recording backend");

    assert_eq!(
        recorder.last_content_type().as_deref(),
        Some("application/rtf"),
        "with no canonical entry for `rtf`, the caller's Content-Type must be preserved"
    );
}

#[tokio::test]
async fn skip_detection_bypasses_a_registered_detector_and_routes_by_hint() {
    // A caller that already knows the exact type (in-process/SDK path) can
    // opt out of detection entirely, even though a detector is registered
    // and would otherwise confidently override the hint. `doc` is
    // registered (StubParser) so, without `Detection::Skip`, this exact
    // detection would win per `confident_detection_overrides_a_wrong_extension_hint`.
    let svc = service_with_detector(Some(detected("doc", 0.99)), std::env::temp_dir());

    let doc = svc
        .parse_bytes(
            Some("notes.txt"),
            None,
            Bytes::from_static(b"hello"),
            Detection::Skip,
        )
        .await
        .expect("Detection::Skip must still route via the hint");

    assert!(
        !doc.meta.is_stub,
        "Detection::Skip must route by the .txt hint, ignoring the confident doc detection"
    );
}

// ---------------------------------------------------------------------
// parse_local
// ---------------------------------------------------------------------

#[tokio::test]
async fn extensionless_local_file_resolved_via_confident_detection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("mystery");
    std::fs::write(&path, b"hello").expect("write temp file");

    let svc = service_with_detector(
        Some(detected("txt", 0.99)),
        tmp.path().canonicalize().expect("canonicalize tempdir"),
    );

    let doc = svc
        .parse_local(&path)
        .await
        .expect("extensionless file should resolve via detection");

    assert!(!doc.meta.is_stub);
}

#[tokio::test]
async fn extensionless_local_file_without_a_detector_still_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("mystery");
    std::fs::write(&path, b"hello").expect("write temp file");

    let svc = service_without_detector(tmp.path().canonicalize().expect("canonicalize tempdir"));

    let err = svc
        .parse_local(&path)
        .await
        .expect_err("no extension and no detector must fail exactly as before detection existed");

    assert!(matches!(err, DomainError::UnsupportedFileType { .. }));
}

#[tokio::test]
async fn extensionless_local_file_with_low_confidence_detection_still_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("mystery");
    std::fs::write(&path, b"hello").expect("write temp file");

    let svc = service_with_detector(
        Some(detected("txt", 0.10)),
        tmp.path().canonicalize().expect("canonicalize tempdir"),
    );

    let err = svc
        .parse_local(&path)
        .await
        .expect_err("a low-confidence detection must not rescue an extensionless file");

    assert!(matches!(err, DomainError::UnsupportedFileType { .. }));
}

#[tokio::test]
async fn confident_detection_overrides_a_wrong_extension_for_local_files() {
    // Regression test: a local file with a present-but-wrong extension must
    // still be corrected by a confident detection, matching parse_bytes's
    // precedence — not routed by the extension unconditionally.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("report.doc");
    std::fs::write(&path, b"hello").expect("write temp file");

    let svc = service_with_detector(
        Some(detected("txt", 0.99)),
        tmp.path().canonicalize().expect("canonicalize tempdir"),
    );

    let doc = svc
        .parse_local(&path)
        .await
        .expect("confident detection should override the wrong extension");

    assert!(
        !doc.meta.is_stub,
        "a confident detection must win over a present-but-wrong extension for local files too"
    );
}

#[tokio::test]
async fn low_confidence_detection_falls_back_to_the_extension_for_local_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("report.doc");
    std::fs::write(&path, b"hello").expect("write temp file");

    let svc = service_with_detector(
        Some(detected("txt", 0.10)),
        tmp.path().canonicalize().expect("canonicalize tempdir"),
    );

    let doc = svc
        .parse_local(&path)
        .await
        .expect("should fall back to the extension hint");

    assert!(
        doc.meta.is_stub,
        "below the confidence threshold, the file's own extension must win"
    );
}

#[tokio::test]
async fn detected_type_is_passed_to_the_backend_for_local_files_too() {
    // parse_local counterpart to detected_type_is_passed_to_the_backend_not_the_stale_hint.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("report.doc");
    std::fs::write(&path, b"fake-png-bytes").expect("write temp file");

    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![Arc::new(ImageParser::new())];
    let config = ServiceConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        allowed_local_base_dir: tmp.path().canonicalize().expect("canonicalize tempdir"),
    };
    let svc = FileParserService::new(parsers, config)
        .with_detector(Arc::new(FixedDetector(Some(detected("png", 0.99)))));

    let doc = svc
        .parse_local(&path)
        .await
        .expect("detection should route to ImageParser and supply a usable content_type");

    assert_eq!(doc.meta.content_type.as_deref(), Some("image/png"));
}

#[tokio::test]
async fn local_file_over_the_size_limit_is_rejected_even_without_a_detector() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("big.txt");
    std::fs::write(&path, vec![b'a'; 20]).expect("write temp file");

    let parsers: Vec<Arc<dyn FileParserBackend>> = vec![Arc::new(PlainTextParser::new())];
    let config = ServiceConfig {
        max_file_size_bytes: 10,
        allowed_local_base_dir: tmp.path().canonicalize().expect("canonicalize tempdir"),
    };
    let svc = FileParserService::new(parsers, config);

    let err = svc
        .parse_local(&path)
        .await
        .expect_err("a local file over the configured size limit must be rejected");

    assert!(matches!(err, DomainError::InvalidRequest { .. }));
}
