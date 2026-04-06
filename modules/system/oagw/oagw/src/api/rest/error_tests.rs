use super::*;

#[test]
fn validation_error_produces_correct_problem() {
    let err = DomainError::Validation {
        detail: "missing required field 'server'".into(),
        instance: "/oagw/v1/upstreams".into(),
    };
    let p: Problem = err.into();
    assert_eq!(p.status, StatusCode::BAD_REQUEST);
    assert_eq!(p.type_url, ERR_VALIDATION);
    assert_eq!(p.title, "Validation Error");
    assert!(p.detail.contains("missing required field"));
    assert_eq!(p.instance, "/oagw/v1/upstreams");
}

#[test]
fn conflict_error_produces_409() {
    let err = DomainError::Conflict {
        detail: "alias already exists".into(),
    };
    let p: Problem = err.into();
    assert_eq!(p.status, StatusCode::CONFLICT);
    assert_eq!(p.type_url, ERR_CONFLICT);
    assert_eq!(p.title, "Conflict");
}

#[test]
fn rate_limit_exceeded_produces_429() {
    let err = DomainError::RateLimitExceeded {
        detail: "rate limit exceeded for upstream".into(),
        instance: "/oagw/v1/proxy/api.openai.com/v1/chat/completions".into(),
        retry_after_secs: Some(30),
    };
    let p: Problem = err.into();
    assert_eq!(p.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(p.type_url, ERR_RATE_LIMIT_EXCEEDED);
}

#[test]
fn not_found_produces_404() {
    let err = DomainError::NotFound {
        entity: "route",
        id: uuid::Uuid::nil(),
    };
    let p: Problem = err.into();
    assert_eq!(p.status, StatusCode::NOT_FOUND);
    assert_eq!(p.type_url, ERR_ROUTE_NOT_FOUND);
}

#[test]
fn all_error_types_produce_valid_json() {
    let errors: Vec<DomainError> = vec![
        DomainError::Validation {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::Conflict {
            detail: "test".into(),
        },
        DomainError::MissingTargetHost {
            instance: "/test".into(),
        },
        DomainError::InvalidTargetHost {
            instance: "/test".into(),
        },
        DomainError::UnknownTargetHost {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::AuthenticationFailed {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::NotFound {
            entity: "route",
            id: uuid::Uuid::nil(),
        },
        DomainError::PayloadTooLarge {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::RateLimitExceeded {
            detail: "test".into(),
            instance: "/test".into(),
            retry_after_secs: None,
        },
        DomainError::SecretNotFound {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::DownstreamError {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::ProtocolError {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::UpstreamDisabled {
            alias: "test".into(),
        },
        DomainError::ConnectionTimeout {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::RequestTimeout {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::Internal {
            message: "test".into(),
        },
        DomainError::GuardRejected {
            status: 400,
            error_code: "MISSING_HEADER".into(),
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::CorsOriginNotAllowed {
            origin: "https://evil.com".into(),
            instance: "/test".into(),
        },
        DomainError::CorsMethodNotAllowed {
            method: "DELETE".into(),
            instance: "/test".into(),
        },
        DomainError::StreamAborted {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::LinkUnavailable {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::CircuitBreakerOpen {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::IdleTimeout {
            detail: "test".into(),
            instance: "/test".into(),
        },
        DomainError::PluginNotFound {
            detail: "test".into(),
        },
        DomainError::PluginInUse {
            detail: "test".into(),
        },
        DomainError::Forbidden {
            detail: "test".into(),
        },
    ];
    for err in errors {
        let p: Problem = err.into();
        let json = serde_json::to_string(&p).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("type").is_some());
        assert!(parsed.get("status").is_some());
        assert!(parsed.get("title").is_some());
        assert!(parsed.get("detail").is_some());
    }
}

#[test]
fn domain_error_to_problem_fills_missing_instance() {
    let err = DomainError::NotFound {
        entity: "upstream",
        id: uuid::Uuid::nil(),
    };
    let p = domain_error_to_problem(err, "/oagw/v1/upstreams/123");
    assert_eq!(p.instance, "/oagw/v1/upstreams/123");
}

#[test]
fn domain_error_to_problem_preserves_existing_instance() {
    let err = DomainError::Validation {
        detail: "bad input".into(),
        instance: "/oagw/v1/upstreams".into(),
    };
    let p = domain_error_to_problem(err, "/fallback");
    assert_eq!(p.instance, "/oagw/v1/upstreams");
}

#[test]
fn guard_rejected_4xx_passes_through() {
    let err = DomainError::GuardRejected {
        status: 403,
        error_code: "FORBIDDEN".into(),
        detail: "test".into(),
        instance: "/test".into(),
    };
    let p: Problem = err.into();
    assert_eq!(p.status, StatusCode::FORBIDDEN);
}

#[test]
fn guard_rejected_5xx_passes_through() {
    let err = DomainError::GuardRejected {
        status: 503,
        error_code: "UNAVAILABLE".into(),
        detail: "test".into(),
        instance: "/test".into(),
    };
    let p: Problem = err.into();
    assert_eq!(p.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn guard_rejected_2xx_falls_back_to_400() {
    let err = DomainError::GuardRejected {
        status: 200,
        error_code: "OK".into(),
        detail: "test".into(),
        instance: "/test".into(),
    };
    let p: Problem = err.into();
    assert_eq!(p.status, StatusCode::BAD_REQUEST);
}

#[test]
fn guard_rejected_3xx_falls_back_to_400() {
    let err = DomainError::GuardRejected {
        status: 301,
        error_code: "REDIRECT".into(),
        detail: "test".into(),
        instance: "/test".into(),
    };
    let p: Problem = err.into();
    assert_eq!(p.status, StatusCode::BAD_REQUEST);
}

#[test]
fn guard_rejected_invalid_status_falls_back_to_400() {
    let err = DomainError::GuardRejected {
        status: 999,
        error_code: "INVALID".into(),
        detail: "test".into(),
        instance: "/test".into(),
    };
    let p: Problem = err.into();
    assert_eq!(p.status, StatusCode::BAD_REQUEST);
}

#[test]
fn error_response_sets_gateway_header() {
    let err = DomainError::NotFound {
        entity: "route",
        id: uuid::Uuid::nil(),
    };
    let resp = error_response(err);
    assert_eq!(
        resp.headers().get("x-oagw-error-source").unwrap(),
        "gateway"
    );
}
