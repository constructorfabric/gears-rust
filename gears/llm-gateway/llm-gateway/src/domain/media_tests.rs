//! Media Resolver (DESIGN §3.2, seq `media-input`): data URI helpers and the
//! resolution of media parts per plugin media input mode.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use llm_gateway_sdk::{CreateResponseBody, LlmGatewayError, MediaInputMode};
use serde_json::json;

use super::{
    MediaResolver, data_uri, decoded_base64_len, has_https_scheme_without_user_info, mime_allowed,
    mime_for_filename, parse_data_uri,
};
use crate::config::MediaConfig;
use crate::domain::ports::{FetchedMedia, MediaFetchLimits, MediaFetcher};
use crate::domain::test_support::{CallLog, FakeMediaFetcher, PNG_BYTES, body, png_media};
use crate::infra::media_fetcher::NoRouteMediaFetcher;

const PNG_B64: &str = "iVBORw0KGgo=";

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

// ── parse_data_uri ──────────────────────────────────────────────────────────

#[test]
fn parses_a_base64_data_uri() {
    let parsed = parse_data_uri("data:image/png;base64,iVBORw0KGgo=").expect("parses");
    assert_eq!(parsed.mime, "image/png");
    assert_eq!(parsed.payload, PNG_B64);
}

#[test]
fn the_scheme_is_case_insensitive_and_the_mime_is_lowercased() {
    let parsed = parse_data_uri("DATA:Image/PNG;base64,AAAA").expect("parses");
    assert_eq!(parsed.mime, "image/png");
    assert_eq!(parsed.payload, "AAAA");
}

#[test]
fn mime_parameters_are_dropped() {
    let parsed = parse_data_uri("data:text/plain;charset=utf-8;base64,aGk=").expect("parses");
    assert_eq!(parsed.mime, "text/plain");
    assert_eq!(parsed.payload, "aGk=");
}

#[test]
fn the_payload_is_everything_after_the_first_comma() {
    let parsed = parse_data_uri("data:image/png;base64,AA,AA").expect("parses");
    assert_eq!(parsed.payload, "AA,AA");
}

#[test]
fn malformed_data_uris_do_not_parse() {
    for value in [
        "",
        "data:",
        "data:image/png;base64",         // no comma
        "data:image/png,iVBORw0KGgo=",   // not base64
        "data:image/png;charset=x,AAAA", // not base64
        "data:;base64,AAAA",             // empty mime
        "data:image;base64,AAAA",        // not type/subtype
        "data:image/png;base64,",        // empty payload
        "https://example.com/a.png",
        "image/png;base64,AAAA",       // no scheme
        " data:image/png;base64,AAAA", // leading space
    ] {
        assert_eq!(parse_data_uri(value), None, "{value:?}");
    }
}

#[test]
fn a_mime_type_must_be_two_tokens_without_wildcards_or_punctuation() {
    for value in [
        "data:text/*;base64,AAAA",
        "data:*/*;base64,AAAA",
        "data:text/plain,x;base64,AAAA",
        "data:te\"xt/plain;base64,AAAA",
        "data:text/pl\\ain;base64,AAAA",
        "data:text/pl\"ain;base64,AAAA",
        "data:text/plain/x;base64,AAAA",
        "data:text/ plain;base64,AAAA",
        "data:text/(plain);base64,AAAA",
    ] {
        assert_eq!(parse_data_uri(value), None, "{value:?}");
    }
}

// ── decoded_base64_len ──────────────────────────────────────────────────────

#[test]
fn decoded_len_of_canonical_base64() {
    assert_eq!(decoded_base64_len("aGk="), Some(2));
    assert_eq!(decoded_base64_len("aGkh"), Some(3));
    assert_eq!(decoded_base64_len("YQ=="), Some(1));
    assert_eq!(decoded_base64_len(PNG_B64), Some(PNG_BYTES.len()));
    assert_eq!(
        decoded_base64_len(&STANDARD.encode(vec![7u8; 1000])),
        Some(1000)
    );
}

#[test]
fn decoded_len_rejects_everything_but_strict_canonical_base64() {
    for value in [
        "", "aGk",      // missing padding
        "aGk==",    // too much padding
        "a$k=",     // bad alphabet
        "aG k=",    // whitespace
        "aGk=\n",   // trailing newline
        "aG\nk=",   // line break
        "aGl=",     // non-canonical trailing bits
        "ab-_",     // url-safe alphabet
        "aGk=aGk=", // padding in the middle
        "A",        // truncated quantum
    ] {
        assert_eq!(decoded_base64_len(value), None, "{value:?}");
    }
}

