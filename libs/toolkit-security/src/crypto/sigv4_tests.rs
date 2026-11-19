use super::{CredentialScope, CryptoError, SigningKey, derive_key, hex_lower, sha256_hex};

const EXAMPLE_SECRET: &[u8] = b"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

fn scope(date: &str) -> CredentialScope {
    CredentialScope::new(date, "us-east-1", "iam").unwrap()
}

#[test]
fn derives_the_aws_documented_signing_key() {
    // AWS SigV4 docs, "Examples of how to derive a signing key": kSigning.
    let key = derive_key(EXAMPLE_SECRET, &scope("20120215")).unwrap();
    assert_eq!(
        hex::encode(key.as_ref()),
        "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d"
    );
}

#[test]
fn signs_the_aws_documented_iam_list_users_request() {
    // AWS SigV4 docs, GET iam.amazonaws.com ListUsers example (2015-08-30).
    let canonical_request = "GET\n\
        /\n\
        Action=ListUsers&Version=2010-05-08\n\
        content-type:application/x-www-form-urlencoded; charset=utf-8\n\
        host:iam.amazonaws.com\n\
        x-amz-date:20150830T123600Z\n\
        \n\
        content-type;host;x-amz-date\n\
        e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let canonical_hash = sha256_hex(canonical_request.as_bytes()).unwrap();
    assert_eq!(
        canonical_hash,
        "f536975d06c0309214f805bb90ccff089219ecd68b2577efef23edd43b7e1a59"
    );

    let scope = scope("20150830");
    assert_eq!(
        hex::encode(derive_key(EXAMPLE_SECRET, &scope).unwrap().as_ref()),
        "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
    );

    let string_to_sign = format!("AWS4-HMAC-SHA256\n20150830T123600Z\n{scope}\n{canonical_hash}");
    let key = SigningKey::derive(EXAMPLE_SECRET, &scope).unwrap();
    assert_eq!(
        key.signature(string_to_sign.as_bytes()).unwrap(),
        "5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
    );
}

#[test]
fn sha256_hex_of_empty_payload() {
    assert_eq!(
        sha256_hex(b"").unwrap(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn scope_renders_and_splits_back_into_its_parts() {
    let scope = CredentialScope::new("20150830", "us-gov-west-1", "execute-api").unwrap();
    assert_eq!(
        scope.as_str(),
        "20150830/us-gov-west-1/execute-api/aws4_request"
    );
    assert_eq!(scope.to_string(), scope.as_str());
    assert_eq!(scope.date(), "20150830");
    assert_eq!(scope.region(), "us-gov-west-1");
    assert_eq!(scope.service(), "execute-api");

    let key = SigningKey::derive(EXAMPLE_SECRET, &scope).unwrap();
    assert_eq!(key.scope(), &scope);
}

#[test]
fn debug_output_contains_no_key_material() {
    let scope = scope("20120215");
    let key = SigningKey::derive(EXAMPLE_SECRET, &scope).unwrap();
    let rendered = format!("{key:?}");
    assert!(rendered.starts_with("SigningKey"), "{rendered}");
    assert!(rendered.contains("20120215/us-east-1/iam/aws4_request"));
    assert!(!rendered.contains(std::str::from_utf8(EXAMPLE_SECRET).unwrap()));
    let signing_key = derive_key(EXAMPLE_SECRET, &scope).unwrap();
    assert!(!rendered.contains(&hex::encode(signing_key.as_ref())));
    assert!(!rendered.contains(&format!("{:?}", signing_key.as_ref())));
}

#[test]
fn rejects_malformed_scopes() {
    let long = "a".repeat(65);
    for (date, region, service) in [
        ("20150830", "", "iam"),
        ("20150830", "us-east-1", ""),
        ("", "us-east-1", "iam"),
        ("2015083", "us-east-1", "iam"),
        ("201508300", "us-east-1", "iam"),
        ("2015-8-3", "us-east-1", "iam"),
        ("20150830T123600Z", "us-east-1", "iam"),
        // Eight bytes, but not ASCII digits.
        ("2015\u{663}\u{663}", "us-east-1", "iam"),
        ("20151330", "us-east-1", "iam"),
        ("20150000", "us-east-1", "iam"),
        ("20150832", "us-east-1", "iam"),
        ("20150830", "us-east-1", "i\u{e4}m"),
        ("20150830", "us-east-1", "i am"),
        ("20150830", "us/east-1", "iam"),
        ("20150830", "us-east-1", "iam\n"),
        ("20150830", long.as_str(), "iam"),
        ("20150830", "us-east-1", long.as_str()),
    ] {
        assert_eq!(
            CredentialScope::new(date, region, service).unwrap_err(),
            CryptoError::InvalidCredentialScope,
            "{date:?} {region:?} {service:?}"
        );
    }
    let max = "a".repeat(64);
    CredentialScope::new("20150830", &max, &max).unwrap();
    CredentialScope::new("20150830", "eu-central-1", "s3.my_store").unwrap();
    // S3-compatible servers outside AWS, e.g. the OpenStack default region.
    CredentialScope::new("20150830", "RegionOne", "s3").unwrap();
    CredentialScope::new("20151231", "US-EAST-1", "s3").unwrap();
}

#[test]
fn rejects_an_empty_secret() {
    assert_eq!(
        SigningKey::derive(b"", &scope("20150830")).unwrap_err(),
        CryptoError::InvalidKey
    );
}

#[test]
fn fips_requires_a_ten_byte_secret() {
    let scope = scope("20150830");
    for len in [1, 9, 10, 40] {
        let result = SigningKey::derive(&vec![b'0'; len], &scope);
        if cfg!(feature = "fips") && len < 10 {
            assert_eq!(result.unwrap_err(), CryptoError::HmacKeyTooShort);
        } else {
            result.unwrap();
        }
    }
    // Python stdlib hmac/hashlib chain over secret "0123456789", the shortest
    // secret a FIPS build accepts.
    assert_eq!(
        hex::encode(derive_key(b"0123456789", &scope).unwrap().as_ref()),
        "945174ff3896ff5491d440ce4f687f17e03129d9854f263e6541dce69280608c"
    );
}

#[test]
fn hex_lower_matches_the_reference_encoder() {
    let bytes: Vec<u8> = (0..=u8::MAX).collect();
    assert_eq!(hex_lower(&bytes), hex::encode(&bytes));
    assert_eq!(hex_lower(&[]), "");
}
