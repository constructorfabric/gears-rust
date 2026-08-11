//! REST DTOs for the token-issuer OBO re-mint endpoint (gated by `obo.enabled`).

/// Request body for `POST /internal/v1/issuers/obo/tokens`.
///
/// The whole body is optional; when present, `scopes` narrows the OBO grant to
/// a subset of what the down-scope (Gate 2) would otherwise yield. Omitting it
/// (or sending an empty/no body) takes the full down-scoped grant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct RemintRequest {
    /// Optional requested scope subset. Must be a subset of the down-scoped
    /// grant; an empty list is rejected (never mint an empty-scope OBO), and the
    /// list is bounded (≤64 entries, ≤256 chars each) at the handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
}

/// Response body for `POST /internal/v1/issuers/obo/tokens`: the minted OBO
/// token in compact JWS form.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct RemintResponse {
    /// The minted OBO token (`typ=obo+jwt`, `aud=public-api`), compact-serialized.
    pub token: String,
}
