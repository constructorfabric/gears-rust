//! Configuration of the AWS Signature Version 4 auth plugin, parsed once for
//! the write check and the plugin.

use std::collections::HashMap;

use credstore_sdk::{CredStoreError, SecretRef};
use toolkit_macros::domain_model;

use super::credentials::{
    ACCESS_KEY_ID_REF_KEY, AuthConfigError, SECRET_ACCESS_KEY_REF_KEY, SESSION_TOKEN_REF_KEY,
    credstore_error_kind, parse_credential_ref,
};

/// Auth config key of the signing region.
pub const REGION_KEY: &str = "region";
/// Auth config key of the signing name of the service.
pub const SERVICE_KEY: &str = "service";
/// Auth config key that allows a streamed body to be sent unsigned.
pub const UNSIGNED_PAYLOAD_KEY: &str = "unsigned_payload";
/// Auth config key of the path encoding.
pub const URI_ENCODING_KEY: &str = "uri_encoding";
/// Auth config key that controls the `X-Amz-Content-Sha256` header.
pub const CONTENT_SHA256_HEADER_KEY: &str = "content_sha256_header";

/// The field a `region` violation names.
pub const REGION_FIELD: &str = "auth.config.region";
/// The field a `service` violation names.
pub const SERVICE_FIELD: &str = "auth.config.service";
/// The field an `unsigned_payload` violation names.
pub const UNSIGNED_PAYLOAD_FIELD: &str = "auth.config.unsigned_payload";
/// The field a `uri_encoding` violation names.
pub const URI_ENCODING_FIELD: &str = "auth.config.uri_encoding";
/// The field a `content_sha256_header` violation names.
pub const CONTENT_SHA256_HEADER_FIELD: &str = "auth.config.content_sha256_header";

/// Longest `region` or `service`, in bytes.
const MAX_SCOPE_PART_LEN: usize = 64;

/// Signing names that default to [`UriEncoding::Single`]: Amazon S3, S3
/// Object Lambda, S3 on Outposts and S3 Express One Zone. The S3 and S3
/// Control endpoint rules set `disableDoubleEncoding` for these names, and
/// the AWS SDKs sign their paths without normalizing them (botocore's
/// `S3SigV4Auth`).
pub const S3_SIGNING_NAMES: [&str; 4] = ["s3", "s3-object-lambda", "s3-outposts", "s3express"];

/// How the path is encoded in the canonical request.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UriEncoding {
    /// The path as sent, without normalization, decoded and encoded once
    /// in SigV4 encoding (the S3 signing names, [`S3_SIGNING_NAMES`]).
    Single,
    /// The path as sent with dot segments and repeated slashes removed,
    /// URI-encoded again (every other service).
    Double,
}

impl UriEncoding {
    /// Stored value of [`Self::Single`].
    pub const SINGLE: &'static str = "single";
    /// Stored value of [`Self::Double`].
    pub const DOUBLE: &'static str = "double";
}

/// The AWS SigV4 auth plugin's config. The write check and the plugin both
/// read it through [`Self::parse`].
#[domain_model]
#[derive(Clone)]
pub struct AwsSigV4AuthConfig {
    /// Signing region, e.g. `us-east-1`.
    pub region: String,
    /// Signing name of the service, e.g. `bedrock`, `s3`, `execute-api`.
    pub service: String,
    /// Credential reference of the access key id.
    pub access_key_id_ref: SecretRef,
    /// Credential reference of the secret access key.
    pub secret_access_key_ref: SecretRef,
    /// Credential reference of the session token of temporary credentials.
    pub session_token_ref: Option<SecretRef>,
    /// Send a streamed body as `UNSIGNED-PAYLOAD` instead of refusing it.
    pub unsigned_payload: bool,
    /// Path encoding; unless configured, [`UriEncoding::Single`] for a
    /// `service` in [`S3_SIGNING_NAMES`], [`UriEncoding::Double`] otherwise.
    pub uri_encoding: UriEncoding,
    /// Send the payload hash as `X-Amz-Content-Sha256`.
    pub content_sha256_header: bool,
}

impl std::fmt::Debug for AwsSigV4AuthConfig {
    // The references stay out of logs and errors (F0008).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsSigV4AuthConfig")
            .field("region", &self.region)
            .field("service", &self.service)
            .field("session_token", &self.session_token_ref.is_some())
            .field("unsigned_payload", &self.unsigned_payload)
            .field("uri_encoding", &self.uri_encoding)
            .field("content_sha256_header", &self.content_sha256_header)
            .finish_non_exhaustive()
    }
}

