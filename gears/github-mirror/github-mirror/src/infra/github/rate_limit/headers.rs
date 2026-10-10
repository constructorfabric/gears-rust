//! Parsed `X-RateLimit-*` and `Retry-After` header values from a GitHub response.

use chrono::{DateTime, TimeZone as _, Utc};

/// Parsed rate-limit headers from a single GitHub REST response.
///
/// All fields are optional: GitHub may omit some headers in certain responses.
#[derive(Debug, Clone, Default)]
pub struct RateLimitHeaders {
    /// `X-RateLimit-Limit`: the token's total hourly quota.
    pub limit: Option<u32>,
    /// `X-RateLimit-Remaining`: requests still available in the current window.
    pub remaining: Option<u32>,
    /// `X-RateLimit-Used`: requests consumed in the current window.
    pub used: Option<u32>,
    /// `X-RateLimit-Reset`: Unix timestamp when the window resets.
    pub reset_at: Option<DateTime<Utc>>,
    /// `X-RateLimit-Resource`: resource category (e.g. `"core"`, `"search"`).
    pub resource: Option<String>,
    /// `Retry-After` seconds, present on 429 and some 403 responses.
    pub retry_after_secs: Option<u64>,
    /// Whether the response was a `429 Too Many Requests`.
    pub is_secondary_rate_limit: bool,
}

impl RateLimitHeaders {
    /// Parse from a sequence of lowercase header name and value pairs.
    ///
    /// Unrecognised or malformed headers are silently ignored.
    pub fn parse<'a>(headers: impl Iterator<Item = (&'a str, &'a str)>) -> Self {
        let mut out = Self::default();
        for (name, value) in headers {
            match name {
                "x-ratelimit-limit" => out.limit = value.parse().ok(),
                "x-ratelimit-remaining" => out.remaining = value.parse().ok(),
                "x-ratelimit-used" => out.used = value.parse().ok(),
                "x-ratelimit-reset" => {
                    if let Ok(secs) = value.parse::<i64>() {
                        out.reset_at = Utc.timestamp_opt(secs, 0).single();
                    }
                }
                "x-ratelimit-resource" => out.resource = Some(value.to_owned()),
                "retry-after" => out.retry_after_secs = value.parse().ok(),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test helpers panic on unexpected errors by design"
)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_headers() {
        let hdrs = [
            ("x-ratelimit-limit", "5000"),
            ("x-ratelimit-remaining", "4800"),
            ("x-ratelimit-used", "200"),
            ("x-ratelimit-reset", "1700000000"),
            ("x-ratelimit-resource", "core"),
        ];
        let r = RateLimitHeaders::parse(hdrs.iter().map(|(k, v)| (*k, *v)));
        assert_eq!(r.limit, Some(5000));
        assert_eq!(r.remaining, Some(4800));
        assert_eq!(r.used, Some(200));
        assert_eq!(r.resource.as_deref(), Some("core"));
        assert!(r.reset_at.is_some());
    }

    #[test]
    fn parse_retry_after() {
        let hdrs = [("retry-after", "60")];
        let r = RateLimitHeaders::parse(hdrs.iter().map(|(k, v)| (*k, *v)));
        assert_eq!(r.retry_after_secs, Some(60));
    }

    #[test]
    fn parse_ignores_unknown_headers() {
        let hdrs = [("x-github-request-id", "abc123")];
        let r = RateLimitHeaders::parse(hdrs.iter().map(|(k, v)| (*k, *v)));
        assert_eq!(r.remaining, None);
    }
}
