use secrecy::SecretString;
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

/// Error returned when `SecurityContextBuilder::build()` is called without
/// required fields.
#[derive(Debug, thiserror::Error)]
pub enum SecurityContextBuildError {
    #[error(
        "subject_id is required - use SecurityContext::anonymous() for unauthenticated contexts"
    )]
    MissingSubjectId,
    #[error(
        "subject_tenant_id is required - use SecurityContext::anonymous() for unauthenticated contexts"
    )]
    MissingSubjectTenantId,
    #[error("expires_at is outside the representable range once converted to UTC")]
    ExpiresAtOutOfRange,
}

/// `SecurityContext` encapsulates the security-related information for a request or operation.
///
/// Built by the `AuthN` Resolver during authentication and passed through the request lifecycle.
/// Gears use this context together with the `AuthZ` Resolver to obtain access scopes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecurityContext {
    /// Subject ID — the authenticated user, service, or system making the request.
    subject_id: Uuid,
    /// Subject type classification (e.g., "user", "service").
    subject_type: Option<String>,
    /// Subject's home tenant (from `AuthN`). Required — every authenticated
    /// subject belongs to a tenant.
    subject_tenant_id: Uuid,
    /// Token capability restrictions. `["*"]` means first-party / unrestricted.
    ///
    /// **Empty means no capability was granted, not unrestricted.** This field
    /// used to be documented the other way round — "treat as unrestricted for
    /// backward compatibility" — while the gateway enforcer did the opposite
    /// and fail-closed on an empty list. A consumer following the old wording
    /// would turn a context carrying no scopes into full access.
    ///
    /// Read it through [`SecurityContext::has_scope`] rather than inspecting
    /// the list, so the `["*"]` rule lives in one place.
    #[serde(default)]
    token_scopes: Vec<String>,
    /// Original bearer token for PDP forwarding. Never serialized/persisted.
    /// Wrapped in `SecretString` so `Debug` redacts the value automatically.
    #[serde(skip)]
    bearer_token: Option<SecretString>,
    /// When the verified or exchanged credential behind this context expires.
    ///
    /// JWT `exp` or the `OAuth2` token deadline, never the `client_secret`
    /// lifetime. Always UTC; revocation may happen earlier. `None` means
    /// unknown expiry, not indefinite validity.
    ///
    /// `#[serde(default)]` accepts older JSON payloads. Positional formats
    /// (`x-secctx-bin` postcard) require matching layouts and codec versions.
    #[serde(default, with = "rfc3339_option")]
    expires_at: Option<OffsetDateTime>,
}

/// `Option<OffsetDateTime>` as an `Option<String>` holding RFC 3339.
///
/// Uses `Option<String>` because `time::serde::rfc3339::option` requires a
/// self-describing format and fails with postcard's `WontImplement`.
mod rfc3339_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser};
    use time::format_description::well_known::Rfc3339;
    use time::{OffsetDateTime, UtcOffset};

    #[expect(clippy::ref_option, reason = "signature fixed by `#[serde(with)]`")]
    pub(super) fn serialize<S: Serializer>(
        value: &Option<OffsetDateTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .map(|t| t.format(&Rfc3339))
            .transpose()
            .map_err(ser::Error::custom)?
            .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<OffsetDateTime>, D::Error> {
        let Some(text) = Option::<String>::deserialize(deserializer)? else {
            return Ok(None);
        };
        let parsed = OffsetDateTime::parse(&text, &Rfc3339).map_err(de::Error::custom)?;
        // RFC 3339 parsing checks the local date-time only; the UTC instant can
        // still fall outside `time`'s range, where `to_offset` would panic.
        parsed
            .checked_to_offset(UtcOffset::UTC)
            .map(Some)
            .ok_or_else(|| de::Error::custom("expires_at is out of range in UTC"))
    }
}

impl SecurityContext {
    /// Create a new `SecurityContext` builder
    #[must_use]
    pub fn builder() -> SecurityContextBuilder {
        SecurityContextBuilder::default()
    }

    /// Create an anonymous `SecurityContext` with no tenant, subject, or permissions.
    ///
    /// Use this for unauthenticated / dev / auth-disabled contexts where no
    /// authenticated subject exists.
    #[must_use]
    pub fn anonymous() -> Self {
        Self {
            subject_id: Uuid::default(),
            subject_type: None,
            subject_tenant_id: Uuid::default(),
            token_scopes: Vec::new(),
            bearer_token: None,
            expires_at: None,
        }
    }