impl AwsSigV4AuthConfig {
    /// Parse the plugin's config: `region`, `service`, `access_key_id_ref`
    /// and `secret_access_key_ref` are required; `session_token_ref`,
    /// `unsigned_payload`, `uri_encoding` and `content_sha256_header` are
    /// optional. Other keys are ignored.
    ///
    /// # Errors
    ///
    /// The first violation, naming its field and never quoting a reference.
    pub fn parse(config: Option<&HashMap<String, String>>) -> Result<Self, AuthConfigError> {
        let get = |key: &str| config.and_then(|c| c.get(key)).map(String::as_str);
        let region = scope_part(get(REGION_KEY), REGION_FIELD)?;
        let service = scope_part(get(SERVICE_KEY), SERVICE_FIELD)?;
        let access_key_id_ref = required_ref(get(ACCESS_KEY_ID_REF_KEY), ACCESS_KEY_ID_REF_FIELD)?;
        let secret_access_key_ref =
            required_ref(get(SECRET_ACCESS_KEY_REF_KEY), SECRET_ACCESS_KEY_REF_FIELD)?;
        let session_token_ref = get(SESSION_TOKEN_REF_KEY)
            .map(|value| credential_ref(value, SESSION_TOKEN_REF_FIELD))
            .transpose()?;
        let unsigned_payload = flag(get(UNSIGNED_PAYLOAD_KEY), UNSIGNED_PAYLOAD_FIELD, false)?;
        let content_sha256_header = flag(
            get(CONTENT_SHA256_HEADER_KEY),
            CONTENT_SHA256_HEADER_FIELD,
            true,
        )?;
        let uri_encoding = match get(URI_ENCODING_KEY) {
            None if S3_SIGNING_NAMES.contains(&service.as_str()) => UriEncoding::Single,
            None => UriEncoding::Double,
            Some(UriEncoding::SINGLE) => UriEncoding::Single,
            Some(UriEncoding::DOUBLE) => UriEncoding::Double,
            Some(_) => {
                return Err(AuthConfigError::new(
                    URI_ENCODING_FIELD,
                    oagw_sdk::field::INVALID_VALUE,
                    format!(
                        "{URI_ENCODING_FIELD}: must be '{}' or '{}'",
                        UriEncoding::SINGLE,
                        UriEncoding::DOUBLE
                    ),
                ));
            }
        };
        Ok(Self {
            region,
            service,
            access_key_id_ref,
            secret_access_key_ref,
            session_token_ref,
            unsigned_payload,
            uri_encoding,
            content_sha256_header,
        })
    }
}

/// The field an `access_key_id_ref` violation names.
pub const ACCESS_KEY_ID_REF_FIELD: &str = oagw_sdk::precondition::ACCESS_KEY_ID_REF;
/// The field a `secret_access_key_ref` violation names.
pub const SECRET_ACCESS_KEY_REF_FIELD: &str = oagw_sdk::precondition::SECRET_ACCESS_KEY_REF;
/// The field a `session_token_ref` violation names.
pub const SESSION_TOKEN_REF_FIELD: &str = oagw_sdk::precondition::SESSION_TOKEN_REF;

/// A credential-scope part (`region`, `service`): required, 1 to
/// [`MAX_SCOPE_PART_LEN`] ASCII letters, digits, `-`, `_` or `.`, the rule of
/// `CredentialScope::new`. AWS names are lowercase; S3-compatible servers may
/// use others, such as `RegionOne`. The scope is `/`-separated and goes into
/// the `Authorization` header.
fn scope_part(value: Option<&str>, field: &'static str) -> Result<String, AuthConfigError> {
    let value = value.ok_or_else(|| {
        AuthConfigError::new(
            field,
            oagw_sdk::field::REQUIRED,
            format!("{field}: required by the AWS SigV4 auth plugin"),
        )
    })?;
    let valid = !value.is_empty()
        && value.len() <= MAX_SCOPE_PART_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if !valid {
        return Err(AuthConfigError::new(
            field,
            oagw_sdk::field::INVALID_VALUE,
            format!(
                "{field}: must be 1 to {MAX_SCOPE_PART_LEN} ASCII letters, digits, '-', '_' or '.'"
            ),
        ));
    }
    Ok(value.to_owned())
}

fn required_ref(value: Option<&str>, field: &'static str) -> Result<SecretRef, AuthConfigError> {
    let value = value.ok_or_else(|| {
        AuthConfigError::new(
            field,
            oagw_sdk::field::REQUIRED,
            format!("{field}: required by the AWS SigV4 auth plugin"),
        )
    })?;
    credential_ref(value, field)
}

