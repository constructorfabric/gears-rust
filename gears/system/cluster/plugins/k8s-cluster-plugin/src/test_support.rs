//! A scripted API server for unit tests: a `kube::Client` that answers each
//! request with the next `(method, status, body)` reply, so rare races can be
//! replayed deterministically.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test-only harness")]

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use http::{Method, Request, Response};
use kube::client::Body;
use serde_json::{Value, json};

/// One scripted reply: the expected method, and the status and JSON body to return.
pub type Reply = (Method, u16, Value);

/// The requests the scripted server saw, as `(method, path)`, in order.
pub type Seen = Arc<Mutex<Vec<(Method, String)>>>;

/// A `kube::Client` (namespace `ns`) answering requests from `script` in order. A
/// request that does not match the next reply, or comes after the last one, gets a
/// `500`.
pub fn scripted_client(script: Vec<Reply>) -> (kube::Client, Seen) {
    let script = Arc::new(Mutex::new(VecDeque::from(script)));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let seen_svc = Arc::clone(&seen);
    let svc = tower::service_fn(move |req: Request<Body>| {
        let script = Arc::clone(&script);
        let seen = Arc::clone(&seen_svc);
        async move {
            let method = req.method().clone();
            seen.lock()
                .unwrap()
                .push((method.clone(), req.uri().path().to_owned()));
            let (status, body) = match script.lock().unwrap().pop_front() {
                Some((want, status, body)) if want == method => (status, body),
                other => (
                    500,
                    status_body(
                        500,
                        "ScriptMismatch",
                        &format!("unscripted {method}: {other:?}"),
                    ),
                ),
            };
            Ok::<_, Infallible>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
        }
    });
    (kube::Client::new(svc, "ns"), seen)
}

/// A `metav1.Status` failure body.
pub fn status_body(code: u16, reason: &str, message: &str) -> Value {
    json!({
        "kind": "Status",
        "apiVersion": "v1",
        "metadata": {},
        "status": "Failure",
        "message": message,
        "reason": reason,
        "code": code,
    })
}

/// The `404 NotFound` the API server returns for a missing object.
pub fn object_not_found(group: &str, kind: &str, name: &str) -> Value {
    let mut body = status_body(
        404,
        "NotFound",
        &format!("{kind}.{group} \"{name}\" not found"),
    );
    body["details"] = json!({ "name": name, "group": group, "kind": kind });
    body
}
