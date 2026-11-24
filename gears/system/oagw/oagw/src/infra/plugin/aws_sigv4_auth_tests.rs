//! Unit tests of the AWS SigV4 plugin. Signatures are the ones published
//! in the AWS Signature Version 4 test suite (credentials `AKIDEXAMPLE`,
//! 2015-08-30T12:36:00Z, `us-east-1`, service `service`); the suite does not
//! send `X-Amz-Content-Sha256`, so those cases run with
//! `content_sha256_header: false`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use credstore_sdk::test_util::MockCredStoreClient;
use uuid::Uuid;

use super::*;

const AKID: &str = "AKIDEXAMPLE";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
/// `kSigning` for 20150830/us-east-1/service, as hex.
const SIGNING_KEY_HEX: &str = "938127b5336810ddb6a5d6af445fcac9e371f9ed418ed386b022aed82901be75";
const TOKEN: &str = "AQoDYXdzEPT//////////wEXAMPLEtc764bNrC9SAPBSM22wDOk4x4HIZ8j4FZTwdQWLWsKWHGBuFqwAeMicRXmxfpSPfIeoIYRqTflfKD8YUuwthAx7mSEI/qkPpKPi/kMcGdQrmGdeehM4IC1NtBmUpp2wUE8phUZampKsburEDy0KPkyQDYwT7WZ0wq5VSXDvp75YU9HFvlRd8Tx6q6fE8YQcHNVXAkiY9q6d+xo0rKwT38xVqr7ZD0u0iPPkUL64lIZbqBAz+scqKmlzm8FDrypNC9Yjc8fPOLn9FX9KSYvKTr4rvx3iSIlTJabIQwj2ICCR/oLxBA==";
const HOST: &str = "example.amazonaws.com";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// 2015-08-30T12:36:00Z.
const VECTOR_TIME: i64 = 1_440_938_160;

struct FixedClock(OffsetDateTime);

impl SigningClock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

/// The suite's credentials; counts the reads.
struct FixedCredentials {
    secret: &'static str,
    session_token: Option<&'static str>,
    reads: AtomicUsize,
}

#[async_trait]
impl AwsCredentialSource for FixedCredentials {
    async fn credentials(
        &self,
        _: &AwsSigV4AuthConfig,
        _: &SecurityContext,
    ) -> Result<AwsCredentials, PluginError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(AwsCredentials {
            access_key_id: AKID.to_owned(),
            secret_access_key: Zeroizing::new(self.secret.as_bytes().to_vec()),
            session_token: self.session_token.map(|t| Zeroizing::new(t.to_owned())),
        })
    }
}

fn source(session_token: Option<&'static str>) -> Arc<FixedCredentials> {
    Arc::new(FixedCredentials {
        secret: SECRET,
        session_token,
        reads: AtomicUsize::new(0),
    })
}

fn plugin_with(credentials: Arc<FixedCredentials>) -> AwsSigV4AuthPlugin {
    AwsSigV4AuthPlugin::with_sources(
        credentials,
        Arc::new(FixedClock(
            OffsetDateTime::from_unix_timestamp(VECTOR_TIME).unwrap(),
        )),
    )
}

fn plugin() -> AwsSigV4AuthPlugin {
    plugin_with(source(None))
}

fn security_context() -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(Uuid::new_v4())
        .subject_id(Uuid::new_v4())
        .build()
        .unwrap()
}

fn config(entries: &[(&str, &str)]) -> HashMap<String, String> {
    let mut config: HashMap<String, String> = [
        ("region", "us-east-1"),
        ("service", "service"),
        ("access_key_id_ref", "cred://aws-akid"),
        ("secret_access_key_ref", "cred://aws-secret"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    for (k, v) in entries {
        config.insert((*k).to_owned(), (*v).to_owned());
    }
    config
}

/// The test-suite config: no `X-Amz-Content-Sha256`, the given path encoding.
fn suite_config(uri_encoding: &str) -> HashMap<String, String> {
    config(&[
        ("content_sha256_header", "false"),
        ("uri_encoding", uri_encoding),
    ])
}

fn request(
    method: &str,
    path: &str,
    query: Option<&str>,
    headers: &[(&str, &[u8])],
    body: SigningBody,
    config: HashMap<String, String>,
) -> SigningContext {
    let mut lines = vec![("host".to_owned(), HOST.as_bytes().to_vec())];
    lines.extend(headers.iter().map(|(n, v)| ((*n).to_owned(), v.to_vec())));
    SigningContext {
        method: method.to_owned(),
        scheme: "https".to_owned(),
        host: HOST.to_owned(),
        port: 443,
        path: path.to_owned(),
        path_rewritten: false,
        query: query.map(str::to_owned),
        headers: lines,
        body,
        config,
        security_context: security_context(),
    }
}

fn empty() -> SigningBody {
    SigningBody::Buffered(Bytes::new())
}

fn header<'a>(output: &'a SigningOutput, name: &str) -> Option<&'a str> {
    output
        .set_headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| std::str::from_utf8(v).unwrap())
}

fn authorization(signed_headers: &str, signature: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
         SignedHeaders={signed_headers}, Signature={signature}"
    )
}

