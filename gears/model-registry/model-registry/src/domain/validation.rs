//! Request field checks (DESIGN §3.1 field bounds, §3.7 column bounds, §4
//! Configuration `max_json_document_bytes`).

use model_registry_sdk::field::Reason;
use model_registry_sdk::{
    CreateModelRequestV1, CreateProviderRequestV1, DefaultInferenceParametersV1,
    DisabledCapabilities, ModelCapabilities, UpdateModelRequestV1, UpdateProviderRequestV1,
};
use toolkit_macros::domain_model;

use super::error::{DomainError, Violation};

pub const SLUG_MAX: usize = 64;
pub const NAME_MAX: usize = 255;
pub const GTS_TYPE_MAX: usize = 512;
pub const MODEL_NAME_MAX: usize = 128;
pub const PROVIDER_MODEL_ID_MAX: usize = 255;
pub const URL_MAX: usize = 2048;
pub const SECRET_REF_MAX: usize = 255;
pub const DESCRIPTION_MAX: usize = 4096;
pub const EXTRA_PARAMS_MAX: usize = 64;
pub const EXTRA_PARAM_NAME_MAX: usize = 128;
pub const MIME_LIST_MAX: usize = 64;
/// RFC 6838 §4.2: type and subtype names are at most 127 characters each.
pub const MIME_MAX: usize = 255;

/// Column bounds of §3.7 for the optional model info strings.
const FAMILY_MAX: usize = 128;
const VENDOR_MAX: usize = 128;
const ARCHITECTURE_MAX: usize = 64;
const FORMAT_MAX: usize = 64;
const REGION_MAX: usize = 64;
const HOSTED_BY_MAX: usize = 128;
const REASONING_LEVEL_MAX: usize = 64;
const MODEL_VERSION_MAX: usize = 64;
const MULTIPLIER_DISPLAY_MAX: usize = 32;

/// Bounds that come from configuration.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_json_document_bytes: usize,
}

struct Checker {
    limits: Limits,
    violations: Vec<Violation>,
}