// ── mime_allowed ────────────────────────────────────────────────────────────

#[test]
fn mime_allow_list_matches_exactly_or_by_wildcard() {
    let list = strings(&["application/pdf", "text/*"]);
    assert!(mime_allowed(&list, "application/pdf"));
    assert!(mime_allowed(&list, "APPLICATION/PDF"));
    assert!(mime_allowed(&list, "text/plain"));
    assert!(mime_allowed(&list, "text/csv"));
    assert!(!mime_allowed(&list, "application/zip"));
    assert!(!mime_allowed(&list, "application/pdfx"));
    assert!(!mime_allowed(&list, "textual/plain"));
    assert!(!mime_allowed(&list, "text"));
    assert!(!mime_allowed(&[], "text/plain"));
}

// ── mime_for_filename ───────────────────────────────────────────────────────

#[test]
fn filename_extension_gives_the_mime_type() {
    for (name, mime) in [
        ("a.pdf", "application/pdf"),
        ("A.PDF", "application/pdf"),
        ("dir/sub.v2/report.pdf", "application/pdf"),
        ("a.txt", "text/plain"),
        ("a.md", "text/markdown"),
        ("a.csv", "text/csv"),
        ("a.json", "application/json"),
        ("a.tar.pdf", "application/pdf"),
    ] {
        assert_eq!(mime_for_filename(name), Some(mime), "{name}");
    }
    for name in ["", "pdf", "a.", "a.exe", "a.pdf.exe", ".pdf/x", "a.pdf "] {
        assert_eq!(mime_for_filename(name), None, "{name:?}");
    }
}

// ── URL rule ────────────────────────────────────────────────────────────────

#[test]
fn url_rule_accepts_https_without_user_info() {
    for url in [
        "https://example.com/a.png",
        "HTTPS://EXAMPLE.COM/a.png",
        "Https://example.com",
        "https://example.com:8443/a.png?x=1#f",
        "https://[2001:db8::1]/a.png",
        "https://203.0.113.7/a.png",
        "https://example.com/a@b.png",
        "https://example.com/?u=a@b",
        "https://[2001:db8::1]:8443/a.png",
        "https://example.com:65535/a.png",
        "https://example.com:1/a.png",
        "https://xn--bcher-kva.example/a.png",
    ] {
        assert!(has_https_scheme_without_user_info(url), "{url}");
    }
}

#[test]
fn url_rule_rejects_everything_else() {
    for url in [
        "",
        "http://example.com/a.png",
        "ftp://example.com/a.png",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:image/png;base64,AAAA",
        "//example.com/a.png",
        "example.com/a.png",
        "/a.png",
        "https://user@example.com/a.png",
        "https://user:pw@example.com/a.png",
        "https://@example.com/a.png",
        "https:///a.png",
        "https://",
        " https://example.com/a.png",
        "https://example.com/a b.png",
        "https:example.com",
        "https://example.com\\a.png",
        "https://example.com/a\\b.png",
        "https://example.com/a\u{0}b.png",
        "https://example.com/a\u{7f}b.png",
        "https://example.com/a\nb.png",
        "https://[::1/a.png",
        "https://[::1]x/a.png",
        "https://[::1]:/a.png",
        "https://[::1]:abc/a.png",
        "https://[]/a.png",
        "https://example.com:/a.png",
        "https://example.com:0/a.png",
        "https://example.com:65536/a.png",
        "https://example.com:99999999999999999999/a.png",
        "https://example.com:80a/a.png",
        "https://example.com:+80/a.png",
        "https://exam]ple.com/a.png",
        "https://b\u{fc}cher.example/a.png",
        "https://example.com:8\u{663}/a.png",
    ] {
        assert!(!has_https_scheme_without_user_info(url), "{url:?}");
    }
}

// ── data_uri ────────────────────────────────────────────────────────────────

#[test]
fn data_uri_encodes_standard_base64() {
    assert_eq!(
        data_uri("image/png", PNG_BYTES),
        format!("data:image/png;base64,{PNG_B64}")
    );
    assert_eq!(data_uri("text/plain", b"hi"), "data:text/plain;base64,aGk=");
}