fn credential_ref(value: &str, field: &'static str) -> Result<SecretRef, AuthConfigError> {
    parse_credential_ref(value).map_err(|e| {
        let reason = match &e {
            CredStoreError::InvalidSecretRef { reason } => reason.clone(),
            // The store's own text may quote the reference (F0008).
            other => credstore_error_kind(other).to_owned(),
        };
        AuthConfigError::new(
            field,
            oagw_sdk::field::VALIDATION,
            format!("{field}: invalid credential reference: {reason}"),
        )
    })
}

fn flag(value: Option<&str>, field: &'static str, default: bool) -> Result<bool, AuthConfigError> {
    match value {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(AuthConfigError::new(
            field,
            oagw_sdk::field::INVALID_VALUE,
            format!("{field}: must be 'true' or 'false'"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    const MINIMAL: &[(&str, &str)] = &[
        ("region", "us-east-1"),
        ("service", "bedrock"),
        ("access_key_id_ref", "cred://aws-akid"),
        ("secret_access_key_ref", "aws-secret"),
    ];

    #[test]
    fn minimal_config_takes_the_defaults() {
        let parsed = AwsSigV4AuthConfig::parse(Some(&config(MINIMAL))).unwrap();
        assert_eq!(parsed.region, "us-east-1");
        assert_eq!(parsed.service, "bedrock");
        assert_eq!(parsed.access_key_id_ref.as_ref(), "aws-akid");
        assert_eq!(parsed.secret_access_key_ref.as_ref(), "aws-secret");
        assert!(parsed.session_token_ref.is_none());
        assert!(!parsed.unsigned_payload);
        assert!(parsed.content_sha256_header);
        assert_eq!(parsed.uri_encoding, UriEncoding::Double);
    }

    /// Every S3 signing name defaults to `single`, and a configured
    /// `uri_encoding` overrides it; a name that only starts like one, differs
    /// in case, or another service, defaults to `double`.
    #[test]
    fn uri_encoding_is_single_for_s3_unless_configured() {
        for service in ["s3", "s3-object-lambda", "s3-outposts", "s3express"] {
            let mut c = config(MINIMAL);
            c.insert("service".into(), service.into());
            let parsed = AwsSigV4AuthConfig::parse(Some(&c)).unwrap();
            assert_eq!(parsed.uri_encoding, UriEncoding::Single, "{service}");
            c.insert("uri_encoding".into(), "double".into());
            let parsed = AwsSigV4AuthConfig::parse(Some(&c)).unwrap();
            assert_eq!(parsed.uri_encoding, UriEncoding::Double, "{service}");
        }
        for service in [
            "s3x",
            "s3-outpost",
            "S3",
            "S3express",
            "execute-api",
            "bedrock",
        ] {
            let mut c = config(MINIMAL);
            c.insert("service".into(), service.into());
            let parsed = AwsSigV4AuthConfig::parse(Some(&c)).unwrap();
            assert_eq!(parsed.uri_encoding, UriEncoding::Double, "{service}");
        }
        let mut c = config(MINIMAL);
        c.insert("service".into(), "execute-api".into());
        c.insert("uri_encoding".into(), "single".into());
        let parsed = AwsSigV4AuthConfig::parse(Some(&c)).unwrap();
        assert_eq!(parsed.uri_encoding, UriEncoding::Single);
    }

    #[test]
    fn optional_keys_are_parsed() {
        let mut c = config(MINIMAL);
        c.insert("session_token_ref".into(), "cred://aws-token".into());
        c.insert("unsigned_payload".into(), "true".into());
        c.insert("content_sha256_header".into(), "false".into());
        let parsed = AwsSigV4AuthConfig::parse(Some(&c)).unwrap();
        assert_eq!(parsed.session_token_ref.unwrap().as_ref(), "aws-token");
        assert!(parsed.unsigned_payload);
        assert!(!parsed.content_sha256_header);
    }

    #[test]
    fn each_missing_required_key_names_its_field() {
        for (key, field) in [
            ("region", REGION_FIELD),
            ("service", SERVICE_FIELD),
            ("access_key_id_ref", ACCESS_KEY_ID_REF_FIELD),
            ("secret_access_key_ref", SECRET_ACCESS_KEY_REF_FIELD),
        ] {
            let mut c = config(MINIMAL);
            c.remove(key);
            let err = AwsSigV4AuthConfig::parse(Some(&c)).err().unwrap();
            assert_eq!(
                (err.field, err.reason),
                (field, oagw_sdk::field::REQUIRED),
                "{key}"
            );
            assert!(err.detail.contains(field), "{}", err.detail);
        }
        let err = AwsSigV4AuthConfig::parse(None).err().unwrap();
        assert_eq!(err.field, REGION_FIELD);
    }

    #[test]
    fn each_malformed_value_names_its_field_without_quoting_a_reference() {
        for (key, value, field, reason) in [
            (
                "region",
                "us east 1",
                REGION_FIELD,
                oagw_sdk::field::INVALID_VALUE,
            ),
            ("region", "", REGION_FIELD, oagw_sdk::field::INVALID_VALUE),
            (
                "service",
                "bed/rock",
                SERVICE_FIELD,
                oagw_sdk::field::INVALID_VALUE,
            ),
            (
                "access_key_id_ref",
                "not a ref",
                ACCESS_KEY_ID_REF_FIELD,
                oagw_sdk::field::VALIDATION,
            ),
            (
                "secret_access_key_ref",
                "not a ref",
                SECRET_ACCESS_KEY_REF_FIELD,
                oagw_sdk::field::VALIDATION,
            ),
            (
                "session_token_ref",
                "not a ref",
                SESSION_TOKEN_REF_FIELD,
                oagw_sdk::field::VALIDATION,
            ),
            (
                "unsigned_payload",
                "yes",
                UNSIGNED_PAYLOAD_FIELD,
                oagw_sdk::field::INVALID_VALUE,
            ),
            (
                "content_sha256_header",
                "1",
                CONTENT_SHA256_HEADER_FIELD,
                oagw_sdk::field::INVALID_VALUE,
            ),
            (
                "uri_encoding",
                "triple",
                URI_ENCODING_FIELD,
                oagw_sdk::field::INVALID_VALUE,
            ),
        ] {
            let mut c = config(MINIMAL);
            c.insert(key.into(), value.into());
            let err = AwsSigV4AuthConfig::parse(Some(&c)).err().unwrap();
            assert_eq!((err.field, err.reason), (field, reason), "{key}={value:?}");
            assert!(!err.detail.contains("not a ref"), "{}", err.detail);
        }
        let long = "a".repeat(MAX_SCOPE_PART_LEN + 1);
        let mut c = config(MINIMAL);
        c.insert("region".into(), long);
        assert_eq!(
            AwsSigV4AuthConfig::parse(Some(&c)).err().unwrap().field,
            REGION_FIELD
        );
    }

    /// `region` and `service` take ASCII letters of either case, digits,
    /// `-`, `_` and `.`, up to 64 bytes: S3-compatible servers use names
    /// such as `RegionOne`.
    #[test]
    fn scope_parts_take_mixed_case_and_punctuation() {
        let longest = "a".repeat(MAX_SCOPE_PART_LEN);
        for value in [
            "RegionOne",
            "us-east-1",
            "my_region.2",
            "S3",
            longest.as_str(),
        ] {
            for key in ["region", "service"] {
                let mut c = config(MINIMAL);
                c.insert(key.into(), value.into());
                let parsed = AwsSigV4AuthConfig::parse(Some(&c)).unwrap();
                let got = if key == "region" {
                    &parsed.region
                } else {
                    &parsed.service
                };
                assert_eq!(got, value, "{key}={value}");
            }
        }
        for value in ["us/east", "us:east", "r\u{e9}gion", "a b", "a\tb"] {
            for (key, field) in [("region", REGION_FIELD), ("service", SERVICE_FIELD)] {
                let mut c = config(MINIMAL);
                c.insert(key.into(), value.into());
                let err = AwsSigV4AuthConfig::parse(Some(&c)).err().unwrap();
                assert_eq!(
                    (err.field, err.reason),
                    (field, oagw_sdk::field::INVALID_VALUE),
                    "{key}={value:?}"
                );
            }
        }
    }

    #[test]
    fn debug_does_not_print_the_references() {
        let mut c = config(MINIMAL);
        c.insert("session_token_ref".into(), "cred://aws-token".into());
        let parsed = AwsSigV4AuthConfig::parse(Some(&c)).unwrap();
        let rendered = format!("{parsed:?}");
        for reference in ["aws-akid", "aws-secret", "aws-token"] {
            assert!(!rendered.contains(reference), "{rendered}");
        }
        assert!(rendered.contains("us-east-1"));
    }
}
