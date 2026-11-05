//! Capability Checker, media input (DESIGN §3.2 Capability Checker / Media
//! Resolver, §4.1 `media_not_found`, §4.5 `limits.max_inline_media_bytes`).
//!
//! Parts are checked in document order and the first failure wins; capability
//! rejections (`file_id`, audio, video, `media_url`) and media rejections are
//! interleaved by part.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use llm_gateway_sdk::{CreateResponseBody, LlmGatewayError};
use model_registry_sdk::ModelInfoV1;
use serde_json::json;

use super::{CapabilityChecker, EffectiveRequest};
use crate::config::{LimitsConfig, MediaConfig};
use crate::domain::ports::ResolvedPlugin;
use crate::domain::provider_rules::ProviderRulesRegistry;
use crate::domain::test_support::{
    CallLog, FakePlugin, OPENAI_TYPE, body, sample_model_info_patched,
};

const PNG_URI: &str = "data:image/png;base64,iVBORw0KGgo=";

fn model() -> ModelInfoV1 {
    sample_model_info_patched(OPENAI_TYPE, |v| {
        v["capabilities"]["vision"]["enabled"] = json!(true);
        v["capabilities"]["file_input"]["enabled"] = json!(true);
    })
}

#[derive(Clone)]
struct Setup {
    prefetch: bool,
    route: bool,
    limits: LimitsConfig,
    media: MediaConfig,
}

impl Setup {
    /// `UrlForward` plugin, no media route, default limits and lists.
    fn url_forward() -> Self {
        Self {
            prefetch: false,
            route: false,
            limits: LimitsConfig::default(),
            media: MediaConfig::default(),
        }
    }

    /// `PrefetchRequired` plugin, no media route.
    fn prefetch() -> Self {
        Self {
            prefetch: true,
            ..Self::url_forward()
        }
    }

    fn with_route(mut self) -> Self {
        self.route = true;
        self
    }

    fn max_inline(mut self, bytes: usize) -> Self {
        self.limits.max_inline_media_bytes = bytes;
        self
    }

    fn check(&self, parts: serde_json::Value) -> Result<EffectiveRequest, LlmGatewayError> {
        self.check_request(request(parts))
    }

    fn check_request(
        &self,
        request: CreateResponseBody,
    ) -> Result<EffectiveRequest, LlmGatewayError> {
        let fake = FakePlugin::echo_id(Arc::new(CallLog::default()));
        fake.set_prefetch_media(self.prefetch);
        let plugin = ResolvedPlugin::new(fake, "openai");
        CapabilityChecker::new(&self.limits, Arc::new(ProviderRulesRegistry::permissive()))
            .with_media_policy(&self.media, self.route)
            .check(&model(), &plugin, request)
    }
}

#[allow(clippy::needless_pass_by_value)] // call sites build the parts inline with `json!`
fn request(parts: serde_json::Value) -> CreateResponseBody {
    let mut value = serde_json::to_value(body("openai::gpt-4o")).expect("body serializes");
    value["input"] = json!([{ "type": "message", "role": "user", "content": parts }]);
    serde_json::from_value(value).expect("request deserializes")
}

fn image(url: &str) -> serde_json::Value {
    json!({ "type": "input_image", "image_url": url })
}

fn file_url(url: &str) -> serde_json::Value {
    json!({ "type": "input_file", "file_url": url })
}

fn file_data(data: &str, filename: Option<&str>) -> serde_json::Value {
    match filename {
        Some(name) => json!({ "type": "input_file", "file_data": data, "filename": name }),
        None => json!({ "type": "input_file", "file_data": data }),
    }
}

fn b64(len: usize) -> String {
    STANDARD.encode(vec![1u8; len])
}

fn uri(mime: &str, len: usize) -> String {
    format!("data:{mime};base64,{}", b64(len))
}

fn assert_ok(result: Result<EffectiveRequest, LlmGatewayError>) {
    if let Err(err) = result {
        panic!("expected the request to pass, got {err:?}");
    }
}