// ── resolver ────────────────────────────────────────────────────────────────

#[allow(clippy::needless_pass_by_value)] // call sites build the parts inline with `json!`
fn request(content: serde_json::Value) -> CreateResponseBody {
    let mut value = serde_json::to_value(body("openai::gpt-4o")).expect("body serializes");
    value["input"] = json!([{ "type": "message", "role": "user", "content": content }]);
    serde_json::from_value(value).expect("request deserializes")
}

#[allow(clippy::needless_pass_by_value)] // call sites build the parts inline with `json!`
fn two_items(first: serde_json::Value, second: serde_json::Value) -> CreateResponseBody {
    let mut value = serde_json::to_value(body("openai::gpt-4o")).expect("body serializes");
    value["input"] = json!([
        { "type": "message", "role": "user", "content": first },
        { "type": "message", "role": "user", "content": second }
    ]);
    serde_json::from_value(value).expect("request deserializes")
}

fn as_json(body: &CreateResponseBody) -> serde_json::Value {
    serde_json::to_value(body).expect("serializes")
}

fn content_of(body: &CreateResponseBody, item: usize) -> serde_json::Value {
    as_json(body)["input"][item]["content"].clone()
}

fn routed() -> (MediaResolver, Arc<FakeMediaFetcher>) {
    routed_with(&MediaConfig::default())
}

fn routed_with(config: &MediaConfig) -> (MediaResolver, Arc<FakeMediaFetcher>) {
    let fetcher = FakeMediaFetcher::new(Arc::new(CallLog::default()));
    fetcher.set_route(true);
    (MediaResolver::new(fetcher.clone(), config), fetcher)
}

fn assert_media_not_found(err: &LlmGatewayError, param: &str) {
    assert!(
        matches!(err, LlmGatewayError::MediaNotFound { param: Some(p), .. } if p == param),
        "expected media_not_found({param}), got {err:?}"
    );
    assert_eq!(err.to_string(), "media not found");
}

#[test]
fn has_media_detects_image_and_file_parts_only() {
    assert!(MediaResolver::has_media(&request(json!([
        { "type": "input_text", "text": "x" },
        { "type": "input_image", "image_url": "https://example.com/a.png" }
    ]))));
    assert!(MediaResolver::has_media(&request(json!([
        { "type": "input_file", "file_data": "aGk=", "filename": "a.txt" }
    ]))));
    assert!(!MediaResolver::has_media(&request(json!([
        { "type": "input_text", "text": "x" }
    ]))));
    assert!(!MediaResolver::has_media(&body("openai::gpt-4o")));
}

#[test]
fn has_route_follows_the_fetcher() {
    let (resolver, fetcher) = routed();
    assert!(resolver.has_route());
    fetcher.set_route(false);
    assert!(!resolver.has_route());
    assert!(!MediaResolver::unrouted(&MediaConfig::default()).has_route());
    assert!(
        !MediaResolver::new(Arc::new(NoRouteMediaFetcher), &MediaConfig::default()).has_route()
    );
}

#[tokio::test]
async fn url_forward_leaves_every_part_as_it_is() {
    let (resolver, fetcher) = routed();
    let original = request(json!([
        { "type": "input_text", "text": "look" },
        { "type": "input_image", "image_url": "https://example.com/a.png", "detail": "low" },
        { "type": "input_image", "image_url": format!("data:image/png;base64,{PNG_B64}") },
        { "type": "input_file", "file_url": "https://example.com/a.pdf" },
        { "type": "input_file", "file_data": "aGk=", "filename": "a.txt" },
        { "type": "input_file", "file_data": "data:application/pdf;base64,aGk=", "filename": "a.pdf" }
    ]));
    let outcome = resolver
        .resolve(MediaInputMode::UrlForward, original.clone())
        .await
        .expect("passes");
    assert_eq!(outcome, original);
    assert_eq!(fetcher.calls(), 0, "UrlForward never fetches");
}

#[tokio::test]
async fn prefetch_keeps_inline_image_data_uris_and_never_fetches_them() {
    let (resolver, fetcher) = routed();
    let original = request(json!([
        { "type": "input_image", "image_url": format!("data:image/png;base64,{PNG_B64}"), "detail": "high" }
    ]));
    let outcome = resolver
        .resolve(MediaInputMode::PrefetchRequired, original.clone())
        .await
        .expect("passes");
    assert_eq!(outcome, original);
    assert_eq!(fetcher.calls(), 0);
}

