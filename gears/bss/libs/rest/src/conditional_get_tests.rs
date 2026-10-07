#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{CacheHeaders, PRIVATE_REVALIDATE, matches_if_none_match, respond, weak_etag};
use http::{HeaderMap, HeaderValue, header};

fn header(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).expect("visible ASCII header")
}

fn opaque(tag: &HeaderValue) -> String {
    tag.to_str()
        .expect("ascii tag")
        .trim_start_matches("W/\"")
        .trim_end_matches('"')
        .to_owned()
}

#[test]
fn weak_etag_is_stable_and_changes_when_one_byte_changes() {
    let same = weak_etag(br#"{"n":1}"#);
    let again = weak_etag(br#"{"n":1}"#);
    let changed = weak_etag(br#"{"n":2}"#);
    assert_eq!(same, again);
    assert_ne!(same, changed);
    let text = same.to_str().expect("ascii tag");
    assert!(text.starts_with("W/\""), "{text}");
    assert!(text.ends_with('"'), "{text}");
    assert_eq!(opaque(&same).len(), 22);
}

#[test]
fn matches_if_none_match_accepts_star_a_list_and_a_strong_tag() {
    let tag = header("W/\"x\"");
    assert!(matches_if_none_match(Some(&header("*")), &tag));
    assert!(matches_if_none_match(
        Some(&header("\"no\", W/\"x\", \"later\"")),
        &tag
    ));
    assert!(matches_if_none_match(Some(&header("W/\"x\"")), &tag));
    assert!(matches_if_none_match(Some(&header("\"x\"")), &tag));
    assert!(!matches_if_none_match(None, &tag));
    assert!(!matches_if_none_match(Some(&header("\"y\"")), &tag));
    assert!(!matches_if_none_match(
        Some(&header("W/\"other\"")),
        &weak_etag(b"body")
    ));
}

/// A comma inside a quoted tag belongs to that tag: the list splits only on the commas between
/// elements. A bare element is not an entity-tag and matches nothing, not even its own text.
#[test]
fn a_quoted_comma_stays_in_its_tag_and_a_bare_element_matches_nothing() {
    let comma = header("W/\"a,b\"");
    assert!(matches_if_none_match(Some(&header("W/\"a,b\"")), &comma));
    assert!(matches_if_none_match(Some(&header("\"a,b\"")), &comma));
    assert!(matches_if_none_match(
        Some(&header("\"x\", W/\"a,b\", \"y,z\"")),
        &comma
    ));
    assert!(!matches_if_none_match(
        Some(&header("\"a\", \"b\"")),
        &comma
    ));

    let tag = header("W/\"x\"");
    assert!(!matches_if_none_match(Some(&header("x")), &tag));
    assert!(!matches_if_none_match(Some(&header("W/x")), &tag));
    assert!(matches_if_none_match(Some(&header("x, W/\"x\"")), &tag));
}

proptest::proptest! {
    /// Any visible-ASCII header is read without a panic, and one with no quote and no `*` holds
    /// no entity-tag and matches nothing.
    #[test]
    fn any_visible_ascii_header_is_read_without_a_panic(raw in "[ -~]{0,80}") {
        let value = header(&raw);
        let served = matches_if_none_match(Some(&value), &weak_etag(raw.as_bytes()));
        let other = matches_if_none_match(Some(&value), &header("W/\"x\""));
        if !raw.contains('"') && !raw.contains('*') {
            proptest::prop_assert!(!served && !other, "{raw}");
        }
    }

    /// The served tag matches wherever it stands in a list of other weak or strong tags, commas
    /// inside them included.
    #[test]
    fn the_served_tag_matches_anywhere_in_a_list(
        body in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
        others in proptest::collection::vec(("[!#-~]{0,12}", proptest::bool::ANY), 0..6),
        at in 0usize..7,
    ) {
        let tag = weak_etag(&body);
        let mut elements: Vec<String> = others
            .iter()
            .map(|(opaque, weak)| format!("{}\"{opaque}\"", if *weak { "W/" } else { "" }))
            .collect();
        let at = at.min(elements.len());
        elements.insert(at, tag.to_str().expect("ascii tag").to_owned());
        proptest::prop_assert!(matches_if_none_match(
            Some(&header(&elements.join(", "))),
            &tag
        ));
    }
}

#[tokio::test]
async fn respond_answers_304_with_an_empty_body_or_200_with_the_json() {
    let value = serde_json::json!({"n": 1});
    let first = respond(&HeaderMap::new(), &value, PRIVATE_REVALIDATE);
    assert_eq!(first.status(), http::StatusCode::OK);
    assert_eq!(
        first.headers().get(header::CACHE_CONTROL).expect("cache"),
        "private, no-cache"
    );
    assert_eq!(
        first.headers().get(header::CONTENT_TYPE).expect("type"),
        "application/json"
    );
    let etag = first.headers().get(header::ETAG).expect("etag").clone();
    let body = axum::body::to_bytes(first.into_body(), 64 * 1024)
        .await
        .expect("body");
    assert_eq!(body.as_ref(), serde_json::to_vec(&value).expect("json"));
    assert_eq!(etag, weak_etag(&body));

    let mut matched = HeaderMap::new();
    matched.insert(header::IF_NONE_MATCH, etag.clone());
    let not_modified = respond(&matched, &value, PRIVATE_REVALIDATE);
    assert_eq!(not_modified.status(), http::StatusCode::NOT_MODIFIED);
    assert_eq!(not_modified.headers().get(header::ETAG), Some(&etag));
    assert_eq!(
        not_modified
            .headers()
            .get(header::CACHE_CONTROL)
            .expect("cache"),
        "private, no-cache"
    );
    let empty = axum::body::to_bytes(not_modified.into_body(), 64 * 1024)
        .await
        .expect("body");
    assert!(empty.is_empty());

    let mut listed = HeaderMap::new();
    let etag_text = etag.to_str().expect("ascii tag");
    listed.insert(
        header::IF_NONE_MATCH,
        header(&format!("\"stale\", {etag_text}")),
    );
    let short = CacheHeaders {
        cache_control: "private, max-age=60",
    };
    let from_list = respond(&listed, &value, short);
    assert_eq!(from_list.status(), http::StatusCode::NOT_MODIFIED);
    assert_eq!(
        from_list
            .headers()
            .get(header::CACHE_CONTROL)
            .expect("cache"),
        "private, max-age=60"
    );

    let mut other = HeaderMap::new();
    other.insert(header::IF_NONE_MATCH, header("\"unrelated\""));
    let fresh = respond(&other, &value, PRIVATE_REVALIDATE);
    assert_eq!(fresh.status(), http::StatusCode::OK);
    let fresh_body = axum::body::to_bytes(fresh.into_body(), 64 * 1024)
        .await
        .expect("body");
    assert_eq!(fresh_body.as_ref(), body.as_ref());
}

/// A value whose `Serialize` always fails.
struct Unserializable;

impl serde::Serialize for Unserializable {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("secret field text"))
    }
}

/// A body that does not serialize is the canonical 500 `Problem`, never conditional, and one
/// error line that names the type and carries no error text.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_value_that_does_not_serialize_is_the_canonical_500_problem() {
    let mut star = HeaderMap::new();
    star.insert(header::IF_NONE_MATCH, header("*"));
    for request in [HeaderMap::new(), star] {
        let response = respond(&request, &Unserializable, PRIVATE_REVALIDATE);
        assert_eq!(response.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.headers().get(header::ETAG).is_none());
        assert!(response.headers().get(header::CACHE_CONTROL).is_none());
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .expect("a Problem names its type"),
            "application/problem+json"
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("a Problem");
        assert_eq!(problem["status"], 500, "{problem}");
        assert!(!body.windows(6).any(|w| w == b"secret"), "{problem}");
    }
    assert!(logs_contain("did not serialize"));
    assert!(logs_contain("Unserializable"));
    assert!(!logs_contain("secret field text"));
}