impl Checker {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            violations: Vec::new(),
        }
    }

    fn finish(self) -> Result<(), DomainError> {
        if self.violations.is_empty() {
            Ok(())
        } else {
            Err(DomainError::invalid_all(self.violations))
        }
    }

    fn push(&mut self, field: &str, description: impl Into<String>, reason: Reason) {
        self.violations
            .push(Violation::new(field, description, reason));
    }

    /// U+0000 cannot be stored in a text or JSON column.
    fn no_nul(&mut self, field: &str, value: &str) {
        if value.contains('\0') {
            self.push(field, "must not contain U+0000", Reason::InvalidFormat);
        }
    }

    fn len_between(&mut self, field: &str, value: &str, min: usize, max: usize) {
        self.no_nul(field, value);
        let n = value.chars().count();
        if n < min {
            self.push(
                field,
                format!("must be at least {min} character(s)"),
                Reason::InvalidFormat,
            );
        } else if n > max {
            self.push(
                field,
                format!("must be at most {max} characters"),
                Reason::TooLarge,
            );
        }
    }

    fn opt_len(&mut self, field: &str, value: Option<&str>, max: usize) {
        if let Some(v) = value {
            self.len_between(field, v, 0, max);
        }
    }

    fn slug(&mut self, value: &str) {
        self.len_between("slug", value, 1, SLUG_MAX);
        if !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            self.push(
                "slug",
                "must contain only lowercase letters, digits, and hyphens",
                Reason::InvalidFormat,
            );
        }
    }

    fn model_name(&mut self, value: &str) {
        self.len_between("name", value, 1, MODEL_NAME_MAX);
        if !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_'))
        {
            self.push(
                "name",
                "must contain only lowercase letters, digits, hyphen, dot, and underscore",
                Reason::InvalidFormat,
            );
        }
    }

    fn provider_model_id(&mut self, value: &str) {
        self.len_between("provider_model_id", value, 1, PROVIDER_MODEL_ID_MAX);
        if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
            self.push(
                "provider_model_id",
                "must not contain whitespace or control characters",
                Reason::InvalidFormat,
            );
        }
    }

    fn http_url(&mut self, field: &str, value: &str) {
        self.no_nul(field, value);
        if value.chars().count() > URL_MAX {
            self.push(
                field,
                format!("must be at most {URL_MAX} characters"),
                Reason::TooLarge,
            );
            return;
        }
        match url::Url::parse(value) {
            Ok(u) if (u.scheme() == "http" || u.scheme() == "https") && u.host().is_some() => {
                if !u.username().is_empty() || u.password().is_some() {
                    self.push(
                        field,
                        "must not carry user information",
                        Reason::InvalidFormat,
                    );
                }
            }
            _ => self.push(
                field,
                "must be an absolute http or https URL",
                Reason::InvalidFormat,
            ),
        }
    }

    fn secret_ref(&mut self, value: &str) {
        self.len_between("secret_ref", value, 1, SECRET_REF_MAX);
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            self.push(
                "secret_ref",
                "must contain only letters, digits, underscore, and hyphen",
                Reason::InvalidFormat,
            );
        }
    }

    fn json_document(&mut self, field: &str, value: &serde_json::Value) {
        if json_has_nul(value) {
            self.push(field, "must not contain U+0000", Reason::InvalidFormat);
        }
        let bytes = serde_json::to_vec(value).map_or(usize::MAX, |v| v.len());
        if bytes > self.limits.max_json_document_bytes {
            self.push(
                field,
                format!(
                    "must be at most {} bytes when serialized",
                    self.limits.max_json_document_bytes
                ),
                Reason::TooLarge,
            );
        }
    }

    fn serialized_document<T: serde::Serialize>(&mut self, field: &str, value: &T) {
        match serde_json::to_value(value) {
            Ok(v) => self.json_document(field, &v),
            Err(e) => self.push(
                field,
                format!("not serializable: {e}"),
                Reason::InvalidFormat,
            ),
        }
    }

    fn mime_list(&mut self, field: &str, list: &[String]) {
        if list.len() > MIME_LIST_MAX {
            self.push(
                field,
                format!("must list at most {MIME_LIST_MAX} media types"),
                Reason::TooLarge,
            );
        }
        for mime in list {
            if mime.len() > MIME_MAX {
                self.push(
                    field,
                    format!("a media type must be at most {MIME_MAX} characters"),
                    Reason::TooLarge,
                );
                continue;
            }
            let ok = mime.split_once('/').is_some_and(|(t, s)| {
                !t.is_empty()
                    && !s.is_empty()
                    && mime
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "/+-.".contains(c))
            });
            if !ok {
                self.push(
                    field,
                    format!("`{mime}` is not a lowercase RFC 6838 media type"),
                    Reason::InvalidFormat,
                );
            }
        }
    }

    fn capabilities(&mut self, caps: &ModelCapabilities) {
        self.serialized_document("capabilities", caps);
        self.mime_list(
            "capabilities/vision/supported_mime_types",
            &caps.vision.supported_mime_types,
        );
        self.mime_list(
            "capabilities/file_input/supported_mime_types",
            &caps.file_input.supported_mime_types,
        );
        self.mime_list(
            "capabilities/image_generation/supported_mime_types",
            &caps.image_generation.supported_mime_types,
        );
        self.mime_list(
            "capabilities/audio_input/supported_mime_types",
            &caps.audio_input.supported_mime_types,
        );
        self.mime_list(
            "capabilities/audio_output/supported_mime_types",
            &caps.audio_output.supported_mime_types,
        );
    }

    fn disabled_capabilities(&mut self, caps: &DisabledCapabilities) {
        self.serialized_document("disabled_capabilities", caps);
        self.mime_list(
            "disabled_capabilities/vision/disabled_mime_types",
            &caps.vision.disabled_mime_types,
        );
        self.mime_list(
            "disabled_capabilities/file_input/disabled_mime_types",
            &caps.file_input.disabled_mime_types,
        );
        self.mime_list(
            "disabled_capabilities/image_generation/disabled_mime_types",
            &caps.image_generation.disabled_mime_types,
        );
        self.mime_list(
            "disabled_capabilities/audio_input/disabled_mime_types",
            &caps.audio_input.disabled_mime_types,
        );
        self.mime_list(
            "disabled_capabilities/audio_output/disabled_mime_types",
            &caps.audio_output.disabled_mime_types,
        );
    }

    fn extra_params(&mut self, names: &[String]) {
        if names.len() > EXTRA_PARAMS_MAX {
            self.push(
                "allow_extra_params",
                format!("must list at most {EXTRA_PARAMS_MAX} names"),
                Reason::TooLarge,
            );
        }
        for (i, name) in names.iter().enumerate() {
            self.no_nul(&format!("allow_extra_params/{i}"), name);
            let n = name.chars().count();
            if n == 0 || n > EXTRA_PARAM_NAME_MAX {
                self.push(
                    &format!("allow_extra_params/{i}"),
                    format!("must be 1 to {EXTRA_PARAM_NAME_MAX} characters"),
                    if n == 0 {
                        Reason::InvalidFormat
                    } else {
                        Reason::TooLarge
                    },
                );
            }
        }
    }

    fn default_parameters(&mut self, params: &DefaultInferenceParametersV1) {
        self.serialized_document("default_parameters", params);
    }

    fn size_bytes(&mut self, value: Option<u64>) {
        if value.is_some_and(|v| i64::try_from(v).is_err()) {
            self.push("size_bytes", "must be at most 2^63 - 1", Reason::TooLarge);
        }
    }
}