#[tokio::test]
async fn prefetch_converts_plain_base64_file_data_to_a_data_uri_with_the_filename_mime() {
    let (resolver, _fetcher) = routed();
    let outcome = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([
                { "type": "input_file", "file_data": "aGk=", "filename": "notes.PDF" }
            ])),
        )
        .await
        .expect("resolves");
    assert_eq!(
        content_of(&outcome, 0),
        json!([{
            "type": "input_file",
            "filename": "notes.PDF",
            "file_data": "data:application/pdf;base64,aGk="
        }])
    );
}

#[tokio::test]
async fn prefetch_keeps_file_data_that_is_already_a_data_uri() {
    let (resolver, _fetcher) = routed();
    let original = request(json!([
        { "type": "input_file", "file_data": "data:text/plain;base64,aGk=", "filename": "a.txt" }
    ]));
    let outcome = resolver
        .resolve(MediaInputMode::PrefetchRequired, original.clone())
        .await
        .expect("resolves");
    assert_eq!(outcome, original);
}

#[tokio::test]
async fn prefetch_moves_a_file_url_data_uri_into_file_data() {
    let (resolver, fetcher) = routed();
    let outcome = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([
                { "type": "input_file", "file_url": "data:application/pdf;base64,aGk=", "filename": "a.pdf" }
            ])),
        )
        .await
        .expect("resolves");
    assert_eq!(
        content_of(&outcome, 0),
        json!([{
            "type": "input_file",
            "filename": "a.pdf",
            "file_data": "data:application/pdf;base64,aGk="
        }])
    );
    assert_eq!(fetcher.calls(), 0);
}

#[tokio::test]
async fn prefetch_fetches_an_https_image_and_hands_the_plugin_a_data_uri() {
    let (resolver, fetcher) = routed();
    let outcome = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([
                { "type": "input_text", "text": "look" },
                { "type": "input_image", "image_url": "https://example.com/a.png", "detail": "low" }
            ])),
        )
        .await
        .expect("resolves");
    assert_eq!(
        content_of(&outcome, 0),
        json!([
            { "type": "input_text", "text": "look" },
            { "type": "input_image", "image_url": format!("data:image/png;base64,{PNG_B64}"), "detail": "low" }
        ])
    );
    assert_eq!(fetcher.urls(), ["https://example.com/a.png"]);
    let config = MediaConfig::default();
    let limits = fetcher.limits();
    assert_eq!(limits[0].timeout, config.fetch_timeout);
    assert_eq!(limits[0].max_bytes, config.max_bytes);
}

#[tokio::test]
async fn prefetch_fetches_an_https_file_url_into_file_data() {
    let (resolver, fetcher) = routed();
    fetcher.script(|_| {
        Ok(FetchedMedia {
            content_type: "application/pdf".to_owned(),
            bytes: b"hi".to_vec(),
        })
    });
    let outcome = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([
                { "type": "input_file", "file_url": "https://example.com/a.pdf", "filename": "a.pdf" }
            ])),
        )
        .await
        .expect("resolves");
    assert_eq!(
        content_of(&outcome, 0),
        json!([{
            "type": "input_file",
            "filename": "a.pdf",
            "file_data": "data:application/pdf;base64,aGk="
        }])
    );
}

#[tokio::test]
async fn prefetch_resolves_parts_of_every_item_and_keeps_other_items() {
    let (resolver, fetcher) = routed();
    let outcome = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            two_items(
                json!([{ "type": "input_image", "image_url": "https://example.com/1.png" }]),
                json!([
                    { "type": "input_text", "text": "x" },
                    { "type": "input_image", "image_url": "https://example.com/2.png" }
                ]),
            ),
        )
        .await
        .expect("resolves");
    assert_eq!(fetcher.calls(), 2);
    let mut urls = fetcher.urls();
    urls.sort();
    assert_eq!(
        urls,
        ["https://example.com/1.png", "https://example.com/2.png"]
    );
    let expected = format!("data:image/png;base64,{PNG_B64}");
    assert_eq!(content_of(&outcome, 0)[0]["image_url"], expected);
    assert_eq!(content_of(&outcome, 1)[1]["image_url"], expected);
    assert_eq!(content_of(&outcome, 1)[0]["text"], "x");
}

