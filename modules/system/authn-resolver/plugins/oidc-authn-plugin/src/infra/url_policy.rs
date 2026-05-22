//! URL validation for outbound `IdP` communication.

use reqwest::Url;

/// Standard OIDC discovery document path appended to issuer/discovery bases.
pub(crate) const OIDC_DISCOVERY_PATH: &str = "/.well-known/openid-configuration";

/// URL security policy for IdP-facing endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UrlSecurityPolicy {
    allow_insecure_http: bool,
}

impl UrlSecurityPolicy {
    pub(crate) const STRICT: Self = Self {
        allow_insecure_http: false,
    };

    /// Build a policy that permits plain HTTP for test-only mock servers.
    #[doc(hidden)]
    pub const fn allow_insecure_http_for_tests() -> Self {
        Self {
            allow_insecure_http: true,
        }
    }

    pub(crate) fn validate_url(self, value: &str, field: &str) -> Result<Url, String> {
        let url =
            Url::parse(value).map_err(|error| format!("{field} must be a valid URL: {error}"))?;
        self.validate_parsed_url(&url, field)?;

        Ok(url)
    }

    fn validate_parsed_url(self, url: &Url, field: &str) -> Result<(), String> {
        match url.scheme() {
            "https" => {}
            "http" if self.allow_insecure_http => {}
            scheme => {
                return Err(format!(
                    "{field} must use https; scheme {scheme:?} is not allowed"
                ));
            }
        }

        if url.host_str().is_none() {
            return Err(format!("{field} must include a host"));
        }

        if !url.username().is_empty() || url.password().is_some() {
            return Err(format!("{field} must not include credentials"));
        }

        if url.fragment().is_some() {
            return Err(format!("{field} must not include a fragment"));
        }

        Ok(())
    }

    pub(crate) fn validate_oidc_base(self, value: &str, field: &str) -> Result<Url, String> {
        let url = self.validate_url(value, field)?;

        if url.query().is_some() {
            return Err(format!("{field} must not include a query string"));
        }

        Ok(url)
    }

    pub(crate) fn discovery_document_url(self, value: &Url, field: &str) -> Result<Url, String> {
        self.validate_parsed_url(value, field)?;

        let mut url = value.clone();

        if url.query().is_some() {
            return Err(format!("{field} must not include a query string"));
        }

        let path = url.path().trim_end_matches('/');
        if path.ends_with(OIDC_DISCOVERY_PATH) {
            return Ok(url);
        }

        let discovery_path = if path.is_empty() {
            OIDC_DISCOVERY_PATH.to_owned()
        } else {
            format!("{path}{OIDC_DISCOVERY_PATH}")
        };
        url.set_path(&discovery_path);

        Ok(url)
    }
}