fn assert_capability(result: Result<EffectiveRequest, LlmGatewayError>, expected: &str) {
    let err = result.expect_err("check must fail");
    assert!(
        matches!(&err, LlmGatewayError::CapabilityNotSupported { capability, .. } if capability == expected),
        "expected capability_not_supported({expected}), got {err:?}"
    );
}

fn assert_media_not_found(result: Result<EffectiveRequest, LlmGatewayError>, param: &str) {
    let err = result.expect_err("check must fail");
    assert!(
        matches!(&err, LlmGatewayError::MediaNotFound { param: Some(p), .. } if p == param),
        "expected media_not_found({param}), got {err:?}"
    );
    assert_eq!(err.to_string(), "media not found", "fixed message");
}

fn assert_too_large(result: Result<EffectiveRequest, LlmGatewayError>) {
    let err = result.expect_err("check must fail");
    assert!(
        matches!(&err, LlmGatewayError::Validation { param: Some(p), .. } if p == "input"),
        "expected validation_error(input), got {err:?}"
    );
}

// ── capabilities the gear does not offer ────────────────────────────────────

#[test]
fn file_id_on_an_image_is_not_supported_in_both_modes() {
    for setup in [Setup::url_forward(), Setup::prefetch().with_route()] {
        assert_capability(
            setup.check(json!([{ "type": "input_image", "file_id": "file_1" }])),
            "file_id",
        );
    }
}

#[test]
fn file_id_on_a_file_is_not_supported_in_both_modes() {
    for setup in [Setup::url_forward(), Setup::prefetch().with_route()] {
        assert_capability(
            setup.check(json!([{ "type": "input_file", "file_id": "file_1" }])),
            "file_id",
        );
    }
}

#[test]
fn file_id_is_rejected_even_next_to_a_usable_source() {
    assert_capability(
        Setup::url_forward().check(json!([
            { "type": "input_image", "image_url": "https://example.com/a.png", "file_id": "f" }
        ])),
        "file_id",
    );
    assert_capability(
        Setup::url_forward().check(json!([
            { "type": "input_file", "file_url": "https://example.com/a.pdf", "file_id": "f" }
        ])),
        "file_id",
    );
}

#[test]
fn audio_input_is_not_supported() {
    for setup in [Setup::url_forward(), Setup::prefetch()] {
        assert_capability(
            setup.check(json!([{ "type": "input_audio", "data": "aGk=", "format": "wav" }])),
            "audio_input",
        );
        assert_capability(
            setup.check(json!([{ "type": "input_audio", "url": "https://example.com/a.wav" }])),
            "audio_input",
        );
    }
}

#[test]
fn video_input_is_not_supported() {
    for setup in [Setup::url_forward(), Setup::prefetch()] {
        assert_capability(
            setup.check(json!([{ "type": "input_video", "url": "https://example.com/a.mp4" }])),
            "video_input",
        );
        assert_capability(
            setup.check(json!([{ "type": "input_video", "file_id": "f" }])),
            "video_input",
        );
    }
}

#[test]
fn model_capability_errors_win_over_the_part_checks() {
    // vision disabled: `vision` is reported, not `file_id`.
    let no_vision = sample_model_info_patched(OPENAI_TYPE, |_| {});
    let fake = FakePlugin::echo_id(Arc::new(CallLog::default()));
    let plugin = ResolvedPlugin::new(fake, "openai");
    let err = CapabilityChecker::new(
        &LimitsConfig::default(),
        Arc::new(ProviderRulesRegistry::permissive()),
    )
    .check(
        &no_vision,
        &plugin,
        request(json!([{ "type": "input_image", "file_id": "f" }])),
    )
    .expect_err("vision disabled");
    assert!(
        matches!(&err, LlmGatewayError::CapabilityNotSupported { capability, .. } if capability == "vision"),
        "{err:?}"
    );
}