#[tokio::test]
async fn a_text_only_request_is_untouched_in_every_mode() {
    let (resolver, fetcher) = routed();
    for mode in [MediaInputMode::UrlForward, MediaInputMode::PrefetchRequired] {
        let original = body("openai::gpt-4o");
        let outcome = resolver.resolve(mode, original.clone()).await.expect("ok");
        assert_eq!(outcome, original);
    }
    assert_eq!(fetcher.calls(), 0);
}

#[tokio::test]
async fn a_failed_fetch_is_media_not_found_naming_the_part() {
    let (resolver, fetcher) = routed();
    fetcher.script(|url| {
        assert!(url.contains("secret-host"));
        Err(LlmGatewayError::media_not_found())
    });
    let err = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            two_items(
                json!([{ "type": "input_text", "text": "x" }]),
                json!([
                    { "type": "input_text", "text": "y" },
                    { "type": "input_image", "image_url": "https://secret-host.example/a.png" }
                ]),
            ),
        )
        .await
        .expect_err("fetch fails");
    assert_media_not_found(&err, "input[1].content[1]");
    assert!(!format!("{err:?}").contains("secret-host"));
}

#[tokio::test]
async fn any_fetcher_error_is_reported_as_media_not_found() {
    let (resolver, fetcher) = routed();
    fetcher.script(|_| Err(LlmGatewayError::internal("boom: secret-detail")));
    let err = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([{ "type": "input_image", "image_url": "https://example.com/a.png" }])),
        )
        .await
        .expect_err("fetch fails");
    assert_media_not_found(&err, "input[0].content[0]");
    assert!(!format!("{err:?}").contains("secret-detail"));
}

#[tokio::test]
async fn the_scheme_rule_applies_before_any_fetch() {
    let (resolver, fetcher) = routed();
    for url in [
        "http://example.com/a.png",
        "ftp://example.com/a.png",
        "https://user:pw@example.com/a.png",
        "file:///etc/passwd",
    ] {
        let err = resolver
            .resolve(
                MediaInputMode::PrefetchRequired,
                request(json!([{ "type": "input_image", "image_url": url }])),
            )
            .await
            .expect_err(url);
        assert_media_not_found(&err, "input[0].content[0]");
    }
    assert_eq!(fetcher.calls(), 0);
}

#[tokio::test]
async fn a_fetched_content_type_outside_the_allow_list_is_rejected() {
    let (resolver, fetcher) = routed();
    for content_type in ["text/html", "image/svg+xml", "application/pdf", ""] {
        let content_type = content_type.to_owned();
        fetcher.script(move |_| {
            Ok(FetchedMedia {
                content_type: content_type.clone(),
                bytes: b"x".to_vec(),
            })
        });
        let err = resolver
            .resolve(
                MediaInputMode::PrefetchRequired,
                request(
                    json!([{ "type": "input_image", "image_url": "https://example.com/a.png" }]),
                ),
            )
            .await
            .expect_err("type not allowed for images");
        assert_media_not_found(&err, "input[0].content[0]");
    }
}

#[tokio::test]
async fn the_allow_list_is_per_part_kind() {
    let (resolver, fetcher) = routed();
    // An image type is not a valid file type under the defaults.
    let err = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([{ "type": "input_file", "file_url": "https://example.com/a.png" }])),
        )
        .await
        .expect_err("png is not an allowed file type");
    assert_media_not_found(&err, "input[0].content[0]");

    fetcher.script(|_| {
        Ok(FetchedMedia {
            content_type: "text/csv".to_owned(),
            bytes: b"a,b".to_vec(),
        })
    });
    resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([{ "type": "input_file", "file_url": "https://example.com/a.csv" }])),
        )
        .await
        .expect("text/* is an allowed file type");
}

#[tokio::test]
async fn the_content_type_is_compared_without_parameters_and_case() {
    let (resolver, fetcher) = routed();
    fetcher.script(|_| {
        Ok(FetchedMedia {
            content_type: "Image/PNG; charset=binary".to_owned(),
            bytes: PNG_BYTES.to_vec(),
        })
    });
    let outcome = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([{ "type": "input_image", "image_url": "https://example.com/a.png" }])),
        )
        .await
        .expect("resolves");
    assert_eq!(
        content_of(&outcome, 0)[0]["image_url"],
        format!("data:image/png;base64,{PNG_B64}")
    );
}

