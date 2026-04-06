use secrecy::SecretString;
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
}

/// `SecurityContext` encapsulates the security-related information for a request or operation.
///
/// Built by the `AuthN` Resolver during authentication and passed through the request lifecycle.
/// Modules use this context together with the `AuthZ` Resolver to obtain access scopes.
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
    /// Empty means no scopes were asserted (treat as unrestricted for backward compatibility).
    #[serde(default)]
    token_scopes: Vec<String>,
    /// Original bearer token for PDP forwarding. Never serialized/persisted.
    /// Wrapped in `SecretString` so `Debug` redacts the value automatically.
    #[serde(skip)]
    bearer_token: Option<SecretString>,
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

    /// Get the original bearer token (for PDP forwarding).
    #[must_use]
    pub fn bearer_token(&self) -> Option<&SecretString> {
        self.bearer_token.as_ref()
    }
}

#[derive(Default)]
pub struct SecurityContextBuilder {
    subject_id: Option<Uuid>,
    subject_type: Option<String>,
    subject_tenant_id: Option<Uuid>,
    token_scopes: Vec<String>,
    bearer_token: Option<SecretString>,
}

impl SecurityContextBuilder {
    #[must_use]
    pub fn subject_id(mut self, subject_id: Uuid) -> Self {
        self.subject_id = Some(subject_id);
        self
    }

    #[must_use]
    pub fn subject_type(mut self, subject_type: &str) -> Self {
        self.subject_type = Some(subject_type.to_owned());
        self
    }

    #[must_use]
    pub fn subject_tenant_id(mut self, subject_tenant_id: Uuid) -> Self {
        self.subject_tenant_id = Some(subject_tenant_id);
        self
    }

    #[must_use]
    pub fn token_scopes(mut self, scopes: Vec<String>) -> Self {
        self.token_scopes = scopes;
        self
    }

    #[must_use]
    pub fn bearer_token(mut self, token: impl Into<SecretString>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    /// Build the `SecurityContext`.
    ///
    /// # Errors
    ///
    /// Returns `SecurityContextBuildError` if `subject_id` or
    /// `subject_tenant_id` was not set. Use `SecurityContext::anonymous()`
    /// for contexts that intentionally have no authenticated subject.
    pub fn build(self) -> Result<SecurityContext, SecurityContextBuildError> {
        let subject_id = self
            .subject_id
            .ok_or(SecurityContextBuildError::MissingSubjectId)?;
        let subject_tenant_id = self
            .subject_tenant_id
            .ok_or(SecurityContextBuildError::MissingSubjectTenantId)?;
        Ok(SecurityContext {
            subject_id,
            subject_type: self.subject_type,
            subject_tenant_id,
            token_scopes: self.token_scopes,
            bearer_token: self.bearer_token,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "context_tests.rs"]
mod tests;
