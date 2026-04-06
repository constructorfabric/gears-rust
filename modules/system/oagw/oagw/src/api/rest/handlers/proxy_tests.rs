#[test]
fn parse_alias_and_suffix() {
    let path = "/oagw/v1/proxy/api.openai.com/v1/chat/completions";
    let prefix = "/oagw/v1/proxy/";
    let remaining = path.strip_prefix(prefix).unwrap();
    let (alias, suffix) = match remaining.find('/') {
        Some(pos) => (&remaining[..pos], &remaining[pos..]),
        None => (remaining, ""),
    };
    assert_eq!(alias, "api.openai.com");
    assert_eq!(suffix, "/v1/chat/completions");
}

#[test]
fn parse_alias_with_port() {
    let path = "/oagw/v1/proxy/host:8443/path";
    let prefix = "/oagw/v1/proxy/";
    let remaining = path.strip_prefix(prefix).unwrap();
    let (alias, suffix) = match remaining.find('/') {
        Some(pos) => (&remaining[..pos], &remaining[pos..]),
        None => (remaining, ""),
    };
    assert_eq!(alias, "host:8443");
    assert_eq!(suffix, "/path");
}

#[test]
fn parse_alias_no_suffix() {
    let path = "/oagw/v1/proxy/api.openai.com";
    let prefix = "/oagw/v1/proxy/";
    let remaining = path.strip_prefix(prefix).unwrap();
    let (alias, suffix) = match remaining.find('/') {
        Some(pos) => (&remaining[..pos], &remaining[pos..]),
        None => (remaining, ""),
    };
    assert_eq!(alias, "api.openai.com");
    assert_eq!(suffix, "");
}

#[test]
fn parse_query_params() {
    let query = "version=2&model=gpt-4";
    let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0], ("version".into(), "2".into()));
    assert_eq!(params[1], ("model".into(), "gpt-4".into()));
}

#[test]
fn percent_encoded_param_name_decoded() {
    let query = "my%20key=value";
    let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0], ("my key".into(), "value".into()));
}