#[tokio::test]
async fn a_response_over_max_bytes_is_rejected_and_the_limit_is_inclusive() {
    let config = MediaConfig {
        max_bytes: PNG_BYTES.len(),
        ..MediaConfig::default()
    };
    let (resolver, fetcher) = routed_with(&config);
    let image =
        request(json!([{ "type": "input_image", "image_url": "https://example.com/a.png" }]));
    resolver
        .resolve(MediaInputMode::PrefetchRequired, image.clone())
        .await
        .expect("exactly max_bytes passes");
    assert_eq!(fetcher.limits()[0].max_bytes, PNG_BYTES.len());

    fetcher.script(|_| {
        Ok(FetchedMedia {
            content_type: "image/png".to_owned(),
            bytes: vec![0; PNG_BYTES.len() + 1],
        })
    });
    let err = resolver
        .resolve(MediaInputMode::PrefetchRequired, image)
        .await
        .expect_err("over max_bytes");
    assert_media_not_found(&err, "input[0].content[0]");
}

#[tokio::test]
async fn an_empty_response_is_rejected() {
    let (resolver, fetcher) = routed();
    fetcher.script(|_| {
        Ok(FetchedMedia {
            content_type: "image/png".to_owned(),
            bytes: Vec::new(),
        })
    });
    let err = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([{ "type": "input_image", "image_url": "https://example.com/a.png" }])),
        )
        .await
        .expect_err("empty media");
    assert_media_not_found(&err, "input[0].content[0]");
}

#[tokio::test(start_paused = true)]
async fn a_fetch_that_outlives_the_timeout_is_media_not_found() {
    let config = MediaConfig {
        fetch_timeout: Duration::from_secs(5),
        ..MediaConfig::default()
    };
    let (resolver, fetcher) = routed_with(&config);
    fetcher.gate(); // never released
    let started = tokio::time::Instant::now();
    let err = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([{ "type": "input_image", "image_url": "https://example.com/a.png" }])),
        )
        .await
        .expect_err("timed out");
    assert_media_not_found(&err, "input[0].content[0]");
    assert_eq!(started.elapsed(), Duration::from_secs(5));
}

#[tokio::test]
async fn without_a_route_every_url_fails_and_inline_data_still_works() {
    let resolver = MediaResolver::new(Arc::new(NoRouteMediaFetcher), &MediaConfig::default());
    let err = resolver
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([{ "type": "input_image", "image_url": "https://example.com/a.png" }])),
        )
        .await
        .expect_err("no route");
    assert_media_not_found(&err, "input[0].content[0]");

    let unrouted = MediaResolver::unrouted(&MediaConfig::default());
    let err = unrouted
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([{ "type": "input_file", "file_url": "https://example.com/a.pdf" }])),
        )
        .await
        .expect_err("no route");
    assert_media_not_found(&err, "input[0].content[0]");

    unrouted
        .resolve(
            MediaInputMode::PrefetchRequired,
            request(json!([{ "type": "input_file", "file_data": "aGk=", "filename": "a.txt" }])),
        )
        .await
        .expect("inline data needs no route");
}

#[tokio::test]
async fn prefetch_rejects_plain_file_data_with_an_unknown_mime() {
    let (resolver, _fetcher) = routed();
    for part in [
        json!({ "type": "input_file", "file_data": "aGk=" }),
        json!({ "type": "input_file", "file_data": "aGk=", "filename": "a.exe" }),
    ] {
        let err = resolver
            .resolve(MediaInputMode::PrefetchRequired, request(json!([part])))
            .await
            .expect_err("unknown mime");
        assert_media_not_found(&err, "input[0].content[0]");
    }
}

#[test]
fn fetched_media_debug_never_shows_the_bytes() {
    let text = format!("{:?}", png_media());
    assert!(text.contains("image/png"));
    assert!(!text.contains("PNG"), "{text}");
    assert!(!text.contains("137"), "{text}");
}

#[allow(dead_code)]
fn _port_is_object_safe(_: &dyn MediaFetcher) {}

// ── fetched Content-Type must be a single MIME type (S6-004) ────────────────