// ── URL rule (image_url, file_url) ──────────────────────────────────────────

#[test]
fn https_urls_pass_in_url_forward_mode_without_a_route() {
    let setup = Setup::url_forward();
    for url in [
        "https://example.com/a.png",
        "HTTPS://EXAMPLE.COM/a.png",
        "https://example.com:8443/a.png?x=1",
        "https://[2001:db8::1]/a.png",
        "https://203.0.113.7/a.png",
        "https://example.com/a@b.png",
    ] {
        assert_ok(setup.check(json!([image(url)])));
        assert_ok(setup.check(json!([file_url(url)])));
    }
}

#[test]
fn url_forward_leaves_the_request_unchanged() {
    let original = request(json!([
        { "type": "input_text", "text": "x" },
        image("https://example.com/a.png"),
        image(PNG_URI),
        file_url("https://example.com/a.pdf"),
        file_data("aGk=", Some("a.txt"))
    ]));
    let effective = Setup::url_forward()
        .check_request(original.clone())
        .expect("passes");
    assert_eq!(effective.body, original);
}

#[test]
fn urls_that_break_the_scheme_rule_are_media_not_found_naming_the_part() {
    let setup = Setup::url_forward();
    for url in [
        "http://example.com/a.png",
        "ftp://example.com/a.png",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "//example.com/a.png",
        "example.com/a.png",
        "/a.png",
        "",
        " https://example.com/a.png",
        "https://user@example.com/a.png",
        "https://user:pw@example.com/a.png",
        "https://@example.com/a.png",
        "https:///a.png",
    ] {
        assert_media_not_found(
            setup.check(json!([{ "type": "input_text", "text": "x" }, image(url)])),
            "input[0].content[1]",
        );
        assert_media_not_found(setup.check(json!([file_url(url)])), "input[0].content[0]");
    }
}

#[test]
fn the_scheme_rule_applies_in_prefetch_mode_too_and_beats_the_route_check() {
    for setup in [Setup::prefetch(), Setup::prefetch().with_route()] {
        assert_media_not_found(
            setup.check(json!([image("http://example.com/a.png")])),
            "input[0].content[0]",
        );
        assert_media_not_found(
            setup.check(json!([file_url("https://user@example.com/a.pdf")])),
            "input[0].content[0]",
        );
    }
}

#[test]
fn the_param_names_the_item_and_the_part() {
    let mut value = serde_json::to_value(body("openai::gpt-4o")).expect("body");
    value["input"] = json!([
        { "type": "message", "role": "user", "content": [image(PNG_URI)] },
        { "type": "message", "role": "user", "content": [
            { "type": "input_text", "text": "x" },
            image(PNG_URI),
            image("http://bad.example/a.png")
        ]}
    ]);
    let request: CreateResponseBody = serde_json::from_value(value).expect("request");
    assert_media_not_found(
        Setup::url_forward().check_request(request),
        "input[1].content[2]",
    );
}

#[test]
fn the_error_never_echoes_the_url_or_the_data() {
    let err = Setup::url_forward()
        .check(json!([image("http://secret-host.example/token=abc")]))
        .expect_err("rejected");
    let text = format!("{err} {err:?}");
    assert!(!text.contains("secret-host"), "{text}");
    assert!(!text.contains("token=abc"), "{text}");

    let err = Setup::url_forward()
        .check(json!([image("data:image/svg+xml;base64,SECRETPAYLOAD")]))
        .expect_err("rejected");
    let text = format!("{err} {err:?}");
    assert!(!text.contains("SECRETPAYLOAD"), "{text}");
}

// ── prefetch mode without / with a media route ──────────────────────────────

#[test]
fn prefetch_without_a_route_rejects_https_urls_as_media_url() {
    let setup = Setup::prefetch();
    assert_capability(
        setup.check(json!([image("https://example.com/a.png")])),
        "media_url",
    );
    assert_capability(
        setup.check(json!([file_url("https://example.com/a.pdf")])),
        "media_url",
    );
}