/// Sign `ctx` with the suite's credentials and check the `Authorization`
/// header.
async fn assert_signature(ctx: &SigningContext, signed_headers: &str, signature: &str) {
    let output = plugin().sign(ctx).await.unwrap();
    assert_eq!(
        header(&output, "authorization"),
        Some(authorization(signed_headers, signature).as_str()),
        "{} {}",
        ctx.method,
        ctx.path
    );
    assert_eq!(header(&output, "x-amz-date"), Some("20150830T123600Z"));
}

// ---------------------------------------------------------------------------
// AWS test suite
// ---------------------------------------------------------------------------

#[tokio::test]
async fn suite_get_vanilla() {
    let ctx = request("GET", "/", None, &[], empty(), suite_config("double"));
    assert_signature(
        &ctx,
        "host;x-amz-date",
        "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31",
    )
    .await;
}

/// `get-vanilla-query-order-key-case`: the query is signed sorted and sent
/// in the client's order, already in SigV4 encoding.
#[tokio::test]
async fn suite_get_vanilla_query_order_key_case() {
    let ctx = request(
        "GET",
        "/",
        Some("Param2=value2&Param1=value1"),
        &[],
        empty(),
        suite_config("double"),
    );
    assert_signature(
        &ctx,
        "host;x-amz-date",
        "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500",
    )
    .await;
    let output = plugin().sign(&ctx).await.unwrap();
    assert_eq!(output.query, QueryChange::Keep);
}

#[tokio::test]
async fn suite_get_vanilla_query_order_value() {
    let ctx = request(
        "GET",
        "/",
        Some("Param1=value2&Param1=value1"),
        &[],
        empty(),
        suite_config("double"),
    );
    assert_signature(
        &ctx,
        "host;x-amz-date",
        "5772eed61e12b33fae39ee5e7012498b51d56abc0abb7c60486157bd471c4694",
    )
    .await;
}

/// `get-vanilla-query-unreserved`: the gateway form-encodes `~` as `%7E`;
/// the plugin signs and sends it unencoded.
#[tokio::test]
async fn suite_get_vanilla_query_unreserved() {
    const U: &str = "-._~0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let form = U.replace('~', "%7E");
    let ctx = request(
        "GET",
        "/",
        Some(&format!("{form}={form}")),
        &[],
        empty(),
        suite_config("double"),
    );
    assert_signature(
        &ctx,
        "host;x-amz-date",
        "9c3e54bfcdf0b19771a7f523ee5669cdf59bc7cc0884027167c21bb143a40197",
    )
    .await;
    let output = plugin().sign(&ctx).await.unwrap();
    assert_eq!(output.query, QueryChange::Replace(format!("{U}={U}")));
}

#[tokio::test]
async fn suite_get_vanilla_utf8_query() {
    let ctx = request(
        "GET",
        "/",
        Some("%E1%88%B4=bar"),
        &[],
        empty(),
        suite_config("double"),
    );
    assert_signature(
        &ctx,
        "host;x-amz-date",
        "2cdec8eed098649ff3a119c94853b13c643bcf08f8b0a1d91e12c9027818dd04",
    )
    .await;
}

#[tokio::test]
async fn suite_post_x_www_form_urlencoded() {
    let ctx = request(
        "POST",
        "/",
        None,
        &[("content-type", b"application/x-www-form-urlencoded")],
        SigningBody::Buffered(Bytes::from_static(b"Param1=value1")),
        suite_config("double"),
    );
    assert_signature(
        &ctx,
        "content-type;host;x-amz-date",
        "ff11897932ad3f4e8b18135d722051e5ac45fc38421b1da7b9d196a0fe09473a",
    )
    .await;
}

/// `post-vanilla` and `post-header-key-case` (the gateway's header names
/// are already lowercase).
#[tokio::test]
async fn suite_post_vanilla() {
    let ctx = request("POST", "/", None, &[], empty(), suite_config("double"));
    assert_signature(
        &ctx,
        "host;x-amz-date",
        "5da7c1a2acd57cee7505fc6676e4e544621c30862966e37dddb68e92efbe5d6b",
    )
    .await;
}

/// `post-sts-header-before`: the session token is sent and signed.
#[tokio::test]
async fn suite_post_sts_token() {
    let ctx = request("POST", "/", None, &[], empty(), suite_config("double"));
    let output = plugin_with(source(Some(TOKEN))).sign(&ctx).await.unwrap();
    assert_eq!(
        header(&output, "authorization"),
        Some(
            authorization(
                "host;x-amz-date;x-amz-security-token",
                "85d96828115b5dc0cfc3bd16ad9e210dd772bbebba041836c64533a82be05ead"
            )
            .as_str()
        )
    );
    assert_eq!(header(&output, "x-amz-security-token"), Some(TOKEN));
}

/// `get-utf8` and `get-space`: the suite's path is raw and encoded once,
/// which is the `single` encoding of the path as sent.
#[tokio::test]
async fn suite_single_encoded_paths() {
    for (path, signature) in [
        (
            "/%E1%88%B4",
            "8318018e0b0f223aa2bbf98705b62bb787dc9c0e678f255a891fd03141be5d85",
        ),
        (
            "/example%20space/",
            "652487583200325589f1fba4c7e578f72c47cb61beeca81406b39ddec1366741",
        ),
    ] {
        let ctx = request("GET", path, None, &[], empty(), suite_config("single"));
        assert_signature(&ctx, "host;x-amz-date", signature).await;
    }
}

