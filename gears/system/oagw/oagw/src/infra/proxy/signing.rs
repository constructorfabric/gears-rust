use std::collections::HashMap;

use http::uri::PathAndQuery;
use http::{HeaderMap, HeaderName, HeaderValue, Uri};
use toolkit_http_proxy::sanitize::is_dropped_request_header;
use toolkit_security::SecurityContext;

use crate::domain::gateway_headers::is_gateway_owned_header;
use crate::domain::plugin::{
    AuthPlugin, PluginError, QueryChange, SigningBody, SigningContext, SigningOutput,
};

/// The outbound request apart from its headers, as the signing phase gets
/// it.
pub(crate) struct SigningInput<'a> {
    /// The outbound HTTP method.
    pub(crate) method: &'a http::Method,
    /// The outbound URL as built.
    pub(crate) uri: Uri,
    /// See [`SigningContext::path_rewritten`].
    pub(crate) path_rewritten: bool,
    /// Endpoint port, also when the URL leaves out the scheme's default.
    pub(crate) port: u16,
    /// The request body as the signer sees it.
    pub(crate) body: SigningBody,
    /// The upstream's `auth.config`.
    pub(crate) config: HashMap<String, String>,
    /// The caller's security context.
    pub(crate) security_context: SecurityContext,
}

/// Run the auth plugin's signing phase (ADR-0023) on the outbound request
/// and apply its output to `headers`. Returns the new URL when the query
/// changed. `headers` must already be sanitized by the engine's rules, and
/// signing must be the last change to them and the URL.
pub(crate) async fn sign_request(
    plugin: &dyn AuthPlugin,
    input: SigningInput<'_>,
    headers: &mut HeaderMap,
) -> Result<Option<String>, PluginError> {
    let uri = input.uri.clone();
    let ctx = signing_context(input, headers);
    let output = plugin.sign(&ctx).await?;
    apply_signing_output(output, uri, headers)
}

/// The signing view of the outbound request.
fn signing_context(input: SigningInput<'_>, headers: &HeaderMap) -> SigningContext {
    let SigningInput {
        method,
        uri,
        path_rewritten,
        port,
        body,
        config,
        security_context,
    } = input;
    SigningContext {
        method: method.as_str().to_owned(),
        scheme: uri.scheme_str().unwrap_or_default().to_owned(),
        host: uri.host().unwrap_or_default().to_owned(),
        port,
        path: uri.path().to_owned(),
        path_rewritten,
        query: uri.query().map(str::to_owned),
        // `HeaderMap` iterates every line of a name in insertion order, the
        // order the engine writes them in.
        headers: headers
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
            .collect(),
        body,
        config,
        security_context,
    }
}

/// Apply what the signing phase returned; the new URL when the query
/// changed. Errors name the header, never the value: it carries the
/// signature or a credential.
fn apply_signing_output(
    output: SigningOutput,
    uri: Uri,
    headers: &mut HeaderMap,
) -> Result<Option<String>, PluginError> {
    let mut set = Vec::with_capacity(output.set_headers.len());
    for (name, value) in output.set_headers {
        let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            PluginError::InvalidConfig(format!("signing header name {name:?} is invalid"))
        })?;
        // The gateway or the engine would overwrite or drop it after the
        // signature was computed over it.
        if is_gateway_owned_header(header.as_str()) || is_dropped_request_header(&header) {
            return Err(PluginError::InvalidConfig(format!(
                "signing may not set header {header}"
            )));
        }
        // A later value would replace an earlier one that may be signed.
        if set.contains(&header) {
            return Err(PluginError::InvalidConfig(format!(
                "signing sets header {header} twice"
            )));
        }
        set.push(header.clone());
        let mut value = HeaderValue::from_bytes(&value).map_err(|_| {
            PluginError::InvalidConfig(format!("signing value for header {header} is invalid"))
        })?;
        value.set_sensitive(true);
        headers.insert(header, value);
    }

    match output.query {
        QueryChange::Keep => Ok(None),
        QueryChange::Remove => with_query(uri, None).map(Some),
        QueryChange::Replace(query) => with_query(uri, Some(&query)).map(Some),
    }
}