#[test]
fn prefetch_with_a_route_accepts_https_urls() {
    let setup = Setup::prefetch().with_route();
    assert_ok(setup.check(json!([image("https://example.com/a.png")])));
    assert_ok(setup.check(json!([file_url("https://example.com/a.pdf")])));
}

#[test]
fn prefetch_without_a_route_still_accepts_inline_media() {
    let setup = Setup::prefetch();
    assert_ok(setup.check(json!([image(PNG_URI)])));
    assert_ok(setup.check(json!([file_url(&uri("application/pdf", 4))])));
    assert_ok(setup.check(json!([file_data(&b64(4), Some("a.pdf"))])));
    assert_ok(setup.check(json!([file_data(&uri("text/plain", 4), None)])));
}

#[test]
fn prefetch_leaves_conversion_to_the_resolver() {
    // The checker only validates; the request reaches the resolver intact.
    let original = request(json!([file_data(&b64(4), Some("a.pdf"))]));
    let effective = Setup::prefetch()
        .check_request(original.clone())
        .expect("ok");
    assert_eq!(effective.body, original);
}

// ── data URIs ───────────────────────────────────────────────────────────────

#[test]
fn allowed_image_data_uris_pass_in_both_modes() {
    for setup in [Setup::url_forward(), Setup::prefetch()] {
        for mime in [
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "IMAGE/PNG",
        ] {
            assert_ok(setup.check(json!([image(&uri(mime, 4))])));
        }
        assert_ok(setup.check(json!([image(&format!(
            "data:image/png;name=a.png;base64,{}",
            b64(4)
        ))])));
    }
}

#[test]
fn image_data_uris_outside_the_image_allow_list_are_media_not_found() {
    for setup in [Setup::url_forward(), Setup::prefetch()] {
        for mime in [
            "image/svg+xml",
            "image/bmp",
            "application/pdf",
            "text/plain",
            "image/pngx",
        ] {
            assert_media_not_found(
                setup.check(json!([image(&uri(mime, 4))])),
                "input[0].content[0]",
            );
        }
    }
}

#[test]
fn file_data_uris_follow_the_file_allow_list() {
    for setup in [Setup::url_forward(), Setup::prefetch()] {
        for mime in ["application/pdf", "text/plain", "text/csv", "text/markdown"] {
            assert_ok(setup.check(json!([file_url(&uri(mime, 4))])));
            assert_ok(setup.check(json!([file_data(&uri(mime, 4), None)])));
        }
        for mime in ["application/zip", "image/png", "application/json"] {
            assert_media_not_found(
                setup.check(json!([file_url(&uri(mime, 4))])),
                "input[0].content[0]",
            );
            assert_media_not_found(
                setup.check(json!([file_data(&uri(mime, 4), Some("a.pdf"))])),
                "input[0].content[0]",
            );
        }
    }
}

#[test]
fn the_allow_lists_come_from_configuration() {
    let mut setup = Setup::url_forward();
    setup.media.allowed_image_types = vec!["image/png".to_owned()];
    setup.media.allowed_file_types = vec!["application/json".to_owned(), "image/*".to_owned()];
    assert_ok(setup.check(json!([image(&uri("image/png", 4))])));
    assert_media_not_found(
        setup.check(json!([image(&uri("image/jpeg", 4))])),
        "input[0].content[0]",
    );
    assert_ok(setup.check(json!([file_url(&uri("application/json", 4))])));
    assert_ok(setup.check(json!([file_url(&uri("image/webp", 4))])));
    assert_media_not_found(
        setup.check(json!([file_url(&uri("application/pdf", 4))])),
        "input[0].content[0]",
    );
}

