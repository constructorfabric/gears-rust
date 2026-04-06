use super::*;

#[test]
fn default_config_is_valid() {
    StreamingConfig::default().validate().unwrap();
    EstimationBudgets::default().validate().unwrap();
    QuotaConfig::default().validate().unwrap();
    OutboxConfig::default().validate().unwrap();
    ContextConfig::default().validate().unwrap();
    RagConfig::default().validate().unwrap();
    ThumbnailConfig::default().validate().unwrap();
}

#[test]
fn estimation_budgets_validation() {
    let valid = EstimationBudgets::default();

    assert!(
        (EstimationBudgets {
            bytes_per_token_conservative: 0,
            ..valid
        })
        .validate()
        .is_err()
    );
    assert!(
        (EstimationBudgets {
            minimal_generation_floor: 0,
            ..valid
        })
        .validate()
        .is_err()
    );
}

#[test]
fn quota_config_validation() {
    assert!(
        (QuotaConfig {
            overshoot_tolerance_factor: 0.99,
            ..QuotaConfig::default()
        })
        .validate()
        .is_err()
    );
    assert!(
        (QuotaConfig {
            overshoot_tolerance_factor: 1.0,
            ..QuotaConfig::default()
        })
        .validate()
        .is_ok()
    );
    assert!(
        (QuotaConfig {
            overshoot_tolerance_factor: 1.5,
            ..QuotaConfig::default()
        })
        .validate()
        .is_ok()
    );
    assert!(
        (QuotaConfig {
            overshoot_tolerance_factor: 1.51,
            ..QuotaConfig::default()
        })
        .validate()
        .is_err()
    );
    assert!(
        (QuotaConfig {
            web_search_max_calls_per_message: 0,
            ..QuotaConfig::default()
        })
        .validate()
        .is_err()
    );
    assert!(
        (QuotaConfig {
            web_search_daily_quota: 0,
            ..QuotaConfig::default()
        })
        .validate()
        .is_err()
    );
    assert!(
        (QuotaConfig {
            code_interpreter_max_calls_per_message: 0,
            ..QuotaConfig::default()
        })
        .validate()
        .is_err()
    );
    assert!(
        (QuotaConfig {
            code_interpreter_daily_quota: 0,
            ..QuotaConfig::default()
        })
        .validate()
        .is_err()
    );
}

#[test]
fn channel_capacity_boundaries() {
    let valid = StreamingConfig::default();

    assert!(
        (StreamingConfig {
            sse_channel_capacity: 15,
            ..valid
        })
        .validate()
        .is_err()
    );
    assert!(
        (StreamingConfig {
            sse_channel_capacity: 16,
            ..valid
        })
        .validate()
        .is_ok()
    );
    assert!(
        (StreamingConfig {
            sse_channel_capacity: 64,
            ..valid
        })
        .validate()
        .is_ok()
    );
    assert!(
        (StreamingConfig {
            sse_channel_capacity: 65,
            ..valid
        })
        .validate()
        .is_err()
    );
}

#[test]
fn ping_interval_boundaries() {
    let valid = StreamingConfig::default();

    assert!(
        (StreamingConfig {
            sse_ping_interval_seconds: 4,
            ..valid
        })
        .validate()
        .is_err()
    );
    assert!(
        (StreamingConfig {
            sse_ping_interval_seconds: 5,
            ..valid
        })
        .validate()
        .is_ok()
    );
    assert!(
        (StreamingConfig {
            sse_ping_interval_seconds: 60,
            ..valid
        })
        .validate()
        .is_ok()
    );
    assert!(
        (StreamingConfig {
            sse_ping_interval_seconds: 61,
            ..valid
        })
        .validate()
        .is_err()
    );
}