/// The `normalize-path` cases and `get-unreserved`, with `double` encoding.
#[tokio::test]
async fn suite_normalize_path() {
    const VANILLA: &str = "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31";
    for (path, signature) in [
        ("//", VANILLA),
        ("/example/..", VANILLA),
        ("/example1/example2/../..", VANILLA),
        ("/./", VANILLA),
        (
            "/./example",
            "ef75d96142cf21edca26f06005da7988e4f8dc83a165a80865db7089db637ec5",
        ),
        (
            "//example//",
            "9a624bd73a37c9a373b5312afbebe7a714a789de108f0bdfe846570885f57e84",
        ),
        (
            "/-._~0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
            "07ef7494c76fa4850883e2b006601f940f8a34d404d0cfa977f52a65bbf5f24f",
        ),
    ] {
        let ctx = request("GET", path, None, &[], empty(), suite_config("double"));
        assert_signature(&ctx, "host;x-amz-date", signature).await;
    }
}

/// `get-header-key-duplicate`, `get-header-value-order` and
/// `get-header-value-trim`: header canonicalization, for headers the plugin
/// does not select itself.
#[test]
fn suite_header_canonicalization() {
    type Lines = &'static [(&'static str, &'static [u8])];
    let cases: [(Lines, &str, &str); 3] = [
        (
            &[
                ("my-header1", b"value2"),
                ("my-header1", b"value2"),
                ("my-header1", b"value1"),
            ],
            "host;my-header1;x-amz-date",
            "c9d5ea9f3f72853aea855b47ea873832890dbdd183b4468f858259531a5138ea",
        ),
        (
            &[
                ("my-header1", b"value4"),
                ("my-header1", b"value1"),
                ("my-header1", b"value3"),
                ("my-header1", b"value2"),
            ],
            "host;my-header1;x-amz-date",
            "08c7e5a9acfcfeb3ab6b2185e75ce8b1deb5e634ec47601a50643f830c755c01",
        ),
        (
            &[
                ("my-header1", b" value1"),
                ("my-header2", b" \"a   b   c\""),
            ],
            "host;my-header1;my-header2;x-amz-date",
            "acc3ed3afb60bb290fc8d2dd0098b9911fcaa05412b367055dee359757a9c736",
        ),
    ];
    for (lines, signed_headers, signature) in cases {
        let mut headers = HeaderLines::new();
        headers.insert("host".into(), vec![HOST.as_bytes().to_vec()]);
        headers.insert("x-amz-date".into(), vec![b"20150830T123600Z".to_vec()]);
        for (name, value) in lines {
            headers
                .entry((*name).to_owned())
                .or_default()
                .push(value.to_vec());
        }
        let canonical = canonical_request("GET", "/", "", &headers, EMPTY_SHA256);
        assert_eq!(canonical.signed_headers, signed_headers);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/service/aws4_request\n{}",
            sha256_hex(&canonical.request).unwrap()
        );
        let key = SigningKey::derive(
            SECRET.as_bytes(),
            &CredentialScope::new("20150830", "us-east-1", "service").unwrap(),
        )
        .unwrap();
        assert_eq!(
            key.signature(string_to_sign.as_bytes()).unwrap(),
            signature,
            "{lines:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Path and query encoding
// ---------------------------------------------------------------------------

#[test]
fn double_encoding_encodes_the_sent_path_again() {
    for (sent, canonical) in [
        ("/example%20space/", "/example%2520space/"),
        ("/a:b/c%2Fd", "/a%3Ab/c%252Fd"),
        ("/a//b/./c/../d", "/a/b/d"),
        (
            "/model/anthropic.claude-v2/invoke",
            "/model/anthropic.claude-v2/invoke",
        ),
        ("", "/"),
    ] {
        assert_eq!(
            canonical_path(sent, UriEncoding::Double),
            canonical,
            "{sent}"
        );
    }
}

#[test]
fn single_encoding_signs_the_path_as_sent() {
    for sent in [
        "/a//b/./c/../d",
        "/key%20with%20spaces",
        "/a%3Ab",
        "/~x-y_z.0",
    ] {
        assert_eq!(canonical_path(sent, UriEncoding::Single), sent);
    }
    assert_eq!(canonical_path("", UriEncoding::Single), "/");
}

/// With single encoding the path is decoded and encoded again in SigV4
/// encoding, as S3 checks it: characters a client sends unencoded are
/// signed encoded, lowercase hex is signed uppercase, `%2F` is `/`.
#[test]
fn single_encoding_reencodes_the_path() {
    for (sent, canonical) in [
        ("/bucket/a:b", "/bucket/a%3Ab"),
        ("/bucket/photo(1).jpg", "/bucket/photo%281%29.jpg"),
        ("/bucket/x!y*z", "/bucket/x%21y%2Az"),
        ("/bucket/k=v@w,u", "/bucket/k%3Dv%40w%2Cu"),
        ("/bucket/a+b", "/bucket/a%2Bb"),
        ("/bucket/a%3ab", "/bucket/a%3Ab"),
        ("/bucket/%7Ex", "/bucket/~x"),
        ("/bucket/a%2Fb", "/bucket/a/b"),
        ("/bucket/caf%C3%A9", "/bucket/caf%C3%A9"),
    ] {
        assert_eq!(
            canonical_path(sent, UriEncoding::Single),
            canonical,
            "{sent}"
        );
    }
}

/// The gateway's form encoding (`+` for a space, `*` and `%7E` as they
/// come) is decoded and re-encoded for SigV4: a space is `%20`, a literal
/// plus `%2B`, `*` is `%2A`, `~` plain. Repeated names and empty values
/// stay; the sent query keeps the order, the canonical one is sorted.
#[test]
fn query_is_rewritten_to_sigv4_encoding() {
    let (canonical, sent) = canonical_query(Some("q=a+b&p=1%2B2&e=&r=x&r=a&s=*&t=%7E"));
    assert_eq!(
        sent.as_deref(),
        Some("q=a%20b&p=1%2B2&e=&r=x&r=a&s=%2A&t=~")
    );
    assert_eq!(canonical, "e=&p=1%2B2&q=a%20b&r=a&r=x&s=%2A&t=~");
    assert_eq!(canonical_query(None), (String::new(), None));
}

#[tokio::test]
async fn query_already_in_sigv4_encoding_is_kept() {
    let ctx = request("GET", "/", Some("b=2&a=%20"), &[], empty(), config(&[]));
    assert_eq!(plugin().sign(&ctx).await.unwrap().query, QueryChange::Keep);
    let ctx = request("GET", "/", Some("a=x+y"), &[], empty(), config(&[]));
    assert_eq!(
        plugin().sign(&ctx).await.unwrap().query,
        QueryChange::Replace("a=x%20y".into())
    );
}

/// A query with no pairs is signed as an empty query and sent without one;
/// a request without a query keeps it absent.
#[tokio::test]
async fn query_without_pairs_is_removed() {
    for query in ["", "&", "&&"] {
        let ctx = request("GET", "/", Some(query), &[], empty(), config(&[]));
        assert_eq!(
            plugin().sign(&ctx).await.unwrap().query,
            QueryChange::Remove,
            "{query:?}"
        );
    }
    let ctx = request("GET", "/", None, &[], empty(), config(&[]));
    assert_eq!(plugin().sign(&ctx).await.unwrap().query, QueryChange::Keep);
}

/// The form-urlencoded pairs of `query` as the gateway builds it (`+` for a
/// space), fully decoded, in order.
fn form_pairs(query: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |c: &str| -> Vec<u8> {
                percent_encoding::percent_decode(c.replace('+', " ").as_bytes()).collect()
            };
            (decode(name), decode(value))
        })
        .collect()
}

