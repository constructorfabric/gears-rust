//! Storage-local JSON shapes for the JSON columns of the `oagw_*` tables.
//!
//! These are not the REST DTOs: the stored layout changes only together with
//! the rows' `schema_version`, independently of the API. Names are snake_case,
//! except CORS methods, which are upper-case like every stored HTTP method.
//! Unknown fields are ignored, so a later additive field stays readable.
//!
//! Sharing modes are not part of these shapes where the table has a sharing
//! column (the column is the source of truth). Route CORS has no column, so
//! [`StoredCors::sharing`] carries it. `RateLimitConfig::pool_owner_id` is
//! computed at merge time and never stored.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::domain::model::{
    BudgetConfig, BudgetMode, BurstConfig, CorsConfig, CorsHttpMethod, Endpoint, HeadersConfig,
    HttpMatch, PassthroughMode, PathSuffixMode, RateLimitAlgorithm, RateLimitConfig,
    RateLimitScope, RateLimitStrategy, RequestHeaderRules, ResponseHeaderRules, Scheme, Server,
    SharingMode, SustainedRate, Window,
};

/// A stored enum mirroring a domain enum variant by variant. The conversions
/// are exhaustive matches, so a new domain variant fails to compile here
/// until it has a stored name.
macro_rules! stored_enum {
    ($(#[$attr:meta])* $stored:ident <=> $domain:ident { $($variant:ident),+ $(,)? }) => {
        #[allow(unknown_lints, de0309_must_have_domain_model)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        $(#[$attr])*
        pub(super) enum $stored {
            $($variant),+
        }

        impl From<$domain> for $stored {
            fn from(value: $domain) -> Self {
                match value {
                    $($domain::$variant => Self::$variant),+
                }
            }
        }

        impl From<$stored> for $domain {
            fn from(value: $stored) -> Self {
                match value {
                    $($stored::$variant => Self::$variant),+
                }
            }
        }
    };
}

stored_enum!(
    #[serde(rename_all = "snake_case")]
    StoredSharingMode <=> SharingMode { Private, Inherit, Enforce }
);
stored_enum!(
    #[serde(rename_all = "snake_case")]
    StoredScheme <=> Scheme { Http, Https, Wss, Wt, Grpc }
);
stored_enum!(
    #[serde(rename_all = "snake_case")]
    StoredPassthroughMode <=> PassthroughMode { None, Allowlist, All }
);
stored_enum!(
    #[serde(rename_all = "snake_case")]
    StoredRateLimitAlgorithm <=> RateLimitAlgorithm { TokenBucket, SlidingWindow }
);
stored_enum!(
    #[serde(rename_all = "snake_case")]
    StoredWindow <=> Window { Second, Minute, Hour, Day }
);
stored_enum!(
    #[serde(rename_all = "snake_case")]
    StoredBudgetMode <=> BudgetMode { Unlimited, Allocated, Shared }
);
stored_enum!(
    #[serde(rename_all = "snake_case")]
    StoredRateLimitScope <=> RateLimitScope { Global, Tenant, User, Ip, Route }
);
stored_enum!(
    #[serde(rename_all = "snake_case")]
    StoredRateLimitStrategy <=> RateLimitStrategy { Reject, Queue, Degrade }
);
stored_enum!(
    #[serde(rename_all = "UPPERCASE")]
    StoredCorsMethod <=> CorsHttpMethod { Get, Post, Put, Delete, Patch, Head, Options }
);
stored_enum!(
    #[serde(rename_all = "snake_case")]
    StoredPathSuffixMode <=> PathSuffixMode { Disabled, Append }
);

// ---------------------------------------------------------------------------
// oagw_upstream.server
// ---------------------------------------------------------------------------

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredServer {
    pub(super) endpoints: Vec<StoredEndpoint>,
}

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredEndpoint {
    pub(super) scheme: StoredScheme,
    pub(super) host: String,
    pub(super) port: u16,
}

impl From<&Server> for StoredServer {
    fn from(server: &Server) -> Self {
        Self {
            endpoints: server
                .endpoints
                .iter()
                .map(|e| StoredEndpoint {
                    scheme: e.scheme.into(),
                    host: e.host.clone(),
                    port: e.port,
                })
                .collect(),
        }
    }
}

impl From<StoredServer> for Server {
    fn from(stored: StoredServer) -> Self {
        Self {
            endpoints: stored
                .endpoints
                .into_iter()
                .map(|e| Endpoint {
                    scheme: e.scheme.into(),
                    host: e.host,
                    port: e.port,
                })
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// oagw_upstream.headers
// ---------------------------------------------------------------------------

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredHeaders {
    pub(super) request: Option<StoredRequestHeaders>,
    pub(super) response: Option<StoredResponseHeaders>,
}

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredRequestHeaders {
    pub(super) set: HashMap<String, String>,
    pub(super) add: HashMap<String, String>,
    pub(super) remove: Vec<String>,
    pub(super) passthrough: StoredPassthroughMode,
    pub(super) passthrough_allowlist: Vec<String>,
}

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredResponseHeaders {
    pub(super) set: HashMap<String, String>,
    pub(super) add: HashMap<String, String>,
    pub(super) remove: Vec<String>,
}

impl From<&HeadersConfig> for StoredHeaders {
    fn from(headers: &HeadersConfig) -> Self {
        Self {
            request: headers.request.as_ref().map(|r| StoredRequestHeaders {
                set: r.set.clone(),
                add: r.add.clone(),
                remove: r.remove.clone(),
                passthrough: r.passthrough.into(),
                passthrough_allowlist: r.passthrough_allowlist.clone(),
            }),
            response: headers.response.as_ref().map(|r| StoredResponseHeaders {
                set: r.set.clone(),
                add: r.add.clone(),
                remove: r.remove.clone(),
            }),
        }
    }
}

impl From<StoredHeaders> for HeadersConfig {
    fn from(stored: StoredHeaders) -> Self {
        Self {
            request: stored.request.map(|r| RequestHeaderRules {
                set: r.set,
                add: r.add,
                remove: r.remove,
                passthrough: r.passthrough.into(),
                passthrough_allowlist: r.passthrough_allowlist,
            }),
            response: stored.response.map(|r| ResponseHeaderRules {
                set: r.set,
                add: r.add,
                remove: r.remove,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// oagw_upstream.cors / oagw_route.cors
// ---------------------------------------------------------------------------

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredCors {
    /// Route CORS only: `oagw_route` has no CORS sharing column. Omitted for
    /// upstreams, where `cors_sharing` holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sharing: Option<StoredSharingMode>,
    pub(super) enabled: bool,
    pub(super) allowed_origins: Vec<String>,
    pub(super) allowed_methods: Vec<StoredCorsMethod>,
    pub(super) expose_headers: Vec<String>,
    pub(super) allow_credentials: bool,
}

impl StoredCors {
    /// The stored shape of `cors`, with its sharing mode only when `with_sharing`.
    pub(super) fn new(cors: &CorsConfig, with_sharing: bool) -> Self {
        Self {
            sharing: with_sharing.then(|| cors.sharing.into()),
            enabled: cors.enabled,
            allowed_origins: cors.allowed_origins.clone(),
            allowed_methods: cors.allowed_methods.iter().map(|m| (*m).into()).collect(),
            expose_headers: cors.expose_headers.clone(),
            allow_credentials: cors.allow_credentials,
        }
    }

    pub(super) fn into_domain(self, sharing: SharingMode) -> CorsConfig {
        CorsConfig {
            sharing,
            enabled: self.enabled,
            allowed_origins: self.allowed_origins,
            allowed_methods: self.allowed_methods.into_iter().map(Into::into).collect(),
            expose_headers: self.expose_headers,
            allow_credentials: self.allow_credentials,
        }
    }
}

// ---------------------------------------------------------------------------
// oagw_upstream.rate_limit / oagw_route.rate_limit
// ---------------------------------------------------------------------------

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredRateLimit {
    pub(super) algorithm: StoredRateLimitAlgorithm,
    pub(super) sustained: StoredSustainedRate,
    pub(super) burst: Option<StoredBurst>,
    pub(super) budget: Option<StoredBudget>,
    pub(super) scope: StoredRateLimitScope,
    pub(super) strategy: StoredRateLimitStrategy,
    pub(super) cost: u32,
    pub(super) response_headers: bool,
}

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredSustainedRate {
    pub(super) rate: u32,
    pub(super) window: StoredWindow,
}

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredBurst {
    pub(super) capacity: u32,
}

#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredBudget {
    pub(super) mode: StoredBudgetMode,
    pub(super) total: Option<u32>,
    pub(super) overcommit_ratio: Option<f64>,
}

impl From<&RateLimitConfig> for StoredRateLimit {
    fn from(rl: &RateLimitConfig) -> Self {
        Self {
            algorithm: rl.algorithm.into(),
            sustained: StoredSustainedRate {
                rate: rl.sustained.rate,
                window: rl.sustained.window.into(),
            },
            burst: rl.burst.as_ref().map(|b| StoredBurst {
                capacity: b.capacity,
            }),
            budget: rl.budget.as_ref().map(|b| StoredBudget {
                mode: b.mode.into(),
                total: b.total,
                overcommit_ratio: b.overcommit_ratio,
            }),
            scope: rl.scope.into(),
            strategy: rl.strategy.into(),
            cost: rl.cost,
            response_headers: rl.response_headers,
        }
    }
}

impl StoredRateLimit {
    pub(super) fn into_domain(self, sharing: SharingMode) -> RateLimitConfig {
        RateLimitConfig {
            sharing,
            algorithm: self.algorithm.into(),
            sustained: SustainedRate {
                rate: self.sustained.rate,
                window: self.sustained.window.into(),
            },
            burst: self.burst.map(|b| BurstConfig {
                capacity: b.capacity,
            }),
            budget: self.budget.map(|b| BudgetConfig {
                mode: b.mode.into(),
                total: b.total,
                overcommit_ratio: b.overcommit_ratio,
            }),
            scope: self.scope.into(),
            strategy: self.strategy.into(),
            cost: self.cost,
            response_headers: self.response_headers,
            pool_owner_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// oagw_route.match_config (HTTP routes)
// ---------------------------------------------------------------------------

/// The HTTP match settings that have no typed column: the path prefix lives in
/// `oagw_route_http_match`, the methods in `oagw_route_method`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct StoredMatchConfig {
    pub(super) query_allowlist: Vec<String>,
    pub(super) path_suffix_mode: StoredPathSuffixMode,
}

impl From<&HttpMatch> for StoredMatchConfig {
    fn from(http: &HttpMatch) -> Self {
        Self {
            query_allowlist: http.query_allowlist.clone(),
            path_suffix_mode: http.path_suffix_mode.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use serde::de::DeserializeOwned;

    use super::*;

    /// Each domain value is stored as exactly `text` and reads back unchanged.
    #[track_caller]
    fn assert_texts<D, S>(cases: &[(D, &str)])
    where
        D: Copy + PartialEq + Debug,
        S: Serialize + DeserializeOwned + From<D> + Into<D>,
    {
        for &(domain, text) in cases {
            let json = serde_json::to_string(&S::from(domain)).unwrap();
            assert_eq!(json, format!("\"{text}\""), "{domain:?}");
            let back: S = serde_json::from_str(&json).unwrap();
            assert_eq!(back.into(), domain);
        }
    }

    #[test]
    fn every_stored_enum_text_round_trips() {
        assert_texts::<_, StoredSharingMode>(&[
            (SharingMode::Private, "private"),
            (SharingMode::Inherit, "inherit"),
            (SharingMode::Enforce, "enforce"),
        ]);
        assert_texts::<_, StoredScheme>(&[
            (Scheme::Http, "http"),
            (Scheme::Https, "https"),
            (Scheme::Wss, "wss"),
            (Scheme::Wt, "wt"),
            (Scheme::Grpc, "grpc"),
        ]);
        assert_texts::<_, StoredPassthroughMode>(&[
            (PassthroughMode::None, "none"),
            (PassthroughMode::Allowlist, "allowlist"),
            (PassthroughMode::All, "all"),
        ]);
        assert_texts::<_, StoredRateLimitAlgorithm>(&[
            (RateLimitAlgorithm::TokenBucket, "token_bucket"),
            (RateLimitAlgorithm::SlidingWindow, "sliding_window"),
        ]);
        assert_texts::<_, StoredWindow>(&[
            (Window::Second, "second"),
            (Window::Minute, "minute"),
            (Window::Hour, "hour"),
            (Window::Day, "day"),
        ]);
        assert_texts::<_, StoredBudgetMode>(&[
            (BudgetMode::Unlimited, "unlimited"),
            (BudgetMode::Allocated, "allocated"),
            (BudgetMode::Shared, "shared"),
        ]);
        assert_texts::<_, StoredRateLimitScope>(&[
            (RateLimitScope::Global, "global"),
            (RateLimitScope::Tenant, "tenant"),
            (RateLimitScope::User, "user"),
            (RateLimitScope::Ip, "ip"),
            (RateLimitScope::Route, "route"),
        ]);
        assert_texts::<_, StoredRateLimitStrategy>(&[
            (RateLimitStrategy::Reject, "reject"),
            (RateLimitStrategy::Queue, "queue"),
            (RateLimitStrategy::Degrade, "degrade"),
        ]);
        assert_texts::<_, StoredCorsMethod>(&[
            (CorsHttpMethod::Get, "GET"),
            (CorsHttpMethod::Post, "POST"),
            (CorsHttpMethod::Put, "PUT"),
            (CorsHttpMethod::Delete, "DELETE"),
            (CorsHttpMethod::Patch, "PATCH"),
            (CorsHttpMethod::Head, "HEAD"),
            (CorsHttpMethod::Options, "OPTIONS"),
        ]);
        assert_texts::<_, StoredPathSuffixMode>(&[
            (PathSuffixMode::Disabled, "disabled"),
            (PathSuffixMode::Append, "append"),
        ]);
    }

    #[test]
    fn unknown_enum_text_is_a_decode_error() {
        assert!(serde_json::from_str::<StoredSharingMode>("\"Private\"").is_err());
        assert!(serde_json::from_str::<StoredWindow>("\"week\"").is_err());
        assert!(serde_json::from_str::<StoredCorsMethod>("\"get\"").is_err());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let stored: StoredBurst =
            serde_json::from_str(r#"{"capacity":5,"added_later":true}"#).unwrap();
        assert_eq!(stored, StoredBurst { capacity: 5 });
    }

    #[test]
    fn cors_sharing_is_written_only_when_asked() {
        let cors = CorsConfig {
            sharing: SharingMode::Enforce,
            enabled: true,
            allowed_origins: vec!["https://a.example".into()],
            allowed_methods: vec![CorsHttpMethod::Get],
            expose_headers: vec![],
            allow_credentials: false,
        };
        let without = serde_json::to_value(StoredCors::new(&cors, false)).unwrap();
        assert!(without.get("sharing").is_none());
        let with = serde_json::to_value(StoredCors::new(&cors, true)).unwrap();
        assert_eq!(with["sharing"], "enforce");
    }
}