#[test]
fn streaming_config_web_search_context_size_enum() {
    use crate::domain::llm::WebSearchContextSize;

    // Default is Low
    let cfg = StreamingConfig::default();
    assert_eq!(cfg.web_search_context_size, WebSearchContextSize::Low);

    // Valid values deserialize correctly
    for (json_val, expected) in [
        ("\"low\"", WebSearchContextSize::Low),
        ("\"medium\"", WebSearchContextSize::Medium),
        ("\"high\"", WebSearchContextSize::High),
    ] {
        let json = format!(r#"{{"web_search_context_size": {json_val}}}"#);
        let cfg: StreamingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.web_search_context_size, expected);
    }

    // Invalid values rejected at parse time
    for bad in ["\"Low\"", "\"med\"", "\"HIGH\"", "\"none\"", "\"\""] {
        let json = format!(r#"{{"web_search_context_size": {bad}}}"#);
        assert!(
            serde_json::from_str::<StreamingConfig>(&json).is_err(),
            "expected parse error for {bad}"
        );
    }
}

#[test]
fn provider_entry_deser_with_alias() {
    let json = r#"{
            "kind": "openai_responses",
            "storage_kind": "openai",
            "host": "10.0.0.1",
            "upstream_alias": "my-llm-service"
        }"#;
    let entry: ProviderEntry = serde_json::from_str(json).unwrap();
    assert_eq!(entry.host, "10.0.0.1");
    assert_eq!(entry.upstream_alias.as_deref(), Some("my-llm-service"));
    assert!(entry.auth_plugin_type.is_none());
}

#[test]
fn provider_entry_deser_without_alias() {
    let json = r#"{
            "kind": "openai_responses",
            "storage_kind": "azure",
            "host": "my-azure.openai.azure.com",
            "api_path": "/openai/v1/responses"
        }"#;
    let entry: ProviderEntry = serde_json::from_str(json).unwrap();
    assert!(entry.upstream_alias.is_none());
    assert_eq!(entry.host, "my-azure.openai.azure.com");
    assert_eq!(entry.api_path, "/openai/v1/responses");
}

#[test]
fn provider_entry_deser_with_auth() {
    let json = r#"{
            "kind": "openai_responses",
            "storage_kind": "openai",
            "host": "api.openai.com",
            "auth_plugin_type": "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.apikey.v1",
            "auth_config": {
                "header": "Authorization",
                "prefix": "Bearer ",
                "secret_ref": "cred://openai-key"
            }
        }"#;
    let entry: ProviderEntry = serde_json::from_str(json).unwrap();
    assert!(entry.auth_plugin_type.is_some());
    let config = entry.auth_config.unwrap();
    assert_eq!(config.get("header").unwrap(), "Authorization");
    assert_eq!(config.get("secret_ref").unwrap(), "cred://openai-key");
}

#[test]
fn default_providers_has_openai() {
    let cfg = MiniChatConfig::default();
    assert!(cfg.providers.contains_key("openai"));
    let openai = &cfg.providers["openai"];
    assert_eq!(openai.host, "api.openai.com");
    assert_eq!(openai.api_path, "/v1/responses");
}

#[test]
fn provider_entry_deser_with_tenant_overrides() {
    let json = r#"{
            "kind": "openai_responses",
            "storage_kind": "azure",
            "host": "default.openai.azure.com",
            "api_path": "/openai/v1/responses",
            "tenant_overrides": {
                "tenant-a": {
                    "host": "tenant-a.openai.azure.com"
                },
                "tenant-b": {
                    "host": "tenant-b.openai.azure.com"
                }
            }
        }"#;
    let entry: ProviderEntry = serde_json::from_str(json).unwrap();
    assert_eq!(entry.tenant_overrides.len(), 2);
    assert_eq!(
        entry.tenant_overrides["tenant-a"].host.as_deref(),
        Some("tenant-a.openai.azure.com")
    );
    assert!(entry.tenant_overrides["tenant-b"].host.is_some());
}

#[test]
fn effective_host_for_tenant_fallback() {
    let entry = ProviderEntry {
        kind: crate::infra::llm::ProviderKind::OpenAiResponses,
        upstream_alias: None,
        host: "default.openai.azure.com".to_owned(),
        port: None,
        use_http: false,
        api_path: "/v1/responses".to_owned(),
        auth_plugin_type: None,
        auth_config: None,
        storage_backend: None,
        supports_file_search_filters: true,
        storage_kind: StorageKind::Azure,
        api_version: Some("2024-10-21".to_owned()),
        tenant_overrides: {
            let mut m = HashMap::new();
            m.insert(
                "tenant-a".to_owned(),
                ProviderTenantOverride {
                    host: Some("tenant-a.openai.azure.com".to_owned()),
                    upstream_alias: None,
                    auth_plugin_type: None,
                    auth_config: None,
                },
            );
            // Tenant with no host override — inherits root.
            m.insert(
                "tenant-c".to_owned(),
                ProviderTenantOverride {
                    host: None,
                    upstream_alias: None,
                    auth_plugin_type: Some("custom-plugin".to_owned()),
                    auth_config: None,
                },
            );
            m
        },
    };
    assert_eq!(
        entry.effective_host_for_tenant("tenant-a"),
        "tenant-a.openai.azure.com"
    );
    assert_eq!(
        entry.effective_host_for_tenant("tenant-c"),
        "default.openai.azure.com"
    );
    assert_eq!(
        entry.effective_host_for_tenant("unknown"),
        "default.openai.azure.com"
    );
}