#[test]
fn malformed_data_uris_are_media_not_found() {
    let payload = b64(4);
    for setup in [Setup::url_forward(), Setup::prefetch()] {
        for bad in [
            "data:".to_owned(),
            "data:image/png;base64".to_owned(),
            format!("data:image/png,{payload}"),
            format!("data:;base64,{payload}"),
            "data:image/png;base64,".to_owned(),
            "data:image/png;base64,@@@@".to_owned(),
            "data:image/png;base64,iVBORw0KGgo".to_owned(), // missing padding
            "data:image/png;base64,iVBO Rw0KGgo=".to_owned(), // whitespace
            "data:image/png;base64,iVBORw0KGgp=".to_owned(), // trailing bits
        ] {
            assert_media_not_found(setup.check(json!([image(&bad)])), "input[0].content[0]");
        }
        assert_media_not_found(
            setup.check(json!([file_url("data:application/pdf;base64,@@@@")])),
            "input[0].content[0]",
        );
        assert_media_not_found(
            setup.check(json!([file_data("data:application/pdf;base64,@@@@", None)])),
            "input[0].content[0]",
        );
    }
}

// ── plain base64 file_data ──────────────────────────────────────────────────

#[test]
fn plain_base64_file_data_takes_its_type_from_the_filename() {
    for setup in [Setup::url_forward(), Setup::prefetch()] {
        for name in ["a.pdf", "A.PDF", "dir/report.pdf", "a.txt", "a.md", "a.csv"] {
            assert_ok(setup.check(json!([file_data(&b64(4), Some(name))])));
        }
    }
}

#[test]
fn plain_base64_file_data_with_an_unknown_type_is_media_not_found() {
    let payload = b64(4);
    for setup in [Setup::url_forward(), Setup::prefetch()] {
        for name in [None, Some("a.exe"), Some("pdf"), Some("a."), Some("a.zip")] {
            assert_media_not_found(
                setup.check(json!([file_data(&payload, name)])),
                "input[0].content[0]",
            );
        }
    }
}

#[test]
fn a_filename_type_must_be_in_the_file_allow_list_too() {
    // `.json` maps to application/json, not allowed by default.
    let payload = b64(4);
    assert_media_not_found(
        Setup::url_forward().check(json!([file_data(&payload, Some("a.json"))])),
        "input[0].content[0]",
    );
    let mut setup = Setup::url_forward();
    setup
        .media
        .allowed_file_types
        .push("application/json".to_owned());
    assert_ok(setup.check(json!([file_data(&payload, Some("a.json"))])));
}

#[test]
fn invalid_plain_base64_file_data_is_media_not_found() {
    for bad in ["@@@@", "aGk", "aG k=", "aGk=\n", "", "aGl="] {
        assert_media_not_found(
            Setup::url_forward().check(json!([file_data(bad, Some("a.pdf"))])),
            "input[0].content[0]",
        );
    }
}

// ── inline size limit ───────────────────────────────────────────────────────

#[test]
fn the_inline_limit_is_on_the_decoded_size_and_inclusive() {
    for setup in [
        Setup::url_forward().max_inline(10),
        Setup::prefetch().max_inline(10),
    ] {
        // 10 bytes encode to 16 base64 characters: over 10 as text, fine decoded.
        assert_ok(setup.check(json!([image(&uri("image/png", 10))])));
        assert_ok(setup.check(json!([file_url(&uri("application/pdf", 10))])));
        assert_ok(setup.check(json!([file_data(&uri("application/pdf", 10), None)])));
        assert_ok(setup.check(json!([file_data(&b64(10), Some("a.pdf"))])));

        assert_too_large(setup.check(json!([image(&uri("image/png", 11))])));
        assert_too_large(setup.check(json!([file_url(&uri("application/pdf", 11))])));
        assert_too_large(setup.check(json!([file_data(&uri("application/pdf", 11), None)])));
        assert_too_large(setup.check(json!([file_data(&b64(11), Some("a.pdf"))])));
    }
}

#[test]
fn the_limit_applies_to_each_part_not_to_their_sum() {
    assert_ok(Setup::url_forward().max_inline(10).check(json!([
        image(&uri("image/png", 10)),
        image(&uri("image/png", 10)),
        file_data(&b64(10), Some("a.txt"))
    ])));
}