/// `uri` with its query replaced by `query`, byte for byte, or without one.
/// A query the URI parser would alter (a `#` cut as a fragment) or reject
/// is an error.
fn with_query(uri: Uri, query: Option<&str>) -> Result<String, PluginError> {
    let invalid = || PluginError::InvalidConfig("signing returned an invalid query".into());
    let path_and_query = match query {
        Some(query) => format!("{}?{query}", uri.path()),
        None => uri.path().to_owned(),
    };
    let path_and_query = PathAndQuery::try_from(path_and_query).map_err(|_| invalid())?;
    if path_and_query.query() != query {
        return Err(invalid());
    }
    let mut parts = uri.into_parts();
    parts.path_and_query = Some(path_and_query);
    Uri::from_parts(parts)
        .map(|uri| uri.to_string())
        .map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use proptest::prelude::*;

    use super::*;

    fn ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_tenant_id(uuid::Uuid::new_v4())
            .subject_id(uuid::Uuid::new_v4())
            .build()
            .unwrap()
    }

    /// Returns `output` from the signing phase.
    struct Fixed(SigningOutput);

    #[async_trait::async_trait]
    impl AuthPlugin for Fixed {
        async fn authenticate(
            &self,
            _: &mut crate::domain::plugin::AuthContext,
        ) -> Result<(), PluginError> {
            Ok(())
        }

        fn signs(&self) -> bool {
            true
        }

        async fn sign(&self, _: &SigningContext) -> Result<SigningOutput, PluginError> {
            Ok(self.0.clone())
        }
    }

    /// Implements only `authenticate`.
    struct AuthOnly;

    #[async_trait::async_trait]
    impl AuthPlugin for AuthOnly {
        async fn authenticate(
            &self,
            _: &mut crate::domain::plugin::AuthContext,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }

    const URL: &str = "https://api.example.com:8443/v1/a%2Fb?x=1&y=a+b";

    fn outbound_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("api.example.com:8443"));
        headers.append("x-multi", HeaderValue::from_static("b"));
        headers.append("x-multi", HeaderValue::from_bytes(b"a\xff").unwrap());
        headers
    }

    fn input<'a>(
        method: &'a http::Method,
        url: &str,
        body: SigningBody,
        config: HashMap<String, String>,
        security_context: SecurityContext,
    ) -> SigningInput<'a> {
        SigningInput {
            method,
            uri: url.parse().unwrap(),
            path_rewritten: false,
            port: 8443,
            body,
            config,
            security_context,
        }
    }

    async fn sign(
        plugin: &dyn AuthPlugin,
        headers: &mut HeaderMap,
    ) -> Result<Option<String>, PluginError> {
        sign_request(
            plugin,
            input(
                &http::Method::POST,
                URL,
                SigningBody::Streamed,
                HashMap::new(),
                ctx(),
            ),
            headers,
        )
        .await
    }

    fn output(set_headers: Vec<(String, Vec<u8>)>, query: QueryChange) -> SigningOutput {
        SigningOutput { set_headers, query }
    }

    #[test]
    fn context_carries_the_url_parts_every_header_line_and_the_inputs() {
        let headers = outbound_headers();
        let body = SigningBody::Buffered(Bytes::from_static(b"{}"));
        let config = HashMap::from([("key_id".to_owned(), "k1".to_owned())]);
        let caller = ctx();
        let c = signing_context(
            input(
                &http::Method::PUT,
                URL,
                body.clone(),
                config.clone(),
                caller.clone(),
            ),
            &headers,
        );
        assert_eq!(c.method, "PUT");
        assert_eq!(c.scheme, "https");
        assert_eq!(c.host, "api.example.com");
        assert_eq!(c.port, 8443);
        assert_eq!(c.path, "/v1/a%2Fb");
        assert_eq!(c.query.as_deref(), Some("x=1&y=a+b"));
        assert_eq!(
            c.headers,
            [
                ("host".to_owned(), b"api.example.com:8443".to_vec()),
                ("x-multi".to_owned(), b"b".to_vec()),
                ("x-multi".to_owned(), b"a\xff".to_vec()),
            ]
        );
        assert_eq!(c.body, body);
        assert_eq!(c.config, config);
        assert_eq!(c.security_context.subject_id(), caller.subject_id());

        let c = signing_context(
            input(
                &http::Method::GET,
                "https://h/p",
                SigningBody::Streamed,
                HashMap::new(),
                ctx(),
            ),
            &HeaderMap::new(),
        );
        assert_eq!(c.query, None);
    }

    /// A plugin without a signing phase says so and returns no change.
    #[tokio::test]
    async fn default_phase_changes_nothing() {
        assert!(!AuthOnly.signs());
        let c = signing_context(
            input(
                &http::Method::GET,
                URL,
                SigningBody::Streamed,
                HashMap::new(),
                ctx(),
            ),
            &HeaderMap::new(),
        );
        assert_eq!(AuthOnly.sign(&c).await.unwrap(), SigningOutput::default());
        let mut headers = outbound_headers();
        assert_eq!(sign(&AuthOnly, &mut headers).await.unwrap(), None);
        assert_eq!(headers, outbound_headers());
    }

    #[tokio::test]
    async fn set_headers_replace_their_name_and_are_sensitive() {
        let mut headers = outbound_headers();
        let plugin = Fixed(output(
            vec![
                ("X-Multi".into(), b"signed".to_vec()),
                ("authorization".into(), b"AWS4 sig".to_vec()),
            ],
            QueryChange::Keep,
        ));
        assert_eq!(sign(&plugin, &mut headers).await.unwrap(), None);
        let multi: Vec<_> = headers.get_all("x-multi").iter().collect();
        assert_eq!(multi, [&HeaderValue::from_static("signed")]);
        assert!(multi[0].is_sensitive());
        assert!(headers["authorization"].is_sensitive());
    }

    #[tokio::test]
    async fn query_change_is_applied_verbatim() {
        for query in ["y=a%20b&x=1", "a=%2B&a=&b=%7E", "k", ""] {
            let mut headers = outbound_headers();
            let plugin = Fixed(output(vec![], QueryChange::Replace(query.into())));
            assert_eq!(
                sign(&plugin, &mut headers).await.unwrap().as_deref(),
                Some(format!("https://api.example.com:8443/v1/a%2Fb?{query}").as_str())
            );
        }
        let mut headers = outbound_headers();
        let plugin = Fixed(output(vec![], QueryChange::Remove));
        assert_eq!(
            sign(&plugin, &mut headers).await.unwrap().as_deref(),
            Some("https://api.example.com:8443/v1/a%2Fb")
        );
    }

    #[tokio::test]
    async fn invalid_output_fails_without_echoing_the_value() {
        let header = |name: &str, value: &[u8]| {
            output(vec![(name.into(), value.to_vec())], QueryChange::Keep)
        };
        let cases = [
            header("bad name", b"secret-bad-name"),
            header("x-sig", b"secret\nsig"),
            header("Host", b"secret-host"),
            header("content-length", b"7007"),
            header("connection", b"secret-connection"),
            header("x-forwarded-for", b"secret-forwarded"),
            header("x-oagw-target-host", b"secret-target"),
            // Dropped by the engine, not by the gateway's own lists.
            header("expect", b"secret-expect"),
            header("proxy-connection", b"secret-proxy-connection"),
            header("http2-settings", b"secret-h2-settings"),
            output(
                vec![
                    ("x-sig".into(), b"secret-first".to_vec()),
                    ("X-Sig".into(), b"secret-second".to_vec()),
                ],
                QueryChange::Keep,
            ),
            output(vec![], QueryChange::Replace("secret=1#frag".into())),
            output(vec![], QueryChange::Replace("secret=1 b".into())),
        ];
        for case in cases {
            let values: Vec<String> = case
                .set_headers
                .iter()
                .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
                .chain(match &case.query {
                    QueryChange::Replace(q) => Some(q.clone()),
                    QueryChange::Keep | QueryChange::Remove => None,
                })
                .collect();
            let mut headers = outbound_headers();
            let err = sign(&Fixed(case), &mut headers).await.unwrap_err();
            assert!(matches!(err, PluginError::InvalidConfig(_)), "{err:?}");
            let message = err.to_string();
            for value in values {
                assert!(!message.contains(&value), "{message}");
            }
        }
    }

    /// `Debug` shows the header names and the kind of query change, never a
    /// value.
    #[test]
    fn debug_hides_values() {
        let out = output(
            vec![("authorization".into(), b"secret-sig".to_vec())],
            QueryChange::Replace("X-Amz-Signature=secret-q".into()),
        );
        let debug = format!("{out:?}");
        assert!(debug.contains("authorization"), "{debug}");
        assert!(debug.contains("Replace"), "{debug}");
        assert!(!debug.contains("secret"), "{debug}");
    }

    proptest! {
        /// Any replacement query is refused or sent byte for byte on the
        /// same scheme, authority and path; `Remove` leaves no `?`.
        #[test]
        fn with_query_keeps_the_path_and_the_bytes(
            query in prop_oneof!["[ -~]{0,40}", "\\PC{0,40}", ".{0,40}"],
        ) {
            let uri: Uri = URL.parse().unwrap();
            if let Ok(url) = with_query(uri.clone(), Some(&query)) {
                let sent: Uri = url.parse().unwrap();
                prop_assert_eq!(sent.scheme(), uri.scheme());
                prop_assert_eq!(sent.authority(), uri.authority());
                prop_assert_eq!(sent.path(), uri.path());
                prop_assert_eq!(sent.query(), Some(query.as_str()));
            }
            let url = with_query(uri.clone(), None).unwrap();
            prop_assert!(!url.contains('?'), "{}", url);
            let sent: Uri = url.parse().unwrap();
            prop_assert_eq!(sent.path(), uri.path());
        }
    }
}
