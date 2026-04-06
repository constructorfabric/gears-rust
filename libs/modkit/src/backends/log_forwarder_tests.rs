use super::*;

#[test]
fn test_detect_log_level_tracing_subscriber_format() {
    // Real tracing-subscriber format examples
    assert_eq!(
        detect_log_level("2025-12-08T00:10:18.2852399Z  INFO hyperspot_server: shutdown"),
        Level::INFO
    );
    assert_eq!(
        detect_log_level(
            "2025-12-08T00:10:18.2861457Z DEBUG modkit::bootstrap::backends::local: Sending termination signal"
        ),
        Level::DEBUG
    );
    assert_eq!(
        detect_log_level("2025-12-08T00:10:18.2852399Z  WARN some_module: warning message"),
        Level::WARN
    );
    assert_eq!(
        detect_log_level("2025-12-08T00:10:18.2852399Z ERROR some_module: error message"),
        Level::ERROR
    );
    assert_eq!(
        detect_log_level("2025-12-08T00:10:18.2852399Z TRACE some_module: trace message"),
        Level::TRACE
    );
}

#[test]
fn test_detect_log_level_with_spans() {
    // tracing-subscriber with span context
    assert_eq!(
        detect_log_level(
            "2025-12-08T00:10:18.2864778Z DEBUG stop:stop: modkit::lifecycle: lifecycle task completed"
        ),
        Level::DEBUG
    );
    assert_eq!(
        detect_log_level(
            "2025-12-08T00:10:18.2865251Z  INFO stop:stop: modkit::lifecycle: lifecycle stopped"
        ),
        Level::INFO
    );
}

#[test]
fn test_detect_log_level_default() {
    // Lines without recognized level pattern default to INFO
    assert_eq!(detect_log_level("some random line"), Level::INFO);
    assert_eq!(detect_log_level(""), Level::INFO);
    assert_eq!(detect_log_level("Starting server..."), Level::INFO);
}

#[test]
fn test_detect_log_level_json_format() {
    // JSON format with uppercase level
    assert_eq!(
        detect_log_level(
            r#"{"timestamp":"2025-12-09T21:09:40.0028859Z","level":"INFO","fields":{"message":"test"},"target":"module"}"#
        ),
        Level::INFO
    );
    assert_eq!(
        detect_log_level(
            r#"{"timestamp":"2025-12-09T21:09:40.0028859Z","level":"DEBUG","fields":{"message":"test"},"target":"module"}"#
        ),
        Level::DEBUG
    );
    assert_eq!(
        detect_log_level(
            r#"{"timestamp":"2025-12-09T21:09:40.0028859Z","level":"WARN","fields":{"message":"test"},"target":"module"}"#
        ),
        Level::WARN
    );
    assert_eq!(
        detect_log_level(
            r#"{"timestamp":"2025-12-09T21:09:40.0028859Z","level":"ERROR","fields":{"message":"test"},"target":"module"}"#
        ),
        Level::ERROR
    );
    assert_eq!(
        detect_log_level(
            r#"{"timestamp":"2025-12-09T21:09:40.0028859Z","level":"TRACE","fields":{"message":"test"},"target":"module"}"#
        ),
        Level::TRACE
    );
}

#[test]
fn test_detect_log_level_json_format_lowercase() {
    // JSON format with lowercase level (some loggers use lowercase)
    assert_eq!(
        detect_log_level(r#"{"level":"info","message":"test"}"#),
        Level::INFO
    );
    assert_eq!(
        detect_log_level(r#"{"level":"debug","message":"test"}"#),
        Level::DEBUG
    );
    assert_eq!(
        detect_log_level(r#"{"level":"warn","message":"test"}"#),
        Level::WARN
    );
    assert_eq!(
        detect_log_level(r#"{"level":"error","message":"test"}"#),
        Level::ERROR
    );
}

#[test]
fn test_stream_kind_display() {
    assert_eq!(format!("{}", StreamKind::Stdout), "stdout");
    assert_eq!(format!("{}", StreamKind::Stderr), "stderr");
}
