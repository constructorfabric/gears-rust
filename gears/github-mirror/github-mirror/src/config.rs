use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct GithubMirrorConfig {
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    /// Temporary shortcut until credstore integration (gears-rust#4534):
    /// GitHub token used by sync. Unauthenticated requests work for public
    /// repositories at a much lower rate limit.
    #[serde(default)]
    pub github_token: Option<String>,
}

impl Default for GithubMirrorConfig {
    fn default() -> Self {
        Self {
            api_base_url: default_api_base_url(),
            github_token: None,
        }
    }
}

fn default_api_base_url() -> String {
    "https://api.github.com".to_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn default_points_at_public_github_api() {
        let cfg = GithubMirrorConfig::default();
        assert_eq!(cfg.api_base_url, "https://api.github.com");
    }

    #[test]
    fn deserializes_with_missing_field_using_default() {
        let cfg: GithubMirrorConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.api_base_url, "https://api.github.com");
    }

    #[test]
    fn deserializes_explicit_base_url() {
        let cfg: GithubMirrorConfig =
            serde_json::from_str(r#"{"api_base_url":"https://ghe.local/api/v3"}"#).unwrap();
        assert_eq!(cfg.api_base_url, "https://ghe.local/api/v3");
    }
}
