use super::*;

fn v(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| (*x).to_owned()).collect()
}

#[test]
fn wildcard_cap_grants_full_allowlist() {
    assert_eq!(
        downscope(&v(&["quotas:read"]), "*", None).unwrap(),
        v(&["quotas:read"])
    );
    // Multi-entry allowlist, sorted.
    assert_eq!(
        downscope(&v(&["b:x", "a:y"]), "*", None).unwrap(),
        v(&["a:y", "b:x"])
    );
}

#[test]
fn intersection_when_no_wildcard() {
    assert_eq!(
        downscope(&v(&["quotas:read", "x:y"]), "quotas:read z", None).unwrap(),
        v(&["quotas:read"])
    );
}

#[test]
fn empty_intersection_is_error() {
    assert!(matches!(
        downscope(&v(&["quotas:read"]), "z", None),
        Err(DownscopeError::EmptyIntersection)
    ));
}

#[test]
fn requested_must_be_subset_of_granted() {
    assert!(downscope(&v(&["quotas:read", "a:b"]), "*", Some(&v(&["quotas:read"]))).is_ok());
    assert!(matches!(
        downscope(&v(&["quotas:read"]), "*", Some(&v(&["a:b"]))),
        Err(DownscopeError::NotSubset)
    ));
}

#[test]
fn requested_subset_narrows_result() {
    assert_eq!(
        downscope(
            &v(&["a:b", "c:d", "e:f"]),
            "*",
            Some(&v(&["e:f", "a:b", "a:b"])),
        )
        .unwrap(),
        v(&["a:b", "e:f"]),
        "result is narrowed to requested, sorted and deduped"
    );
}

#[test]
fn wildcard_never_appears_in_output() {
    // Allowlist literally containing "*" must not leak it.
    let out = downscope(&v(&["*"]), "*", None);
    // allowlist is only "*", which is filtered out → empty → error.
    assert!(matches!(out, Err(DownscopeError::EmptyIntersection)));

    // "*" alongside real scopes is filtered, real scopes survive.
    let out = downscope(&v(&["*", "quotas:read"]), "*", None).unwrap();
    assert!(!out.contains(&"*".to_owned()));
    assert_eq!(out, v(&["quotas:read"]));
}