    /// Get the subject ID (user, service, or system) associated with the security context
    #[must_use]
    pub fn subject_id(&self) -> Uuid {
        self.subject_id
    }

    /// Get the subject type classification (e.g., "user", "service").
    #[must_use]
    pub fn subject_type(&self) -> Option<&str> {
        self.subject_type.as_deref()
    }

    /// Get the subject's home tenant ID (from `AuthN` token).
    #[must_use]
    pub fn subject_tenant_id(&self) -> Uuid {
        self.subject_tenant_id
    }

    /// Get the token scopes. `["*"]` means first-party / unrestricted.
    #[must_use]
    pub fn token_scopes(&self) -> &[String] {
        &self.token_scopes
    }

    /// Whether this context has no authenticated subject.
    ///
    /// Anonymity is encoded as a nil `subject_id` / `subject_tenant_id`, the
    /// same fields a real subject uses, so there is nothing on the type that
    /// separates the two. Every consumer that cared was re-deriving this by
    /// hand — `ctx.subject_id().is_nil() || ctx.subject_tenant_id().is_nil()`,
    /// written out identically in the ledger and pricing gears — and a caller
    /// that forgets the check treats an unauthenticated request as a subject in
    /// the nil tenant.
    #[must_use]
    pub fn is_anonymous(&self) -> bool {
        self.subject_id.is_nil() || self.subject_tenant_id.is_nil()
    }

    /// Whether the token carries `scope`.
    ///
    /// The wildcard `"*"` satisfies every scope: it is what a first-party
    /// caller presents. An empty list satisfies none — it means no capability
    /// was granted, which is why this is the accessor to reason with rather
    /// than the raw list, where "empty" has repeatedly been read as its
    /// opposite.
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.token_scopes
            .iter()
            .any(|granted| granted == "*" || granted == scope)
    }

    /// Get the original bearer token (for PDP forwarding).
    #[must_use]
    pub fn bearer_token(&self) -> Option<&SecretString> {
        self.bearer_token.as_ref()
    }

    /// When the verified or exchanged credential behind this context expires.
    ///
    /// An upper bound: the credential can be revoked earlier. `None` means the
    /// expiry is unknown, not that the context never expires.
    #[must_use]
    pub fn expires_at(&self) -> Option<OffsetDateTime> {
        self.expires_at
    }
}

/// Builds a [`SecurityContext`] field by field.
///
/// `subject_id` and `subject_tenant_id` are required; [`Self::build`] reports a
/// missing one rather than defaulting it, since a nil subject is how an
/// *anonymous* context is represented and silently producing one would turn a
/// wiring mistake into an unauthenticated caller. Use
/// [`SecurityContext::anonymous`] when that is what you actually mean.
///
/// `Debug` never renders the bearer token: the field holds a `SecretString`,
/// which redacts itself.
#[derive(Debug, Default)]
pub struct SecurityContextBuilder {
    subject_id: Option<Uuid>,
    subject_type: Option<String>,
    subject_tenant_id: Option<Uuid>,
    token_scopes: Vec<String>,
    bearer_token: Option<SecretString>,
    expires_at: Option<OffsetDateTime>,
}

impl SecurityContextBuilder {
    /// Set the subject's unique id. Required.
    #[must_use]
    pub fn subject_id(mut self, subject_id: Uuid) -> Self {
        self.subject_id = Some(subject_id);
        self
    }

    /// Set the subject's classification, e.g. `"user"` or `"service"`.
    ///
    /// Optional to build, but it is the positive marker a real `AuthN` resolver
    /// always populates, so consumers use its absence to spot a context that
    /// never went through authentication.
    #[must_use]
    pub fn subject_type(mut self, subject_type: &str) -> Self {
        self.subject_type = Some(subject_type.to_owned());
        self
    }

    /// Set the subject's home tenant. Required.
    #[must_use]
    pub fn subject_tenant_id(mut self, subject_tenant_id: Uuid) -> Self {
        self.subject_tenant_id = Some(subject_tenant_id);
        self
    }