#[test]
fn the_default_inline_limit_is_20_mib() {
    let setup = Setup::url_forward();
    let limit = 20 * 1024 * 1024;
    assert_ok(setup.check(json!([image(&uri("image/png", limit))])));
    assert_too_large(setup.check(json!([image(&uri("image/png", limit + 1))])));
}

#[test]
fn an_oversized_part_is_checked_against_its_own_param_only_for_inline_data() {
    // URLs have no inline size: a long URL is not an inline media part.
    let url = format!("https://example.com/{}", "a".repeat(100));
    assert_ok(
        Setup::url_forward()
            .max_inline(10)
            .check(json!([image(&url)])),
    );
}

// ── ordering ────────────────────────────────────────────────────────────────

#[test]
fn the_first_failing_part_wins() {
    let setup = Setup::url_forward();
    assert_media_not_found(
        setup.check(json!([
            image("http://example.com/a.png"),
            { "type": "input_audio", "data": "aGk=" }
        ])),
        "input[0].content[0]",
    );
    assert_capability(
        setup.check(json!([
            { "type": "input_audio", "data": "aGk=" },
            image("http://example.com/a.png")
        ])),
        "audio_input",
    );
}

#[test]
fn media_failures_come_before_the_parameter_policy() {
    // `temperature` is unsupported by no rule here; the point is that the
    // media error is reported by `check` before anything is dropped.
    let mut value =
        serde_json::to_value(request(json!([image("http://example.com/a.png")]))).expect("body");
    value["temperature"] = json!(0.5);
    let request: CreateResponseBody = serde_json::from_value(value).expect("request");
    assert_media_not_found(
        Setup::url_forward().check_request(request),
        "input[0].content[0]",
    );
}

#[test]
fn text_and_unknown_parts_are_not_media() {
    assert_ok(Setup::prefetch().check(json!([
        { "type": "input_text", "text": "hello" },
        { "type": "acme:widget", "payload": 1 }
    ])));
}

// ── malformed core-typed parts and items (S6-001) ───────────────────────────

fn request_items(items: serde_json::Value) -> CreateResponseBody {
    let mut value = serde_json::to_value(body("openai::gpt-4o")).expect("body serializes");
    value["input"] = items;
    serde_json::from_value(value).expect("request deserializes")
}

fn assert_validation(
    result: Result<EffectiveRequest, LlmGatewayError>,
    message: &str,
    param: &str,
) {
    let err = result.expect_err("check must fail");
    assert!(
        matches!(&err, LlmGatewayError::Validation { param: Some(p), .. } if p == param),
        "expected validation_error({param}), got {err:?}"
    );
    assert_eq!(
        err.to_string(),
        format!("validation error: {message}"),
        "fixed message, no echo"
    );
}

#[test]
fn the_sdk_keeps_a_malformed_core_typed_part_or_item_as_an_extension() {
    use llm_gateway_sdk::models::items::InputContent;
    use llm_gateway_sdk::{InputContentPart, InputItem, ResponseInput};

    let parts = request(json!([
        { "type": "input_image", "image_url": PNG_URI, "detail": "bogus" },
        { "type": "input_audio", "data": "AAAA", "format": "bogus" },
    ]));
    let Some(ResponseInput::Items(items)) = &parts.input else {
        panic!("items expected");
    };
    let InputItem::Message(message) = &items[0] else {
        panic!("message expected");
    };
    let InputContent::Parts(parts) = &message.content else {
        panic!("parts expected");
    };
    assert!(matches!(parts[0], InputContentPart::Other(_)));
    assert!(matches!(parts[1], InputContentPart::Other(_)));

    let items = request_items(json!([
        { "type": "message", "role": "bogus", "content": "hi" },
        { "type": "item_reference" },
        { "type": "function_call", "call_id": "c1" },
    ]));
    let Some(ResponseInput::Items(items)) = &items.input else {
        panic!("items expected");
    };
    assert!(items.iter().all(|item| matches!(item, InputItem::Other(_))));
}