fn json_has_nul(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => s.contains('\0'),
        serde_json::Value::Array(items) => items.iter().any(json_has_nul),
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(key, value)| key.contains('\0') || json_has_nul(value)),
        _ => false,
    }
}

pub fn validate_create_provider(
    req: &CreateProviderRequestV1,
    limits: Limits,
) -> Result<(), DomainError> {
    let mut c = Checker::new(limits);
    c.slug(req.slug());
    c.len_between("name", req.name(), 1, NAME_MAX);
    c.len_between("gts_type", req.gts_type().as_ref(), 0, GTS_TYPE_MAX);
    if let Some(m) = req.metadata() {
        c.json_document("metadata", m);
    }
    c.finish()
}

pub fn validate_update_provider(
    req: &UpdateProviderRequestV1,
    limits: Limits,
) -> Result<(), DomainError> {
    let mut c = Checker::new(limits);
    if let Some(name) = &req.name {
        c.len_between("name", name, 1, NAME_MAX);
    }
    c.opt_len(
        "gts_type",
        req.gts_type.as_ref().map(AsRef::as_ref),
        GTS_TYPE_MAX,
    );
    if let Some(Some(m)) = &req.metadata {
        c.json_document("metadata", m);
    }
    c.finish()
}

pub fn validate_create_model(
    req: &CreateModelRequestV1,
    limits: Limits,
) -> Result<(), DomainError> {
    let mut c = Checker::new(limits);
    c.model_name(&req.name);
    c.provider_model_id(&req.provider_model_id);
    if let Some(u) = &req.base_url {
        c.http_url("base_url", u);
    }
    if let Some(s) = &req.secret_ref {
        c.secret_ref(s);
    }
    c.len_between("display_name", &req.display_name, 1, NAME_MAX);
    c.opt_len("description", req.description.as_deref(), DESCRIPTION_MAX);
    c.opt_len("family", req.family.as_deref(), FAMILY_MAX);
    c.opt_len("vendor", req.vendor.as_deref(), VENDOR_MAX);
    c.opt_len(
        "architecture",
        req.architecture.as_deref(),
        ARCHITECTURE_MAX,
    );
    c.size_bytes(req.size_bytes);
    c.opt_len("format", req.format.as_deref(), FORMAT_MAX);
    c.opt_len("region", req.region.as_deref(), REGION_MAX);
    c.opt_len("hosted_by", req.hosted_by.as_deref(), HOSTED_BY_MAX);
    c.opt_len(
        "reasoning_level",
        req.reasoning_level.as_deref(),
        REASONING_LEVEL_MAX,
    );
    c.opt_len(
        "model_version",
        req.model_version.as_deref(),
        MODEL_VERSION_MAX,
    );
    if let Some(u) = &req.icon {
        c.http_url("icon", u);
    }
    c.opt_len(
        "multiplier_display",
        req.multiplier_display.as_deref(),
        MULTIPLIER_DISPLAY_MAX,
    );
    c.serialized_document("additional_info", &req.additional_info);
    c.capabilities(&req.capabilities);
    c.disabled_capabilities(&req.disabled_capabilities);
    c.default_parameters(&req.default_parameters);
    c.extra_params(&req.allow_extra_params);
    c.json_document("provider_settings", &req.provider_settings);
    c.finish()
}