#[test]
fn effective_auth_for_tenant() {
    let root_auth: HashMap<String, String> = {
        let mut c = HashMap::new();
        c.insert("header".to_owned(), "api-key".to_owned());
        c.insert("secret_ref".to_owned(), "cred://root-key".to_owned());
        c
    };
    let tenant_auth: HashMap<String, String> = {
        let mut c = HashMap::new();
        c.insert("header".to_owned(), "api-key".to_owned());
        c.insert("secret_ref".to_owned(), "cred://tenant-a-key".to_owned());
        c
    };
    let entry = ProviderEntry {
        kind: crate::infra::llm::ProviderKind::OpenAiResponses,
        upstream_alias: None,
        host: "default.openai.azure.com".to_owned(),
        port: None,
        use_http: false,
        api_path: "/v1/responses".to_owned(),
        auth_plugin_type: Some("root-plugin".to_owned()),
        auth_config: Some(root_auth),
        storage_backend: None,
        supports_file_search_filters: true,
        storage_kind: StorageKind::Azure,
        api_version: Some("2024-10-21".to_owned()),
        tenant_overrides: {
            let mut m = HashMap::new();
            m.insert(
                "tenant-a".to_owned(),
                ProviderTenantOverride {
                    host: None,
                    upstream_alias: None,
                    auth_plugin_type: Some("tenant-plugin".to_owned()),
                    auth_config: Some(tenant_auth),
                },
            );
            m
        },
    };
    // Tenant with auth override.
    assert_eq!(
        entry.effective_auth_plugin_type_for_tenant("tenant-a"),
        Some("tenant-plugin")
    );
    assert_eq!(
        entry
            .effective_auth_config_for_tenant("tenant-a")
            .unwrap()
            .get("secret_ref")
            .unwrap(),
        "cred://tenant-a-key"
    );
    // Unknown tenant → falls back to root.
    assert_eq!(
        entry.effective_auth_plugin_type_for_tenant("unknown"),
        Some("root-plugin")
    );
    assert_eq!(
        entry
            .effective_auth_config_for_tenant("unknown")
            .unwrap()
            .get("secret_ref")
            .unwrap(),
        "cred://root-key"
    );
}

#[test]
fn validate_rejects_empty_tenant_override_host() {
    let entry = ProviderEntry {
        kind: crate::infra::llm::ProviderKind::OpenAiResponses,
        upstream_alias: None,
        host: "default.openai.azure.com".to_owned(),
        port: None,
        use_http: false,
        api_path: "/v1/responses".to_owned(),
        auth_plugin_type: None,
        auth_config: None,
        storage_backend: None,
        supports_file_search_filters: true,
        storage_kind: StorageKind::Azure,
        api_version: Some("2024-10-21".to_owned()),
        tenant_overrides: {
            let mut m = HashMap::new();
            m.insert(
                "bad-tenant".to_owned(),
                ProviderTenantOverride {
                    host: Some("  ".to_owned()),
                    upstream_alias: None,
                    auth_plugin_type: None,
                    auth_config: None,
                },
            );
            m
        },
    };
    let err = entry.validate("azure_openai").unwrap_err();
    assert!(err.contains("bad-tenant"));
    assert!(err.contains("host must not be empty"));
}

#[test]
fn validate_rejects_auth_only_override_without_alias() {
    let entry = ProviderEntry {
        kind: crate::infra::llm::ProviderKind::OpenAiResponses,
        upstream_alias: None,
        host: "default.openai.azure.com".to_owned(),
        port: None,
        use_http: false,
        api_path: "/v1/responses".to_owned(),
        auth_plugin_type: None,
        auth_config: None,
        storage_backend: None,
        supports_file_search_filters: true,
        storage_kind: StorageKind::Azure,
        api_version: Some("2024-10-21".to_owned()),
        tenant_overrides: {
            let mut m = HashMap::new();
            m.insert(
                "tenant-a".to_owned(),
                ProviderTenantOverride {
                    host: None,
                    upstream_alias: None,
                    auth_plugin_type: Some("custom-plugin".to_owned()),
                    auth_config: Some({
                        let mut c = HashMap::new();
                        c.insert("secret_ref".to_owned(), "tenant-a-key".to_owned());
                        c
                    }),
                },
            );
            m
        },
    };
    let err = entry.validate("azure_openai").unwrap_err();
    assert!(err.contains("tenant-a"));
    assert!(err.contains("overrides auth"));
}

