//! Body conversion trait for the fluent request builder.

use axum::body::Body;
use http::header::HeaderValue;
use serde::Serialize;

/// Converts a value into a request body, optionally providing a Content-Type header.
pub trait IntoBody {
    fn into_body(self) -> (Body, Option<HeaderValue>);
}

/// Newtype wrapper that signals "serialize this as JSON".
pub struct Json<T>(pub T);

static APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");

impl<T: Serialize> IntoBody for Json<T> {
    fn into_body(self) -> (Body, Option<HeaderValue>) {
        let bytes = serde_json::to_vec(&self.0).expect("failed to serialize body as JSON");
        (Body::from(bytes), Some(APPLICATION_JSON.clone()))
    }
}

impl IntoBody for serde_json::Value {
    fn into_body(self) -> (Body, Option<HeaderValue>) {
        let bytes = serde_json::to_vec(&self).expect("failed to serialize JSON Value");
        (Body::from(bytes), Some(APPLICATION_JSON.clone()))
    }
}

impl IntoBody for &str {
    fn into_body(self) -> (Body, Option<HeaderValue>) {
        (Body::from(self.to_owned()), None)
    }
}

impl IntoBody for String {
    fn into_body(self) -> (Body, Option<HeaderValue>) {
        (Body::from(self), None)
    }
}

impl IntoBody for bytes::Bytes {
    fn into_body(self) -> (Body, Option<HeaderValue>) {
        (Body::from(self), None)
    }
}

impl IntoBody for Vec<u8> {
    fn into_body(self) -> (Body, Option<HeaderValue>) {
        (Body::from(self), None)
    }
}

#[cfg(test)]
#[path = "body_tests.rs"]
mod body_tests;