pub fn validate_update_model(
    req: &UpdateModelRequestV1,
    limits: Limits,
) -> Result<(), DomainError> {
    let mut c = Checker::new(limits);
    if let Some(p) = &req.provider_model_id {
        c.provider_model_id(p);
    }
    if let Some(Some(u)) = &req.base_url {
        c.http_url("base_url", u);
    }
    if let Some(Some(s)) = &req.secret_ref {
        c.secret_ref(s);
    }
    if let Some(d) = &req.display_name {
        c.len_between("display_name", d, 1, NAME_MAX);
    }
    let opt = |v: &Option<Option<String>>| v.as_ref().and_then(|o| o.as_deref().map(str::to_owned));
    c.opt_len(
        "description",
        opt(&req.description).as_deref(),
        DESCRIPTION_MAX,
    );
    c.opt_len("family", opt(&req.family).as_deref(), FAMILY_MAX);
    c.opt_len("vendor", opt(&req.vendor).as_deref(), VENDOR_MAX);
    c.opt_len(
        "architecture",
        opt(&req.architecture).as_deref(),
        ARCHITECTURE_MAX,
    );
    if let Some(Some(s)) = req.size_bytes {
        c.size_bytes(Some(s));
    }
    c.opt_len("format", opt(&req.format).as_deref(), FORMAT_MAX);
    c.opt_len("region", opt(&req.region).as_deref(), REGION_MAX);
    c.opt_len("hosted_by", opt(&req.hosted_by).as_deref(), HOSTED_BY_MAX);
    c.opt_len(
        "reasoning_level",
        opt(&req.reasoning_level).as_deref(),
        REASONING_LEVEL_MAX,
    );
    c.opt_len(
        "model_version",
        opt(&req.model_version).as_deref(),
        MODEL_VERSION_MAX,
    );
    if let Some(Some(u)) = &req.icon {
        c.http_url("icon", u);
    }
    c.opt_len(
        "multiplier_display",
        opt(&req.multiplier_display).as_deref(),
        MULTIPLIER_DISPLAY_MAX,
    );
    if let Some(a) = &req.additional_info {
        c.serialized_document("additional_info", a);
    }
    if let Some(caps) = &req.capabilities {
        c.capabilities(caps);
    }
    if let Some(caps) = &req.disabled_capabilities {
        c.disabled_capabilities(caps);
    }
    if let Some(p) = &req.default_parameters {
        c.default_parameters(p);
    }
    if let Some(names) = &req.allow_extra_params {
        c.extra_params(names);
    }
    if let Some(s) = &req.provider_settings {
        c.json_document("provider_settings", s);
    }
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{model_update, provider_update};
    use model_registry_sdk::ContextWindow;
    use toolkit_gts::gts_id;
    use uuid::Uuid;

    const LIMITS: Limits = Limits {
        max_json_document_bytes: 1024,
    };

    fn reasons(result: Result<(), DomainError>) -> Vec<(String, Reason)> {
        match result {
            Ok(()) => Vec::new(),
            Err(DomainError::Invalid { violations: v, .. }) => {
                v.into_iter().map(|v| (v.field, v.reason)).collect()
            }
            Err(other) => panic!("{other:?}"),
        }
    }

    fn gts() -> gts::GtsTypeId {
        gts::GtsTypeId::new(gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~"))
    }

    #[test]
    fn provider_create_checks() {
        let ok = CreateProviderRequestV1::builder("open-ai2", "OpenAI", gts()).build();
        assert!(reasons(validate_create_provider(&ok, LIMITS)).is_empty());

        let bad = CreateProviderRequestV1::builder("Open AI", "", gts())
            .metadata(serde_json::json!({ "k": "x".repeat(2000) }))
            .build();
        let r = reasons(validate_create_provider(&bad, LIMITS));
        assert!(r.contains(&("slug".into(), Reason::InvalidFormat)), "{r:?}");
        assert!(r.contains(&("name".into(), Reason::InvalidFormat)), "{r:?}");
        assert!(r.contains(&("metadata".into(), Reason::TooLarge)), "{r:?}");

        let long = CreateProviderRequestV1::builder("a".repeat(65), "n", gts()).build();
        assert_eq!(
            reasons(validate_create_provider(&long, LIMITS)),
            vec![("slug".into(), Reason::TooLarge)]
        );

        let long_type = gts::GtsTypeId::new(&format!("{}{}~", gts().as_ref(), "a".repeat(512)));
        let long = CreateProviderRequestV1::builder("openai", "n", long_type.clone()).build();
        assert_eq!(
            reasons(validate_create_provider(&long, LIMITS)),
            vec![("gts_type".into(), Reason::TooLarge)]
        );
        let mut update = UpdateProviderRequestV1::default();
        update.gts_type = Some(long_type);
        assert_eq!(
            reasons(validate_update_provider(&update, LIMITS)),
            vec![("gts_type".into(), Reason::TooLarge)]
        );
    }

    #[test]
    fn provider_update_checks() {
        let req = provider_update(|r| {
            r.name = Some(String::new());
            r.metadata = Some(None);
        });
        assert_eq!(
            reasons(validate_update_provider(&req, LIMITS)),
            vec![("name".into(), Reason::InvalidFormat)]
        );
    }

    fn model_req() -> CreateModelRequestV1 {
        CreateModelRequestV1::new(
            Uuid::new_v4(),
            "gpt-4o",
            "gpt-4o",
            "GPT-4o",
            ContextWindow::default(),
        )
    }

    #[test]
    fn model_create_checks() {
        assert!(reasons(validate_create_model(&model_req(), LIMITS)).is_empty());

        let mut req = model_req();
        req.provider_model_id = "gpt 4o".into();
        req.base_url = Some("ftp://x".into());
        req.icon = Some("https://user:pw@host/i.png".into());
        req.secret_ref = Some("bad ref".into());
        req.size_bytes = Some(u64::MAX);
        req.description = Some("d".repeat(4097));
        req.allow_extra_params = vec![String::new()];
        req.capabilities.vision.enabled = true;
        req.capabilities.vision.supported_mime_types = vec!["image/PNG".into()];
        req.provider_settings = serde_json::json!({ "k": "x".repeat(2000) });
        let r = reasons(validate_create_model(&req, LIMITS));
        let expect = [
            ("provider_model_id", Reason::InvalidFormat),
            ("base_url", Reason::InvalidFormat),
            ("icon", Reason::InvalidFormat),
            ("secret_ref", Reason::InvalidFormat),
            ("size_bytes", Reason::TooLarge),
            ("description", Reason::TooLarge),
            ("allow_extra_params/0", Reason::InvalidFormat),
            (
                "capabilities/vision/supported_mime_types",
                Reason::InvalidFormat,
            ),
            ("provider_settings", Reason::TooLarge),
        ];
        for (f, reason) in expect {
            assert!(r.contains(&(f.to_owned(), reason)), "missing {f}: {r:?}");
        }
    }

    #[test]
    fn nul_characters_and_oversized_capabilities_are_rejected() {
        let mut req = model_req();
        req.display_name = "GPT\0".into();
        req.provider_settings = serde_json::json!({ "k\0": { "nested": ["a\0"] } });
        req.capabilities.vision.supported_mime_types =
            vec![format!("image/{}", "a".repeat(MIME_MAX))];
        req.disabled_capabilities.vision.disabled_mime_types = (0..MIME_LIST_MAX)
            .map(|i| format!("image/x-{i:060}"))
            .collect();
        let r = reasons(validate_create_model(&req, LIMITS));
        let expect = [
            ("display_name", Reason::InvalidFormat),
            ("provider_settings", Reason::InvalidFormat),
            ("capabilities/vision/supported_mime_types", Reason::TooLarge),
            ("disabled_capabilities", Reason::TooLarge),
        ];
        for (f, reason) in expect {
            assert!(r.contains(&(f.to_owned(), reason)), "missing {f}: {r:?}");
        }

        let bad = CreateProviderRequestV1::builder("openai", "Open\0AI", gts())
            .metadata(serde_json::json!({ "k": "v\0" }))
            .build();
        assert_eq!(
            reasons(validate_create_provider(&bad, LIMITS)),
            vec![
                ("name".into(), Reason::InvalidFormat),
                ("metadata".into(), Reason::InvalidFormat)
            ]
        );
    }

    #[test]
    fn model_update_checks_only_present_fields() {
        let req = UpdateModelRequestV1::default();
        assert!(reasons(validate_update_model(&req, LIMITS)).is_empty());
        let req = model_update(|r| {
            r.base_url = Some(None);
            r.display_name = Some(String::new());
            r.region = Some(Some("r".repeat(65)));
        });
        let r = reasons(validate_update_model(&req, LIMITS));
        assert_eq!(
            r,
            vec![
                ("display_name".into(), Reason::InvalidFormat),
                ("region".into(), Reason::TooLarge)
            ]
        );
    }
}