#[test]
fn validate_rejects_auth_plugin_type_only_override_without_alias() {
    let entry = ProviderEntry {
        kind: crate::infra::llm::ProviderKind::OpenAiResponses,
        upstream_alias: None,
        host: "default.openai.azure.com".to_owned(),
        port: None,
        use_http: false,
        api_path: "/v1/responses".to_owned(),
        auth_plugin_type: None,
        auth_config: None,
        storage_backend: None,
        supports_file_search_filters: true,
        storage_kind: StorageKind::Azure,
        api_version: Some("2024-10-21".to_owned()),
        tenant_overrides: {
            let mut m = HashMap::new();
            m.insert(
                "tenant-b".to_owned(),
                ProviderTenantOverride {
                    host: None,
                    upstream_alias: None,
                    auth_plugin_type: Some("different-plugin".to_owned()),
                    auth_config: None,
                },
            );
            m
        },
    };
    let err = entry.validate("azure_openai").unwrap_err();
    assert!(err.contains("tenant-b"));
    assert!(err.contains("overrides auth"));
}

#[test]
fn validate_accepts_auth_only_override_with_explicit_alias() {
    let entry = ProviderEntry {
        kind: crate::infra::llm::ProviderKind::OpenAiResponses,
        upstream_alias: None,
        host: "default.openai.azure.com".to_owned(),
        port: None,
        use_http: false,
        api_path: "/v1/responses".to_owned(),
        auth_plugin_type: None,
        auth_config: None,
        storage_backend: None,
        supports_file_search_filters: true,
        storage_kind: StorageKind::Azure,
        api_version: Some("2024-10-21".to_owned()),
        tenant_overrides: {
            let mut m = HashMap::new();
            m.insert(
                "tenant-a".to_owned(),
                ProviderTenantOverride {
                    host: None,
                    upstream_alias: Some("azure-tenant-a".to_owned()),
                    auth_plugin_type: Some("custom-plugin".to_owned()),
                    auth_config: None,
                },
            );
            m
        },
    };
    assert!(entry.validate("azure_openai").is_ok());
}

#[test]
fn validate_accepts_host_differing_override_with_auth() {
    let entry = ProviderEntry {
        kind: crate::infra::llm::ProviderKind::OpenAiResponses,
        upstream_alias: None,
        host: "default.openai.azure.com".to_owned(),
        port: None,
        use_http: false,
        api_path: "/v1/responses".to_owned(),
        auth_plugin_type: None,
        auth_config: None,
        storage_backend: None,
        supports_file_search_filters: true,
        storage_kind: StorageKind::Azure,
        api_version: Some("2024-10-21".to_owned()),
        tenant_overrides: {
            let mut m = HashMap::new();
            m.insert(
                "tenant-a".to_owned(),
                ProviderTenantOverride {
                    host: Some("tenant-a.openai.azure.com".to_owned()),
                    upstream_alias: None,
                    auth_plugin_type: Some("custom-plugin".to_owned()),
                    auth_config: None,
                },
            );
            m
        },
    };
    assert!(entry.validate("azure_openai").is_ok());
}

#[test]
fn metrics_effective_prefix_uses_module_name_when_empty() {
    let cfg = MetricsConfig {
        prefix: String::new(),
    };
    assert_eq!(cfg.effective_prefix("mini-chat"), "mini_chat");
}

#[test]
fn metrics_effective_prefix_uses_module_name_when_whitespace() {
    let cfg = MetricsConfig {
        prefix: "   ".to_owned(),
    };
    assert_eq!(cfg.effective_prefix("mini-chat"), "mini_chat");
}

#[test]
fn metrics_effective_prefix_uses_trimmed_explicit_prefix() {
    let cfg = MetricsConfig {
        prefix: "  custom.prefix  ".to_owned(),
    };
    assert_eq!(cfg.effective_prefix("mini-chat"), "custom.prefix");
}