#[tokio::test]
async fn a_malformed_fetched_content_type_is_rejected_even_when_the_list_matches() {
    for content_type in [
        "text/plain,x",
        "text/plain,",
        "text/*",
        "te\"xt/plain",
        "text/pl\\ain",
        "text",
        "/plain",
        "text/",
    ] {
        let (resolver, fetcher) = routed();
        fetcher.script(move |_| {
            Ok(FetchedMedia {
                content_type: content_type.to_owned(),
                bytes: b"hi".to_vec(),
            })
        });
        let err = resolver
            .resolve(
                MediaInputMode::PrefetchRequired,
                request(json!([{ "type": "input_file", "file_url": "https://example.com/a.txt" }])),
            )
            .await
            .expect_err(content_type);
        assert_media_not_found(&err, "input[0].content[0]");
    }
}

// ── data URI normalization (S6-006) ─────────────────────────────────────────

#[tokio::test]
async fn data_uris_are_normalized_to_the_canonical_header_and_the_payload_is_kept() {
    let (resolver, _fetcher) = routed();
    for (given, expected) in [
        (
            "data:image/png;base64,iVBORw0KGgo=",
            "data:image/png;base64,iVBORw0KGgo=",
        ),
        (
            "DATA:Image/PNG;BASE64,iVBORw0KGgo=",
            "data:image/png;base64,iVBORw0KGgo=",
        ),
        (
            "data:image/png;name=a;base64,iVBORw0KGgo=",
            "data:image/png;base64,iVBORw0KGgo=",
        ),
    ] {
        let outcome = resolver
            .resolve(
                MediaInputMode::PrefetchRequired,
                request(json!([{ "type": "input_image", "image_url": given }])),
            )
            .await
            .expect("resolves");
        assert_eq!(content_of(&outcome, 0)[0]["image_url"], expected, "{given}");
    }
}

// ── total timeout (S6-003) ──────────────────────────────────────────────────

/// Fetcher answering a tiny PNG after `delay`.
struct SlowFetcher {
    delay: Duration,
}

#[async_trait]
impl MediaFetcher for SlowFetcher {
    fn has_route(&self) -> bool {
        true
    }

    async fn fetch(
        &self,
        _url: &str,
        _limits: &MediaFetchLimits,
    ) -> Result<FetchedMedia, LlmGatewayError> {
        tokio::time::sleep(self.delay).await;
        Ok(FetchedMedia {
            content_type: "image/png".to_owned(),
            bytes: PNG_BYTES.to_vec(),
        })
    }
}

fn images(count: usize) -> CreateResponseBody {
    request(serde_json::Value::Array(
        (0..count)
            .map(|n| json!({ "type": "input_image", "image_url": format!("https://example.com/{n}.png") }))
            .collect(),
    ))
}

#[tokio::test(start_paused = true)]
async fn the_total_timeout_bounds_the_whole_resolution_and_names_the_first_unresolved_part() {
    let config = MediaConfig {
        fetch_timeout: Duration::from_secs(10),
        total_timeout: Duration::from_secs(15),
        ..MediaConfig::default()
    };
    // Concurrency 4, 6s per fetch: parts 0..=7 are done at 12s, 8..=11 at 18s.
    let resolver = MediaResolver::new(
        Arc::new(SlowFetcher {
            delay: Duration::from_secs(6),
        }),
        &config,
    );
    let started = tokio::time::Instant::now();
    let err = resolver
        .resolve(MediaInputMode::PrefetchRequired, images(12))
        .await
        .expect_err("total timeout");
    assert_media_not_found(&err, "input[0].content[8]");
    assert_eq!(started.elapsed(), Duration::from_secs(15));
}

#[tokio::test(start_paused = true)]
async fn a_resolution_within_the_total_timeout_succeeds() {
    let config = MediaConfig {
        fetch_timeout: Duration::from_secs(10),
        total_timeout: Duration::from_secs(20),
        ..MediaConfig::default()
    };
    let resolver = MediaResolver::new(
        Arc::new(SlowFetcher {
            delay: Duration::from_secs(6),
        }),
        &config,
    );
    let outcome = resolver
        .resolve(MediaInputMode::PrefetchRequired, images(8))
        .await
        .expect("resolves in 12s");
    assert!(
        content_of(&outcome, 0)
            .as_array()
            .expect("parts")
            .iter()
            .all(|part| part["image_url"]
                .as_str()
                .is_some_and(|uri| uri.starts_with("data:image/png;base64,")))
    );
}