#[test]
fn a_malformed_core_typed_part_is_rejected_with_its_path() {
    for setup in [Setup::url_forward(), Setup::prefetch().with_route()] {
        for bad in [
            json!({ "type": "input_image", "image_url": PNG_URI, "detail": "bogus" }),
            json!({ "type": "input_audio", "data": "AAAA", "format": "bogus" }),
            json!({ "type": "input_text" }),
            json!({ "type": "input_file", "file_data": 5 }),
        ] {
            assert_validation(
                setup.check(json!([{ "type": "input_text", "text": "hi" }, bad])),
                "malformed content part",
                "input[0].content[1]",
            );
        }
    }
}

#[test]
fn a_malformed_core_typed_item_is_rejected_with_its_path() {
    for bad in [
        json!({ "type": "message", "role": "bogus", "content": "hi" }),
        json!({ "type": "item_reference" }),
        json!({ "type": "function_call", "call_id": "c1" }),
        json!({ "type": "function_call_output", "call_id": 1 }),
        json!({ "type": "reasoning", "id": 1 }),
    ] {
        let items = json!([
            { "type": "message", "role": "user", "content": "hi" },
            bad
        ]);
        assert_validation(
            Setup::url_forward().check_request(request_items(items)),
            "malformed input item",
            "input[1]",
        );
    }
}

#[test]
fn a_malformed_part_is_rejected_before_media_checks_of_later_parts() {
    assert_validation(
        Setup::url_forward().check(json!([
            { "type": "input_image", "detail": "bogus" },
            image("http://example.com/a.png"),
        ])),
        "malformed content part",
        "input[0].content[0]",
    );
}

#[test]
fn unknown_and_provider_namespaced_types_stay_allowed() {
    let setup = Setup::url_forward();
    assert_ok(setup.check(json!([
        { "type": "my_provider:thing", "data": 1 },
        { "type": "brand_new_part" },
        { "no_type": true },
    ])));
    assert_ok(setup.check_request(request_items(json!([
        { "type": "message", "role": "user", "content": "hi" },
        { "type": "openai:web_search_call", "id": "x" },
    ]))));
}

// ── max_media_fetches (S6-003) ──────────────────────────────────────────────

fn setup_with_fetch_limit(setup: Setup, limit: usize) -> Setup {
    let mut setup = setup;
    setup.limits.max_media_fetches = limit;
    setup
}

fn urls(count: usize) -> serde_json::Value {
    serde_json::Value::Array(
        (0..count)
            .map(|n| image(&format!("https://example.com/{n}.png")))
            .collect(),
    )
}

#[test]
fn more_fetchable_urls_than_max_media_fetches_is_a_validation_error() {
    let setup = setup_with_fetch_limit(Setup::prefetch().with_route(), 2);
    assert_ok(setup.check(urls(2)));
    assert_too_large(setup.check(urls(3)));
}

#[test]
fn only_urls_that_would_be_fetched_count_towards_max_media_fetches() {
    // UrlForward: nothing is fetched.
    assert_ok(setup_with_fetch_limit(Setup::url_forward(), 1).check(urls(5)));

    // Inline data URIs and a `file_url` next to `file_data` are not fetched.
    let setup = setup_with_fetch_limit(Setup::prefetch().with_route(), 1);
    assert_ok(setup.check(json!([
        image(PNG_URI),
        image(PNG_URI),
        {
            "type": "input_file",
            "file_data": "aGk=",
            "filename": "a.pdf",
            "file_url": "https://example.com/a.pdf"
        },
        image("https://example.com/a.png"),
    ])));
}

#[test]
fn the_default_max_media_fetches_is_16() {
    let setup = Setup::prefetch().with_route();
    assert_ok(setup.check(urls(16)));
    assert_too_large(setup.check(urls(17)));
}