/// The pairs of a query in SigV4 encoding (no `+` for a space), decoded.
fn uri_pairs(query: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    query
        .split('&')
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap();
            let decode =
                |c: &str| -> Vec<u8> { percent_encoding::percent_decode(c.as_bytes()).collect() };
            (decode(name), decode(value))
        })
        .collect()
}

proptest::proptest! {
    /// Any query: the query sent is a fixed point (a second pass keeps it),
    /// carries the input's form-decoded pairs in order, has only unreserved
    /// characters and `%XX` in its names and values, and the canonical query
    /// is its pairs sorted.
    #[test]
    fn canonical_query_is_a_fixed_point_with_the_same_pairs(
        q in r"[a-zA-Z0-9 %+=&~*._\-/:?#\u{e9}\u{1F600}]{0,40}"
    ) {
        let (canonical, sent) = canonical_query(Some(&q));
        let pairs = form_pairs(&q);
        match &sent {
            None => {
                proptest::prop_assert!(pairs.is_empty());
                proptest::prop_assert_eq!(canonical, "");
            }
            Some(sent) => {
                proptest::prop_assert_eq!(canonical_query(Some(sent)), (canonical.clone(), Some(sent.clone())));
                proptest::prop_assert_eq!(uri_pairs(sent), pairs);
                proptest::prop_assert!(
                    sent.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%=&".contains(&b)),
                    "{}", sent
                );
                // Sorted by name, then value (not as `name=value` text).
                let mut sorted: Vec<(&str, &str)> =
                    sent.split('&').map(|p| p.split_once('=').unwrap()).collect();
                sorted.sort_unstable();
                let sorted: Vec<String> = sorted.iter().map(|(n, v)| format!("{n}={v}")).collect();
                proptest::prop_assert_eq!(canonical, sorted.join("&"));
            }
        }
    }

    /// Any path, with `double`: [`double_encoded_model`] of it. Repeated
    /// slashes and `.` are dropped, `..` removes the segment before it, a
    /// trailing slash stays, and each segment is encoded once more.
    #[test]
    fn double_encoded_path_is_normalized(
        p in r"(/|//|\.|\.\.|[a-zA-Z0-9%~ \u{e9}]{1,4}){0,12}"
    ) {
        let canonical = canonical_path(&p, UriEncoding::Double);
        proptest::prop_assert_eq!(&canonical, &double_encoded_model(&p), "{:?}", p);
    }

    /// `region` and `service` follow the rule of `CredentialScope::new`: a
    /// value the config accepts can be signed, and one it refuses could not.
    #[test]
    fn config_scope_rule_is_the_crypto_rule(
        v in r"[a-zA-Z0-9\-_./: \u{e9}]{0,3}|[a-zA-Z0-9]{60,68}"
    ) {
        for key in ["region", "service"] {
            let parsed = AwsSigV4AuthConfig::parse(Some(&config(&[(key, &v)])));
            let (region, service) = if key == "region" {
                (v.as_str(), "service")
            } else {
                ("us-east-1", v.as_str())
            };
            let scope = CredentialScope::new("20150830", region, service);
            proptest::prop_assert_eq!(parsed.is_ok(), scope.is_ok(), "{}={:?}", key, v);
        }
    }
}

