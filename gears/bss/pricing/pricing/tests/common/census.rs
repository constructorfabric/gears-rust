//! Source parser used by the route, authz and precondition equality guards.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[path = "../../src/source_scan_tests.rs"]
pub mod source_scan;

pub type Routes = BTreeSet<(String, String)>;

#[derive(Debug, PartialEq, Eq)]
pub struct Registration {
    pub method: String,
    pub path: String,
    pub handler: String,
    pub declaration: String,
}

/// Inspect actual code tokens while recovering path strings from their original offsets.
/// A construct the parser cannot understand fails closed instead of disappearing.
#[must_use]
/// # Panics
/// Fails the census for unreadable sources or unsupported registration syntax.
pub fn registrations(source: &str) -> Vec<Registration> {
    let code = source_scan::blank_comments_and_literals(source);
    let mut constants = BTreeMap::new();
    for (at, _) in code.match_indices("const ") {
        let rest = &source[at + 6..];
        if rest.starts_with("fn ") {
            continue;
        }
        let (name, value) = rest.split_once(':').unwrap();
        if let Some((_, value)) = value.split_once('=') {
            let value = value.split(';').next().unwrap().trim();
            if value.starts_with('"') {
                constants.insert(name.trim(), value.trim_matches('"'));
            }
        }
    }
    code.match_indices("OperationBuilder::")
        .map(|(at, _)| {
            let start = at + "OperationBuilder::".len();
            let open = start + code[start..].find('(').unwrap();
            let method = code[start..open].trim().to_ascii_uppercase();
            assert!(
                ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
                    .contains(&method.as_str()),
                "unparsed builder: {method}"
            );
            let close = source_scan::matching_delim(&code, open, b'(', b')').unwrap();
            let arg = source[open + 1..close].trim();
            let path = if arg.starts_with('"') {
                arg.trim_matches('"')
            } else {
                constants.get(arg).expect("known route path constant")
            };
            let end = close
                + code[close..]
                    .find(".register(")
                    .expect("operation must register");
            let declaration = &code[close..end];
            let after = declaration
                .split_once(".handler(")
                .expect("operation must have a handler")
                .1;
            let handler = after.split(')').next().unwrap().trim().to_owned();
            Registration {
                method,
                path: path.to_owned(),
                handler,
                declaration: declaration.to_owned(),
            }
        })
        .collect()
}

/// Isolate function signatures and bodies; braces in prose cannot move their boundaries.
#[must_use]
/// # Panics
/// Fails the census for unreadable sources or unsupported registration syntax.
pub fn functions(source: &str) -> BTreeMap<String, String> {
    let code = source_scan::blank_comments_and_literals(source);
    let mut found = BTreeMap::new();
    for (at, _) in code.match_indices("fn ") {
        if at > 0
            && (code.as_bytes()[at - 1].is_ascii_alphanumeric() || code.as_bytes()[at - 1] == b'_')
        {
            continue;
        }
        let rest = &code[at + 3..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let Some(offset) = rest.find('{') else {
            continue;
        };
        // Trait declarations have no body of their own.
        if rest[..offset].contains(';') {
            continue;
        }
        let open = at + 3 + offset;
        let close = source_scan::matching_brace(&code, open).expect("balanced function");
        found.insert(name, code[at..=close].to_owned());
    }
    found
}

/// Every production Rust file, including nested REST modules and the runtime mount.
#[must_use]
/// # Panics
/// Fails the census for unreadable sources or unsupported registration syntax.
pub fn sources() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs")
                && !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with("_tests.rs")
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[must_use]
/// # Panics
/// Fails the census for unreadable sources or unsupported registration syntax.
pub fn source_routes() -> Routes {
    let mut found = Routes::new();
    for path in sources() {
        for route in registrations(&std::fs::read_to_string(path).unwrap()) {
            assert!(
                found.insert((route.method, route.path)),
                "duplicate source registration"
            );
        }
    }
    found
}

/// Routes whose actual handler uses a transport/authz operation.
#[must_use]
/// # Panics
/// Fails the census for unreadable sources or unsupported registration syntax.
pub fn readers(needle: &str) -> Routes {
    let mut found = Routes::new();
    for path in sources() {
        let source = std::fs::read_to_string(path).unwrap();
        let bodies = functions(&source);
        for route in registrations(&source) {
            let body = bodies
                .get(&route.handler)
                .expect("registered handler has a body in its module");
            if body.contains(needle) {
                found.insert((route.method, route.path));
            }
        }
    }
    found
}

/// Count handler syntax independent of the route roster, so an unmounted handler is visible.
#[must_use]
pub fn count_in_functions(source: &str, needle: &str) -> usize {
    functions(source)
        .values()
        .map(|s| s.matches(needle).count())
        .sum()
}

#[must_use]
/// # Panics
/// Fails the census for unreadable sources or unsupported registration syntax.
pub fn production_count(needle: &str) -> usize {
    sources()
        .iter()
        .map(|p| count_in_functions(&std::fs::read_to_string(p).unwrap(), needle))
        .sum()
}

/// Positive parser control with both literal and constant paths and all measured constructs.
pub const CONTROL: &str = r#"
// OperationBuilder::delete("/comment").handler(missing).register(router, api);
const PATH: &str = "/bss-pricing/v1/control";
fn router() {
    OperationBuilder::post(PATH).authenticated().param(if_match_param()).param(idempotency_key_param())
        .handler(create).register(router, api);
    OperationBuilder::get("/bss-pricing/v1/control").authenticated().handler(read).register(router, api);
}
async fn create(headers: HeaderMap, Query(query): Query<Input>) {
    require_authenticated(ctx)?;
    authz::access_scope(enforcer, ctx).await?;
    preconditions::if_match(&headers)?;
    preconditions::idempotency_key(&headers)?;
    (StatusCode::CREATED, body)
}
async fn read() {
    require_authenticated(ctx)?;
    authz::access_scope(enforcer, ctx).await?;
    StatusCode::OK
}
"#;
