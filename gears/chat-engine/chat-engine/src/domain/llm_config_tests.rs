use super::*;

#[test]
fn validate_plugin_config_happy_path() {
    let blob = serde_json::json!({
        "gateway_url": "https://gw.example/v1",
    });
    let cfg = validate_plugin_config(&blob).expect("valid config");
    assert_eq!(cfg.gateway_url, "https://gw.example/v1");
    assert!(cfg.default_model.is_none());
    assert!(!cfg.summarization_enabled());
    assert_eq!(cfg.effective_retry_count(), DEFAULT_RETRY_COUNT);
    assert_eq!(
        cfg.effective_retry_delay(),
        Duration::from_millis(u64::from(DEFAULT_RETRY_DELAY_MS))
    );
}

#[test]
fn validate_plugin_config_with_summarization_defaults() {
    let blob = serde_json::json!({
        "gateway_url": "https://gw.example",
        "summarization_settings": {},
    });
    let cfg = validate_plugin_config(&blob).expect("valid config");
    let s = cfg.summarization_settings.expect("settings present");
    assert_eq!(s.recent_messages_to_keep, RECENT_MESSAGES_TO_KEEP_DEFAULT);
}

#[test]
fn validate_plugin_config_rejects_empty_gateway_url() {
    let blob = serde_json::json!({
        "gateway_url": "  ",
    });
    let err = validate_plugin_config(&blob).expect_err("empty gateway_url");
    assert!(err.to_string().contains("gateway_url"));
}

#[test]
fn validate_plugin_config_rejects_low_recent_messages_to_keep() {
    let blob = serde_json::json!({
        "gateway_url": "https://gw.example",
        "summarization_settings": { "recent_messages_to_keep": 1 },
    });
    let err = validate_plugin_config(&blob).expect_err("violates min");
    assert!(err.to_string().contains("recent_messages_to_keep"));
}

#[test]
fn validate_plugin_config_preserves_source_chain() {
    use std::error::Error;
    // Wrong-shape `gateway_url` (number, not string) — serde fails with
    // a typed error; the plugin error MUST keep it attached.
    let blob = serde_json::json!({ "gateway_url": 42 });
    let err = validate_plugin_config(&blob).expect_err("malformed blob");
    let source = err.source().expect("source preserved");
    assert!(source.to_string().to_ascii_lowercase().contains("string"));
}

#[test]
fn summarization_settings_keep_count_clamps_to_min() {
    // The struct deserializer accepts any u32; `keep_count()` clamps to
    // the lower bound so callers cannot accidentally slice off the
    // entire history.
    let s = LlmSummarizationSettings {
        recent_messages_to_keep: 0,
    };
    assert_eq!(s.keep_count(), RECENT_MESSAGES_TO_KEEP_MIN);
}

#[test]
fn finish_reason_serializes_snake_case() {
    let s = serde_json::to_string(&FinishReason::ContentFilter).unwrap();
    assert_eq!(s, "\"content_filter\"");
}

#[test]
fn llm_message_metadata_round_trips() {
    let meta = LlmMessageMetadata {
        model_used: "gpt-4".into(),
        finish_reason: FinishReason::Stop,
        temperature_used: Some(0.7),
        usage: Some(LlmUsage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            cached_tokens: Some(5),
        }),
    };
    let json = meta.to_json();
    let back: LlmMessageMetadata = serde_json::from_value(json).unwrap();
    assert_eq!(back, meta);
}

#[test]
fn schema_ids_are_stable() {
    // Acts as a tripwire: renaming any of these is a breaking change
    // (clients persist the IDs in the GTS registry).
    assert_eq!(
        schema_ids::LLM_PLUGIN_CONFIG_SCHEMA_ID,
        "gtx.cf.chat_engine.llm_gateway_plugin_config.v1~"
    );
    assert_eq!(
        schema_ids::LLM_SUMMARIZATION_SETTINGS_SCHEMA_ID,
        "gtx.cf.chat_engine.llm_gateway.summarization_settings.v1~"
    );
    assert_eq!(
        schema_ids::LLM_MESSAGE_METADATA_SCHEMA_ID,
        "gtx.cf.chat_engine.llm_gateway.message_metadata.v1~"
    );
    assert_eq!(
        schema_ids::LLM_USAGE_SCHEMA_ID,
        "gtx.cf.chat_engine.llm_gateway.usage.v1~"
    );
}