    /// Set the token's capability scopes.
    ///
    /// `["*"]` is first-party / unrestricted. See the field documentation on
    /// [`SecurityContext`] for what an empty list means.
    #[must_use]
    pub fn token_scopes(mut self, scopes: Vec<String>) -> Self {
        self.token_scopes = scopes;
        self
    }

    /// Carry the original bearer token, for forwarding to a policy decision
    /// point. Never serialized and never rendered by `Debug`.
    #[must_use]
    pub fn bearer_token(mut self, token: impl Into<SecretString>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    /// Set when the verified or exchanged credential expires.
    ///
    /// Use the credential's own expiry (JWT `exp`, `OAuth2` token deadline),
    /// never the lifetime of a long-lived `client_secret`. Stored in UTC;
    /// [`build`](Self::build) fails if the instant cannot be represented there.
    #[must_use]
    pub fn expires_at(mut self, expires_at: OffsetDateTime) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Build the `SecurityContext`.
    ///
    /// # Errors
    ///
    /// Returns `SecurityContextBuildError` if `subject_id` or
    /// `subject_tenant_id` was not set. Use `SecurityContext::anonymous()`
    /// for contexts that intentionally have no authenticated subject.
    /// Returns [`SecurityContextBuildError::ExpiresAtOutOfRange`] if
    /// `expires_at` has no UTC representation.
    pub fn build(self) -> Result<SecurityContext, SecurityContextBuildError> {
        let subject_id = self
            .subject_id
            .ok_or(SecurityContextBuildError::MissingSubjectId)?;
        let subject_tenant_id = self
            .subject_tenant_id
            .ok_or(SecurityContextBuildError::MissingSubjectTenantId)?;
        let expires_at = self
            .expires_at
            .map(|t| {
                t.checked_to_offset(UtcOffset::UTC)
                    .ok_or(SecurityContextBuildError::ExpiresAtOutOfRange)
            })
            .transpose()?;
        Ok(SecurityContext {
            subject_id,
            subject_type: self.subject_type,
            subject_tenant_id,
            token_scopes: self.token_scopes,
            bearer_token: self.bearer_token,
            expires_at,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn test_security_context_builder_full() {
        let subject_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
        let subject_tenant_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap();

        let ctx = SecurityContext::builder()
            .subject_id(subject_id)
            .subject_type("user")
            .subject_tenant_id(subject_tenant_id)
            .token_scopes(vec!["read:events".to_owned(), "write:events".to_owned()])
            .bearer_token("test-token-123".to_owned())
            .build()
            .unwrap();

        assert_eq!(ctx.subject_id(), subject_id);
        assert_eq!(ctx.subject_tenant_id(), subject_tenant_id);
        assert_eq!(ctx.token_scopes(), &["read:events", "write:events"]);
        assert_eq!(
            ctx.bearer_token().map(ExposeSecret::expose_secret),
            Some("test-token-123"),
        );
    }

    #[test]
    fn test_security_context_builder_missing_subject_id() {
        let err = SecurityContext::builder()
            .subject_tenant_id(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap())
            .build();

        assert!(matches!(
            err,
            Err(SecurityContextBuildError::MissingSubjectId)
        ));
    }

    #[test]
    fn test_security_context_builder_missing_tenant_id() {
        let err = SecurityContext::builder()
            .subject_id(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap())
            .build();

        assert!(matches!(
            err,
            Err(SecurityContextBuildError::MissingSubjectTenantId)
        ));
    }

    #[test]
    fn test_security_context_builder_missing_both() {
        let err = SecurityContext::builder().build();

        assert!(matches!(
            err,
            Err(SecurityContextBuildError::MissingSubjectId)
        ));
    }

    #[test]
    fn test_security_context_anonymous() {
        let ctx = SecurityContext::anonymous();

        assert_eq!(ctx.subject_id(), Uuid::default());
        assert_eq!(ctx.subject_tenant_id(), Uuid::default());
        assert!(ctx.token_scopes().is_empty());
        assert!(ctx.bearer_token().is_none());
    }

    #[test]
    fn test_security_context_builder_keeps_the_last_value_set() {
        // `test_security_context_builder_full` already covers a plain chain
        // with more assertions than this did. What nothing covered is a setter
        // called twice: a builder that accumulated instead of replacing would
        // pass every other test here.
        let first = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
        let second = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440003").unwrap();
        let subject_tenant_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap();

        let ctx = SecurityContext::builder()
            .subject_id(first)
            .subject_type("user")
            .subject_id(second)
            .subject_tenant_id(subject_tenant_id)
            .token_scopes(vec!["read".to_owned()])
            .token_scopes(vec!["write".to_owned()])
            .build()
            .unwrap();

        assert_eq!(ctx.subject_id(), second);
        assert_eq!(ctx.token_scopes(), ["write".to_owned()]);
    }

    #[test]
    fn test_security_context_clone() {
        let subject_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
        let subject_tenant_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap();

        let ctx1 = SecurityContext::builder()
            .subject_id(subject_id)
            .subject_tenant_id(subject_tenant_id)
            .token_scopes(vec!["*".to_owned()])
            .bearer_token("secret".to_owned())
            .build()
            .unwrap();

        let ctx2 = ctx1.clone();

        assert_eq!(ctx2.subject_id(), ctx1.subject_id());
        assert_eq!(ctx2.subject_tenant_id(), ctx1.subject_tenant_id());
        assert_eq!(ctx2.token_scopes(), ctx1.token_scopes());
        assert_eq!(
            ctx2.bearer_token().map(ExposeSecret::expose_secret),
            ctx1.bearer_token().map(ExposeSecret::expose_secret),
        );
    }

    #[test]
    fn test_security_context_serialize_deserialize() {
        let subject_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
        let subject_tenant_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap();

        let original = SecurityContext::builder()
            .subject_id(subject_id)
            .subject_type("user")
            .subject_tenant_id(subject_tenant_id)
            .token_scopes(vec!["admin".to_owned()])
            .bearer_token("secret-token".to_owned())
            .build()
            .unwrap();

        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: SecurityContext = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.subject_id(), original.subject_id());
        assert_eq!(
            deserialized.subject_tenant_id(),
            original.subject_tenant_id()
        );
        assert_eq!(deserialized.token_scopes(), original.token_scopes());
        // bearer_token is skipped during serialization
        assert!(deserialized.bearer_token().is_none());
    }

    #[test]
    fn test_security_context_bearer_token_not_serialized() {
        // Built *with* a token: `anonymous()` has none, so asserting on it only
        // checked that an absent value stays absent -- which keeps passing if
        // `#[serde(skip)]` is replaced by anything that skips `None` but writes
        // a real token, the exact case this guards.
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(2))
            .bearer_token("super-secret-token")
            .build()
            .unwrap();
        assert!(ctx.bearer_token().is_some(), "guard: the token is set");

        let serialized = serde_json::to_string(&ctx).unwrap();
        assert!(
            !serialized.contains("bearer_token"),
            "the field name must not appear: {serialized}"
        );
        assert!(
            !serialized.contains("super-secret-token"),
            "the token value must not appear: {serialized}"
        );
    }

    #[test]
    fn is_anonymous_separates_an_unauthenticated_context_from_a_real_subject() {
        assert!(SecurityContext::anonymous().is_anonymous());

        let authenticated = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(2))
            .build()
            .unwrap();
        assert!(!authenticated.is_anonymous());

        // Either field being nil is enough: a subject with no tenant is not a
        // subject this system can authorize.
        let no_tenant = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::nil())
            .build()
            .unwrap();
        assert!(no_tenant.is_anonymous());
    }