/// The non-empty segments of `path` other than `.`, with `..` removing the
/// one before it.
fn dot_resolved_segments(path: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments
}

/// SigV4's double encoding of a path as sent: [`dot_resolved_segments`],
/// each percent-encoded again (every byte but `A-Z a-z 0-9 - . _ ~`),
/// joined under a leading `/`, with the trailing `/` of a non-empty path.
fn double_encoded_model(path: &str) -> String {
    const SIGV4: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    let segments = dot_resolved_segments(path);
    let encoded: Vec<String> = segments
        .iter()
        .map(|s| percent_encoding::utf8_percent_encode(s, SIGV4).to_string())
        .collect();
    let mut out = format!("/{}", encoded.join("/"));
    if path.ends_with('/') && !segments.is_empty() {
        out.push('/');
    }
    out
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

/// Signed: `host`, every `x-amz-*`, `content-type`, `content-md5`. Not
/// signed: `cookie`, `user-agent`, anything else. A client's `x-amz-date`,
/// `x-amz-content-sha256` and `authorization` are replaced; other
/// `x-amz-*` headers are signed with every line.
#[tokio::test]
async fn header_selection_and_replacement() {
    let body = Bytes::from_static(br#"{"k":1}"#);
    let kept: [(&str, &[u8]); 8] = [
        ("content-type", b"application/json"),
        ("content-md5", b"md5"),
        ("x-amz-target", b"Service.Op"),
        ("x-amz-meta", b"one"),
        ("x-amz-meta", b"two"),
        ("cookie", b"c=1"),
        ("user-agent", b"ua"),
        ("accept", b"*/*"),
    ];
    let forged: [(&str, &[u8]); 3] = [
        ("x-amz-date", b"19700101T000000Z"),
        ("x-amz-content-sha256", b"forged"),
        ("authorization", b"forged"),
    ];
    let sign = |headers: Vec<(&'static str, &'static [u8])>| {
        let ctx = request(
            "POST",
            "/v1",
            None,
            &headers,
            SigningBody::Buffered(body.clone()),
            config(&[]),
        );
        async move { plugin().sign(&ctx).await.unwrap() }
    };
    let output = sign(kept.iter().chain(&forged).copied().collect()).await;
    let names: Vec<&str> = output.set_headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["authorization", "x-amz-date", "x-amz-content-sha256"]
    );
    let body_hash = sha256_hex(&body).unwrap();
    assert_eq!(
        header(&output, "x-amz-content-sha256"),
        Some(body_hash.as_str())
    );
    assert_eq!(header(&output, "x-amz-date"), Some("20150830T123600Z"));
    let auth = header(&output, "authorization").unwrap();
    assert!(
        auth.contains(
            "SignedHeaders=content-md5;content-type;host;x-amz-content-sha256;x-amz-date;\
             x-amz-meta;x-amz-target,"
        ),
        "{auth}"
    );
    // The client's values do not reach the canonical request: the signature
    // is the one of the same request without them.
    let clean = sign(kept.to_vec()).await;
    assert_eq!(header(&clean, "authorization"), Some(auth));
}

/// With `content_sha256_header: false` the plugin does not add the header,
/// but a client's one is still replaced with the real hash.
#[tokio::test]
async fn content_sha256_header_off_still_replaces_a_client_value() {
    let cfg = config(&[("content_sha256_header", "false")]);
    let ctx = request("GET", "/", None, &[], empty(), cfg.clone());
    let output = plugin().sign(&ctx).await.unwrap();
    assert_eq!(header(&output, "x-amz-content-sha256"), None);

    let ctx = request(
        "GET",
        "/",
        None,
        &[("x-amz-content-sha256", b"forged")],
        empty(),
        cfg,
    );
    let output = plugin().sign(&ctx).await.unwrap();
    assert_eq!(header(&output, "x-amz-content-sha256"), Some(EMPTY_SHA256));
}

/// A client's `x-amz-security-token` is replaced by the configured token;
/// without one the plugin cannot remove it and refuses the request before
/// reading credentials.
#[tokio::test]
async fn client_security_token() {
    let client = [("x-amz-security-token", &b"client-token"[..])];
    let with_token = config(&[("session_token_ref", "cred://aws-token")]);
    let ctx = request("GET", "/", None, &client, empty(), with_token);
    let output = plugin_with(source(Some(TOKEN))).sign(&ctx).await.unwrap();
    assert_eq!(header(&output, "x-amz-security-token"), Some(TOKEN));

    let credentials = source(None);
    let ctx = request("GET", "/", None, &client, empty(), config(&[]));
    let err = plugin_with(credentials.clone())
        .sign(&ctx)
        .await
        .unwrap_err();
    let PluginError::InvalidRequest { field, detail: msg } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(*field, "x-amz-security-token");
    assert!(msg.contains("x-amz-security-token"), "{msg}");
    assert!(msg.contains("session_token_ref"), "{msg}");
    assert!(!msg.contains("client-token"), "{msg}");
    assert_eq!(credentials.reads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn request_without_host_is_not_signed() {
    let mut ctx = request("GET", "/", None, &[], empty(), config(&[]));
    ctx.headers.clear();
    assert!(matches!(
        plugin().sign(&ctx).await,
        Err(PluginError::Internal(_))
    ));
}

// ---------------------------------------------------------------------------
// Body
// ---------------------------------------------------------------------------

/// A streamed body is refused unless `unsigned_payload` is set, before any
/// credential is read; with it the payload is `UNSIGNED-PAYLOAD`.
#[tokio::test]
async fn streamed_body_needs_unsigned_payload() {
    let credentials = source(None);
    let ctx = request("POST", "/", None, &[], SigningBody::Streamed, config(&[]));
    let err = plugin_with(credentials.clone())
        .sign(&ctx)
        .await
        .unwrap_err();
    let PluginError::InvalidRequest { field, detail: msg } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(*field, "body");
    assert!(msg.contains("auth.config.unsigned_payload"), "{msg}");
    assert!(msg.contains("UNSIGNED-PAYLOAD"), "{msg}");
    assert_eq!(credentials.reads.load(Ordering::SeqCst), 0);

    let ctx = request(
        "POST",
        "/",
        None,
        &[],
        SigningBody::Streamed,
        config(&[("unsigned_payload", "true")]),
    );
    let output = plugin().sign(&ctx).await.unwrap();
    assert_eq!(
        header(&output, "x-amz-content-sha256"),
        Some("UNSIGNED-PAYLOAD")
    );
}

/// A path the gateway normalized is refused with `single` encoding (S3,
/// where it names another key), before any credential is read; `double`
/// signs it, the service normalizes the same way.
#[tokio::test]
async fn rewritten_path_is_refused_with_single_encoding() {
    for service in ["s3", "s3express"] {
        let credentials = source(None);
        let mut ctx = request(
            "PUT",
            "/bucket/a/b",
            None,
            &[],
            empty(),
            config(&[("service", service)]),
        );
        ctx.path_rewritten = true;
        let err = plugin_with(credentials.clone())
            .sign(&ctx)
            .await
            .unwrap_err();
        let PluginError::InvalidRequest { field, detail } = &err else {
            panic!("{service}: {err:?}");
        };
        assert_eq!(*field, "path", "{service}");
        assert!(detail.contains("another object"), "{detail}");
        assert_eq!(credentials.reads.load(Ordering::SeqCst), 0, "{service}");
    }

    let mut ctx = request("GET", "/a/b", None, &[], empty(), suite_config("single"));
    ctx.path_rewritten = true;
    assert!(matches!(
        plugin().sign(&ctx).await,
        Err(PluginError::InvalidRequest { field: "path", .. })
    ));

    for config in [config(&[]), suite_config("double")] {
        let mut ctx = request("GET", "/a/b", None, &[], empty(), config);
        ctx.path_rewritten = true;
        let output = plugin().sign(&ctx).await.unwrap();
        assert!(header(&output, "authorization").is_some());
    }

    // An S3 path the gateway left as it was is signed.
    let ctx = request(
        "PUT",
        "/bucket/a/b",
        None,
        &[],
        empty(),
        config(&[("service", "s3")]),
    );
    assert!(plugin().sign(&ctx).await.is_ok());
}

/// `unsigned_payload` changes nothing for a buffered body: it is hashed.
#[tokio::test]
async fn buffered_body_is_always_hashed() {
    let ctx = request(
        "POST",
        "/",
        None,
        &[],
        SigningBody::Buffered(Bytes::from_static(b"Param1=value1")),
        config(&[("unsigned_payload", "true")]),
    );
    let output = plugin().sign(&ctx).await.unwrap();
    assert_eq!(
        header(&output, "x-amz-content-sha256"),
        Some("9095672bbd1f56dfc5b65f3e153adc8731a4a654192329106275f4c7b24d0b6e")
    );
}

/// On a multi-threaded runtime a body up to 64 KiB is hashed inline and a
/// larger one under `block_in_place`; both give the body's SHA-256.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_body_is_hashed() {
    assert_hashes(&[(INLINE_HASH_MAX_LEN, 0), (INLINE_HASH_MAX_LEN + 1, 1)]).await;
}

/// On a current-thread runtime, where `block_in_place` would panic, a large
/// body is hashed inline.
#[tokio::test]
async fn large_body_is_hashed_on_a_current_thread_runtime() {
    assert_hashes(&[(INLINE_HASH_MAX_LEN + 1, 0)]).await;
}

/// Signs a body of each length and checks its hash and how many of them
/// went through `block_in_place`.
async fn assert_hashes(cases: &[(usize, usize)]) {
    for &(len, blocking) in cases {
        let before = BLOCKING_HASHES.with(std::cell::Cell::get);
        let body = Bytes::from(vec![b'a'; len]);
        let ctx = request(
            "PUT",
            "/",
            None,
            &[],
            SigningBody::Buffered(body.clone()),
            config(&[]),
        );
        let output = plugin().sign(&ctx).await.unwrap();
        assert_eq!(
            header(&output, "x-amz-content-sha256"),
            Some(sha256_hex(&body).unwrap().as_str()),
            "{len}"
        );
        let blocked = BLOCKING_HASHES.with(std::cell::Cell::get) - before;
        assert_eq!(blocked, blocking, "{len}");
    }
}

// ---------------------------------------------------------------------------
// Config and credentials
// ---------------------------------------------------------------------------

/// `authenticate` checks the config and sets nothing; a missing region,
/// service or key reference is `InvalidConfig` naming the key.
#[tokio::test]
async fn authenticate_checks_the_config_and_sets_nothing() {
    let mut ctx = AuthContext::new(config(&[]), security_context());
    plugin().authenticate(&mut ctx).await.unwrap();
    assert!(ctx.set_headers.is_empty());

    for key in [
        "region",
        "service",
        "access_key_id_ref",
        "secret_access_key_ref",
    ] {
        let mut cfg = config(&[]);
        cfg.remove(key);
        let mut ctx = AuthContext::new(cfg.clone(), security_context());
        let err = plugin().authenticate(&mut ctx).await.unwrap_err();
        assert!(
            matches!(&err, PluginError::InvalidConfig(msg) if msg.contains(&format!("auth.config.{key}"))),
            "{key}: {err:?}"
        );
        let sign = request("GET", "/", None, &[], empty(), cfg);
        assert!(
            matches!(
                plugin().sign(&sign).await,
                Err(PluginError::InvalidConfig(_))
            ),
            "{key}"
        );
    }
}

#[test]
fn timestamps_are_utc() {
    let at = OffsetDateTime::from_unix_timestamp(VECTOR_TIME).unwrap();
    assert_eq!(
        timestamps(at),
        ("20150830".to_owned(), "20150830T123600Z".to_owned())
    );
    let offset = at.to_offset(UtcOffset::from_hms(5, 30, 0).unwrap());
    assert_eq!(timestamps(offset).1, "20150830T123600Z");
}

fn store_config() -> HashMap<String, String> {
    config(&[("session_token_ref", "cred://aws-token")])
}

fn parsed(config: &HashMap<String, String>) -> AwsSigV4AuthConfig {
    AwsSigV4AuthConfig::parse(Some(config)).unwrap()
}

#[tokio::test]
async fn credstore_source_reads_the_three_references() {
    let store = CredStoreCredentialSource::new(Arc::new(MockCredStoreClient::with_secrets(vec![
        ("aws-akid".into(), AKID.into()),
        ("aws-secret".into(), SECRET.into()),
        ("aws-token".into(), TOKEN.into()),
    ])));
    let creds = store
        .credentials(&parsed(&store_config()), &security_context())
        .await
        .unwrap();
    assert_eq!(creds.access_key_id, AKID);
    assert_eq!(creds.secret_access_key.as_slice(), SECRET.as_bytes());
    assert_eq!(
        creds.session_token.as_deref().map(String::as_str),
        Some(TOKEN)
    );
}

/// The three references are read concurrently: with every read taking a
/// second, the credentials come in one second, not three.
#[tokio::test(start_paused = true)]
async fn credstore_source_reads_concurrently() {
    use crate::infra::proxy::credstore_spy::{CredStoreSpy, SpySecret};
    const READ: std::time::Duration = std::time::Duration::from_secs(1);
    let spy = CredStoreSpy::with_entries(vec![
        SpySecret::new("aws-akid", AKID),
        SpySecret::new("aws-secret", SECRET),
        SpySecret::new("aws-token", TOKEN),
    ])
    .answering_after(READ);
    let store = CredStoreCredentialSource::new(Arc::new(spy));
    let start = tokio::time::Instant::now();
    let creds = store
        .credentials(&parsed(&store_config()), &security_context())
        .await
        .unwrap();
    assert_eq!(start.elapsed(), READ);
    assert!(creds.session_token.is_some());
}

/// Unusable values and store failures name the config key, never the
/// reference or the value.
#[tokio::test]
async fn credstore_source_errors_name_the_key_only() {
    let cases: [(&str, &str, &str); 4] = [
        ("aws-akid", "AKID/EXAMPLE", "access_key_id_ref"),
        ("aws-akid", "", "access_key_id_ref"),
        ("aws-secret", "", "secret_access_key_ref"),
        ("aws-token", "tok en", "session_token_ref"),
    ];
    for (reference, value, key) in cases {
        let mut secrets = vec![
            ("aws-akid".to_owned(), AKID.to_owned()),
            ("aws-secret".to_owned(), SECRET.to_owned()),
            ("aws-token".to_owned(), TOKEN.to_owned()),
        ];
        for (r, v) in &mut secrets {
            if r == reference {
                *v = value.to_owned();
            }
        }
        let store =
            CredStoreCredentialSource::new(Arc::new(MockCredStoreClient::with_secrets(secrets)));
        let err = store
            .credentials(&parsed(&store_config()), &security_context())
            .await
            .unwrap_err();
        let PluginError::Internal(msg) = &err else {
            panic!("{key}: {err:?}");
        };
        assert!(msg.contains(key), "{msg}");
        assert!(!msg.contains(reference), "{msg}");
        if !value.is_empty() {
            assert!(!msg.contains(value), "{msg}");
        }
    }

    // The access key id length limit.
    for (len, accepted) in [
        (MAX_ACCESS_KEY_ID_LEN, true),
        (MAX_ACCESS_KEY_ID_LEN + 1, false),
    ] {
        let akid = "A".repeat(len);
        let store =
            CredStoreCredentialSource::new(Arc::new(MockCredStoreClient::with_secrets(vec![
                ("aws-akid".into(), akid.clone()),
                ("aws-secret".into(), SECRET.into()),
                ("aws-token".into(), TOKEN.into()),
            ])));
        let result = store
            .credentials(&parsed(&store_config()), &security_context())
            .await;
        if accepted {
            assert_eq!(result.unwrap().access_key_id, akid);
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(&err, PluginError::Internal(msg) if msg.contains("access_key_id_ref") && !msg.contains(&akid)),
                "{err:?}"
            );
        }
    }

    let missing = CredStoreCredentialSource::new(Arc::new(MockCredStoreClient::empty()));
    assert!(matches!(
        missing
            .credentials(&parsed(&store_config()), &security_context())
            .await,
        Err(PluginError::SecretNotFound {
            key: "access_key_id_ref",
            kind: "secret not found"
        })
    ));
}

/// Credential store errors follow the one mapping of every auth plugin: a
/// missing or refused secret, or an invalid reference, is not found with
/// the store's error kind, an outage is unavailable with the store's hint,
/// an error only a write causes is internal. Each names the config key,
/// never the reference or the store's text.
#[tokio::test]
async fn credstore_source_errors_follow_the_credential_mapping() {
    use crate::domain::test_support::ErroringCredStore;
    use credstore_sdk::CredStoreError;
    use std::time::Duration;
    type MakeError = fn() -> CredStoreError;

    let cases: [(MakeError, &str); 7] = [
        (|| CredStoreError::NotFound, "secret not found"),
        (|| CredStoreError::AccessDenied, "access denied"),
        (
            || CredStoreError::InvalidSecretRef {
                reason: "bad aws-akid".into(),
            },
            "invalid secret reference",
        ),
        (
            || CredStoreError::Internal("store failed reading aws-akid".into()),
            "unavailable, no hint",
        ),
        (
            || CredStoreError::ServiceUnavailable {
                detail: "aws-akid: backend down".into(),
                retry_after: Some(Duration::from_secs(7)),
            },
            "unavailable, 7s",
        ),
        (|| CredStoreError::NoPluginAvailable, "unavailable, no hint"),
        (|| CredStoreError::Conflict, "internal"),
    ];
    for (error, expected) in cases {
        let source = CredStoreCredentialSource::new(Arc::new(ErroringCredStore(error)));
        let err = source
            .credentials(&parsed(&store_config()), &security_context())
            .await
            .unwrap_err();
        // The access key id's read fails first (`biased`).
        let got = match &err {
            PluginError::SecretNotFound {
                key: "access_key_id_ref",
                kind,
            } => kind,
            PluginError::CredentialStoreUnavailable {
                retry_after: None, ..
            } => "unavailable, no hint",
            PluginError::CredentialStoreUnavailable {
                retry_after: Some(d),
                ..
            } if *d == Duration::from_secs(7) => "unavailable, 7s",
            PluginError::Internal(_) => "internal",
            other => panic!("{expected}: unexpected {other:?}"),
        };
        assert_eq!(got, expected, "{err:?}");
        let text = err.to_string();
        assert!(text.contains("auth.config.access_key_id_ref"), "{text}");
        assert!(!text.contains("aws-akid"), "{text}");
    }
}

/// The secret key, the derived signing key and the session token appear in
/// no `Debug` output, no error and no log line, on success or failure.
#[tokio::test]
#[tracing_test::traced_test]
async fn keys_are_absent_from_debug_errors_and_logs() {
    let secrets = [SECRET, SIGNING_KEY_HEX, TOKEN];
    // A derived `Debug` on a byte buffer (`Zeroizing<Vec<u8>>`, a header
    // value) prints the bytes as a list.
    let byte_forms = [
        format!("{:?}", SECRET.as_bytes()),
        format!("{:?}", TOKEN.as_bytes()),
    ];
    let assert_clean = |text: &str| {
        for secret in secrets {
            assert!(!text.contains(secret), "{text}");
        }
        for bytes in &byte_forms {
            assert!(!text.contains(bytes.as_str()), "{text}");
        }
    };

    let ctx = request("POST", "/", None, &[], empty(), suite_config("double"));
    let credentials = source(Some(TOKEN));
    let output = plugin_with(credentials.clone()).sign(&ctx).await.unwrap();
    assert_clean(&format!("{output:?}"));
    let creds = credentials
        .credentials(&parsed(&ctx.config), &security_context())
        .await
        .unwrap();
    assert_clean(&format!("{creds:?}"));
    assert_clean(&format!("{:?}", parsed(&store_config())));

    // A secret too short for any key is refused by the facade: the error
    // carries no key either.
    let short = Arc::new(FixedCredentials {
        secret: "",
        session_token: Some(TOKEN),
        reads: AtomicUsize::new(0),
    });
    let err = plugin_with(short).sign(&ctx).await.unwrap_err();
    assert_clean(&format!("{err:?} {err}"));

    for streamed in [
        request("POST", "/", None, &[], SigningBody::Streamed, config(&[])),
        request(
            "GET",
            "/",
            None,
            &[("x-amz-security-token", TOKEN.as_bytes())],
            empty(),
            config(&[]),
        ),
    ] {
        let err = plugin_with(source(Some(TOKEN)))
            .sign(&streamed)
            .await
            .unwrap_err();
        assert_clean(&format!("{err:?} {err}"));
    }

    for secret in secrets {
        assert!(!logs_contain(secret));
    }
    for bytes in &byte_forms {
        assert!(!logs_contain(bytes));
    }
}