    #[test]
    fn has_scope_treats_empty_as_no_capability_and_wildcard_as_all() {
        let build = |scopes: Vec<String>| {
            SecurityContext::builder()
                .subject_id(Uuid::from_u128(1))
                .subject_tenant_id(Uuid::from_u128(2))
                .token_scopes(scopes)
                .build()
                .unwrap()
        };

        let none = build(Vec::new());
        assert!(
            !none.has_scope("read:events"),
            "an empty scope list grants nothing; it once read as unrestricted"
        );

        let wildcard = build(vec!["*".to_owned()]);
        assert!(wildcard.has_scope("read:events"));
        assert!(wildcard.has_scope("anything-at-all"));

        let specific = build(vec!["read:events".to_owned()]);
        assert!(specific.has_scope("read:events"));
        assert!(!specific.has_scope("write:events"));
    }

    #[test]
    fn test_security_context_empty_scopes() {
        // `anonymous()` is covered by `test_security_context_anonymous`; the
        // case no other test reaches is a builder-supplied empty scope list.
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(2))
            .token_scopes(Vec::new())
            .build()
            .unwrap();

        assert!(ctx.token_scopes().is_empty());
    }

    #[test]
    fn expires_at_round_trips_through_json_as_utc_rfc3339() {
        let in_plus_two = OffsetDateTime::from_unix_timestamp(1_893_499_200)
            .unwrap()
            .to_offset(UtcOffset::from_hms(2, 0, 0).unwrap());
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(2))
            .expires_at(in_plus_two)
            .build()
            .unwrap();

        assert_eq!(ctx.expires_at(), Some(in_plus_two));
        assert_eq!(ctx.expires_at().unwrap().offset(), UtcOffset::UTC);

        let json = serde_json::to_value(&ctx).unwrap();
        assert_eq!(json["expires_at"], "2030-01-01T12:00:00Z");
        let back: SecurityContext = serde_json::from_value(json).unwrap();
        assert_eq!(back.expires_at(), Some(in_plus_two));
    }

    #[test]
    fn json_written_before_expires_at_existed_still_deserializes() {
        let old = serde_json::json!({
            "subject_id": "550e8400-e29b-41d4-a716-446655440001",
            "subject_type": "service",
            "subject_tenant_id": "550e8400-e29b-41d4-a716-446655440002",
            "token_scopes": ["*"],
        });

        let ctx: SecurityContext = serde_json::from_value(old).unwrap();

        assert!(ctx.expires_at().is_none());
        assert_eq!(ctx.token_scopes(), ["*".to_owned()]);

        let with_offset = serde_json::json!({
            "subject_id": "550e8400-e29b-41d4-a716-446655440001",
            "subject_tenant_id": "550e8400-e29b-41d4-a716-446655440002",
            "expires_at": "2030-01-01T14:00:00+02:00",
        });
        let ctx: SecurityContext = serde_json::from_value(with_offset).unwrap();
        let expires_at = ctx.expires_at().unwrap();
        assert_eq!(expires_at.offset(), UtcOffset::UTC, "normalized on read");
        assert_eq!(expires_at.unix_timestamp(), 1_893_499_200);
    }

    #[test]
    fn json_with_a_malformed_expires_at_is_rejected() {
        let payload = serde_json::json!({
            "subject_id": "550e8400-e29b-41d4-a716-446655440001",
            "subject_tenant_id": "550e8400-e29b-41d4-a716-446655440002",
            "expires_at": "2030-01-01 12:00",
        });

        assert!(serde_json::from_value::<SecurityContext>(payload).is_err());
    }

    #[test]
    fn json_with_an_expires_at_out_of_range_in_utc_is_rejected() {
        // Valid RFC 3339, but year 10000 once normalized to UTC.
        let payload = serde_json::json!({
            "subject_id": "550e8400-e29b-41d4-a716-446655440001",
            "subject_tenant_id": "550e8400-e29b-41d4-a716-446655440002",
            "expires_at": "9999-12-31T23:59:59-01:00",
        });

        let err = serde_json::from_value::<SecurityContext>(payload)
            .expect_err("out-of-range expiry must be a decode error, not a panic");
        assert!(err.to_string().contains("expires_at"), "{err}");
    }

    #[test]
    fn builder_rejects_an_expires_at_out_of_range_in_utc() {
        let year_9999_minus_one = time::PrimitiveDateTime::new(
            time::Date::from_calendar_date(9999, time::Month::December, 31).unwrap(),
            time::Time::from_hms(23, 30, 0).unwrap(),
        )
        .assume_offset(UtcOffset::from_hms(-1, 0, 0).unwrap());

        let err = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(2))
            .expires_at(year_9999_minus_one)
            .build()
            .expect_err("expiry past the UTC range must not build");
        assert!(
            matches!(err, SecurityContextBuildError::ExpiresAtOutOfRange),
            "{err:?}"
        );
    }

    #[test]
    fn expires_at_defaults_to_unknown() {
        assert!(SecurityContext::anonymous().expires_at().is_none());
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(2))
            .build()
            .unwrap();
        assert!(ctx.expires_at().is_none());
    }
}
