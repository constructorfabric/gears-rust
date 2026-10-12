#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::guard::error_position;
use super::{
    MAX_AST_DEPTH, MAX_BRACKET_NESTING, MAX_EVALUATION_DEPTH, MAX_RESULT_DEPTH,
    MAX_RULE_DEPENDENCY_DEPTH, REGO_DENYLIST, REGO_RESOURCE_DENYLIST, RegoBackend,
    catch_compile_panic, classify_failure, registered_builtins, screen_denylist, within_bound,
};
use crate::backend::{
    CompileError, CompiledDocument, CostBound, EvaluationBackend, EvaluationContext,
    EvaluationError,
};

const GENEROUS: CostBound = CostBound::new(Duration::from_secs(5));

/// Work that no test can finish: 10^10 iterations over two small ranges.
const UNBOUNDED_POLICY: &str = r"
package expensive

condition := count([1 |
    some a in numbers.range(1, 100000)
    some b in numbers.range(1, 100000)
])
";

fn compile(source: &str) -> Arc<dyn CompiledDocument> {
    RegoBackend::new()
        .compile("test.rego", source, "condition")
        .unwrap()
}

fn builtins_of(source: &str) -> BTreeSet<String> {
    compile(source).referenced_builtins().clone()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn ctx(input: serde_json::Value) -> EvaluationContext {
    let at = OffsetDateTime::parse("2026-09-23T12:00:00Z", &Rfc3339).unwrap();
    EvaluationContext::new(input, at).unwrap()
}

// ---- syntax -----------------------------------------------------------------

#[test]
fn syntax_valid_document_passes() {
    let backend = RegoBackend::new();
    backend
        .validate_syntax("package p\n\ncondition if input.action == \"create\"\n")
        .unwrap();
}

#[test]
fn syntax_invalid_document_reports_position() {
    let err = RegoBackend::new()
        .validate_syntax("package p\n\ncondition if {\n  input.x ==\n}\n")
        .unwrap_err();
    let CompileError::Syntax {
        message,
        line,
        column,
    } = err
    else {
        panic!("expected a syntax error, got {err:?}");
    };
    assert!(!message.is_empty());
    assert!(line.is_some_and(|l| (3..=5).contains(&l)), "line {line:?}");
    assert!(column.is_some(), "column missing in {message}");
}

#[test]
fn syntax_validation_never_evaluates() {
    let started = Instant::now();
    RegoBackend::new()
        .validate_syntax(UNBOUNDED_POLICY)
        .unwrap();
    // Compilation evaluates nothing either.
    compile(UNBOUNDED_POLICY);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn error_position_parses_regorus_marker() {
    let message = "\n--> my:doc.rego:12:7\n   |\n12 | x\n   |       ^\nerror: boom";
    assert_eq!(error_position(message), (Some(12), Some(7)));
    assert_eq!(error_position("no marker"), (None, None));
}

// ---- compile ------------------------------------------------------------------

#[test]
fn compile_requires_entrypoint_rule() {
    let backend = RegoBackend::new();
    let err = backend
        .compile("d.rego", "package p\n\nother := true\n", "condition")
        .unwrap_err();
    assert!(
        matches!(&err, CompileError::Unsupported { message } if message.contains("data.p.condition")),
        "{err:?}"
    );

    // A function of that name is not an entrypoint.
    let err = backend
        .compile("d.rego", "package p\n\ncondition(x) := x\n", "condition")
        .unwrap_err();
    assert!(matches!(err, CompileError::Unsupported { .. }), "{err:?}");

    let err = backend
        .compile("d.rego", "package p\n\ncondition := true\n", "not a name")
        .unwrap_err();
    assert!(matches!(err, CompileError::Unsupported { .. }), "{err:?}");
}

#[test]
fn compile_accepts_default_and_set_entrypoints() {
    let backend = RegoBackend::new();
    backend
        .compile(
            "d.rego",
            "package p\n\ndefault condition := false\n",
            "condition",
        )
        .unwrap();
    backend
        .compile(
            "d.rego",
            "package p\n\ncondition contains 1 if true\n",
            "condition",
        )
        .unwrap();
}

#[test]
fn compile_reports_semantic_errors_as_syntax() {
    let err = RegoBackend::new()
        .compile(
            "d.rego",
            "package p\n\ncondition if { x == 1 }\n",
            "condition",
        )
        .unwrap_err();
    assert!(matches!(err, CompileError::Syntax { .. }), "{err:?}");
}

// ---- introspection -----------------------------------------------------------

#[test]
fn referenced_builtins_found_in_nested_constructs() {
    let source = r#"
package p

condition if {
    every x in input.items { startswith(x, "a") }
    s := {y | some y in input.xs; y == upper("a")}
    o := {k: v | some k, v in input.m; v == abs(-1)}
    a := [z | some z in array.concat([1], [2])]
    count(s) + count(a) + count(o) >= 0
    not endswith("abc", "z")
    g(1) == "1"
    h
    sum([1, 2]) == 3
    input.a == "x" with input.a as trim(" x ", " ")
}

g(x) := concat("-", [format_int(x, 10)])

h := 1 if { false } else := lower("A")

default fallback := 1

fallback := object.get(input, "k", 0) if true
"#;
    let found = builtins_of(source);
    for name in [
        "startswith",
        "upper",
        "abs",
        "array.concat",
        "count",
        "endswith",
        "concat",
        "format_int",
        "lower",
        "sum",
        "trim",
        "object.get",
    ] {
        assert!(found.contains(name), "{name} missing from {found:?}");
    }
    // User functions are not builtins.
    assert!(!found.contains("g"));
}

#[test]
fn referenced_input_paths_are_static_prefixes() {
    let source = r#"
package p

import input.subject as who

condition if {
    input.properties.size > 1
    input["resource"]["type"] == "t"
    input.properties.tags[_] == "x"
    who.id != ""
    some v in input.list
    v == 1
}
"#;
    let paths = compile(source).referenced_input_paths().clone();
    assert_eq!(
        paths,
        set(&[
            "list",
            "properties.size",
            "properties.tags",
            "resource.type",
            "subject.id",
        ])
    );
}

// ---- denylist screen -----------------------------------------------------------

#[test]
fn denylist_screen_catches_clock_and_random_everywhere() {
    let backend = RegoBackend::new();
    let cases = [
        "condition if time.now_ns() > 0",
        "condition if { x := [t | t := time.now_ns()]; count(x) > 0 }",
        "condition if { every i in [1] { rand.intn(\"x\", 10) >= 0 } }",
        "condition := f(1)\nf(x) := rand.intn(\"x\", x)",
        "condition := 1 if { false } else := time.now_ns()",
        "condition if { input.a == 1 with input.a as rand.intn(\"x\", 10) }",
        "condition if { {k: v | k := \"a\"; v := time.now_ns()} != {} }",
        "condition if print(\"leak\")",
        // Spellings that are not the bare dotted name still resolve to it.
        "condition if time[\"now_ns\"]() > 0",
        "import data.p as time\n\ncondition if time.now_ns() > 0",
    ];
    for body in cases {
        let source = format!("package p\n\n{body}\n");
        let doc = backend.compile("d.rego", &source, "condition").unwrap();
        let offending = screen_denylist(doc.as_ref());
        assert!(
            offending
                .iter()
                .any(|b| ["time.now_ns", "rand.intn", "print"].contains(&b.as_str())),
            "screen missed a denylisted builtin in {body:?}: {offending:?}"
        );
    }

    let clean = backend
        .compile(
            "d.rego",
            "package p\n\ncondition if time.parse_rfc3339_ns(input.evaluated_at) > 0\n",
            "condition",
        )
        .unwrap();
    assert!(screen_denylist(clean.as_ref()).is_empty());
}

// ---- evaluation ---------------------------------------------------------------

#[test]
fn evaluation_returns_true_false_and_null_for_undefined() {
    let source = "package p\n\ncondition if input.action == \"create\"\n";
    let doc = compile(source);
    assert_eq!(
        doc.evaluate(&ctx(json!({"action": "create"})), GENEROUS)
            .unwrap(),
        json!(true)
    );
    assert_eq!(
        doc.evaluate(&ctx(json!({"action": "delete"})), GENEROUS)
            .unwrap(),
        serde_json::Value::Null
    );

    let doc = compile("package p\n\ndefault condition := false\n\ncondition if input.ok\n");
    assert_eq!(
        doc.evaluate(&ctx(json!({"ok": false})), GENEROUS).unwrap(),
        json!(false)
    );
}

#[test]
fn evaluation_sees_input_and_injected_timestamp_only() {
    let doc = compile("package p\n\ncondition := input\n");
    let value = doc
        .evaluate(&ctx(json!({"properties": {"n": 1}})), GENEROUS)
        .unwrap();
    assert_eq!(
        value,
        json!({
            "properties": {"n": 1},
            "evaluated_at": "2026-09-23T12:00:00Z",
            "evaluated_at_ns": 1_790_164_800_000_000_000_i64,
        })
    );

    // No data document is ever loaded: anything outside the document's own
    // package is undefined.
    let doc = compile("package p\n\ncondition := data.tenants\n");
    assert_eq!(
        doc.evaluate(&ctx(json!({})), GENEROUS).unwrap(),
        serde_json::Value::Null
    );
}

#[test]
fn evaluation_is_deterministic() {
    let doc = compile(
        "package p\n\ncondition := {\"t\": input.evaluated_at_ns, \"s\": sort([x | some x in input.xs])}\n",
    );
    let input = ctx(json!({"xs": [3, 1, 2]}));
    let first = doc.evaluate(&input, GENEROUS).unwrap();
    let second = doc.evaluate(&input, GENEROUS).unwrap();
    assert_eq!(first, second);
    assert_eq!(first["s"], json!([1, 2, 3]));
}

#[test]
fn evaluation_runtime_error_is_failed() {
    let doc = compile("package p\n\ncondition := to_number(\"not a number\")\n");
    let err = doc.evaluate(&ctx(json!({})), GENEROUS).unwrap_err();
    assert!(matches!(err, EvaluationError::Failed { .. }), "{err:?}");
}

#[test]
fn evaluation_bound_exceeded() {
    let limit = Duration::from_millis(1);
    let bound = CostBound::new(limit);

    let started = Instant::now();
    let err = compile(UNBOUNDED_POLICY)
        .evaluate(&ctx(json!({})), bound)
        .unwrap_err();
    assert_eq!(err, EvaluationError::BoundExceeded { limit });
    assert!(started.elapsed() < Duration::from_secs(5));

    // Also when the bound expires inside a user function body, where the
    // interpreter re-wraps the limit error as text.
    let in_function = "package p\n\ncondition := f(100000)\n\nf(n) := count([1 | some a in numbers.range(1, n); some b in numbers.range(1, n)])\n";
    let err = compile(in_function)
        .evaluate(&ctx(json!({})), bound)
        .unwrap_err();
    assert_eq!(err, EvaluationError::BoundExceeded { limit });
}

#[test]
fn bound_exceedance_is_decided_by_measured_elapsed_time_not_error_text() {
    let limit = Duration::from_millis(10);
    let err = anyhow::anyhow!("execution exceeded time limit (elapsed=1ns, limit=1ns)");
    // Error text that merely looks like a time-limit error does not make it
    // one while the measured elapsed time stays under the limit...
    assert!(matches!(
        classify_failure(&err, limit, Duration::ZERO),
        EvaluationError::Failed { .. }
    ));
    // ...but once the bound really elapsed, the same text is classified as
    // one (R-015: the elapsed measurement decides, not the message shape).
    assert_eq!(
        classify_failure(&err, limit, limit),
        EvaluationError::BoundExceeded { limit }
    );
    let typed = anyhow::Error::from(regorus::LimitError::TimeLimitExceeded {
        elapsed: limit,
        limit,
    });
    assert_eq!(
        classify_failure(&typed, limit, Duration::ZERO),
        EvaluationError::BoundExceeded { limit }
    );
}

#[test]
fn evaluation_is_thread_safe() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<regorus::Engine>();
    assert_send_sync::<RegoBackend>();

    let doc = compile("package p\n\ncondition := input.n * 2\n");
    let handles: Vec<_> = (0..8_i64)
        .map(|thread| {
            let doc = Arc::clone(&doc);
            std::thread::spawn(move || {
                for i in 0..50_i64 {
                    let n = thread * 1000 + i;
                    let value = doc.evaluate(&ctx(json!({"n": n})), GENEROUS).unwrap();
                    assert_eq!(value, json!(n * 2));
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}

// ---- denylist audit --------------------------------------------------------------

/// Registered builtins that look like clock, random, identifier, I/O or
/// runtime accessors but were audited as pure functions of their arguments.
const AUDITED_PURE_LOOKALIKES: &[&str] = &[
    "time.add_date",
    "time.clock",
    "time.date",
    "time.diff",
    "time.format",
    "time.parse_duration_ns",
    "time.parse_ns",
    "time.parse_rfc3339_ns",
    "time.weekday",
];

fn looks_nondeterministic_or_capability(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "time.",
        "rand.",
        "uuid.",
        "http.",
        "net.lookup",
        "opa.",
        "io.",
        "os.",
        "env.",
        "test.",
        "crypto.",
        "trace",
        "print",
        "file.",
        "sleep",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

#[test]
fn audit_every_suspicious_builtin_is_denylisted() {
    for name in registered_builtins() {
        if looks_nondeterministic_or_capability(&name) {
            assert!(
                REGO_DENYLIST.contains(&name.as_str())
                    || AUDITED_PURE_LOOKALIKES.contains(&name.as_str()),
                "registered builtin {name} is neither denylisted nor audited as pure"
            );
        }
    }
    for name in ["time.now_ns", "rand.intn", "print", "trace"] {
        assert!(
            registered_builtins().contains(name),
            "{name} not registered"
        );
        assert!(REGO_DENYLIST.contains(&name), "{name} not denylisted");
    }
}

// ---- with modifiers (F-001) ----------------------------------------------------

/// The review probe: a function-target replacement whose evaluation the
/// interpreter runs with the time-limit error discarded.
const WITH_FUNCTION_PROBE: &str = r"package p

slow := count([1 | some a in numbers.range(1, 300); some b in numbers.range(1, 300)])

condition if {
    some i in numbers.range(1, 3000)
    count([]) == 0 with count as data.p.slow
}

condition := false
";

fn assert_unsupported(result: Result<Arc<dyn CompiledDocument>, CompileError>, needle: &str) {
    match result {
        Err(CompileError::Unsupported { message }) => {
            assert!(message.contains(needle), "{needle:?} not in {message:?}");
        }
        other => panic!("expected an unsupported-document error, got {other:?}"),
    }
}

#[test]
fn with_function_target_is_refused_at_validation_and_compile() {
    let backend = RegoBackend::new();
    assert_unsupported(
        backend.compile("d.rego", WITH_FUNCTION_PROBE, "condition"),
        "`with` target `count` is a function or builtin",
    );
    assert!(matches!(
        backend.validate_syntax(WITH_FUNCTION_PROBE),
        Err(CompileError::Unsupported { .. })
    ));

    let cases = [
        // builtins, including by computed spelling and denylisted replacements
        "condition if { count([1]) > 0 with count as time[\"now_ns\"] }",
        "condition := y if { y := numbers.range(1, 2) with numbers.range as rand.intn }",
        "condition := y if { y := count([1]) with count as trace }",
        "condition if { count([1]) == 1 with count as time.now_ns() }",
        // user functions: bare, package-qualified, through an import alias
        "condition := g(1)\ng(x) := y if { y := h(x) with h as time.now_ns }\nh(x) := x",
        "condition if { data.p.h(1) == 2 with data.p.h as 2 }\nh(x) := x",
        "import data.p.h\n\ncondition if { h(1) == 2 with h as 2 }\nh(x) := x",
        "condition if { h(1) == 2 with h as 2 }\ndefault h(_) := 1",
    ];
    for body in cases {
        let source = format!("package p\n\n{body}\n");
        assert_unsupported(
            backend.compile("d.rego", &source, "condition"),
            "is a function or builtin",
        );
    }

    // A target that is neither `input` nor `data`.
    assert_unsupported(
        backend.compile(
            "d.rego",
            "package p\n\ncondition if { true with undefined_name as 1 }\n",
            "condition",
        ),
        "is not `input`, `input.*` or a `data.*` document",
    );
    // A dynamic target (whose index would escape the screen) never compiles.
    assert!(
        backend
            .compile(
                "d.rego",
                "package p\n\ncondition if { input.a == 1 with input[time.now_ns()] as 1 }\n",
                "condition",
            )
            .is_err()
    );
}

#[test]
fn with_input_and_data_document_targets_are_supported() {
    let doc = compile("package p\n\ncondition if { input.x == 2 with input.x as 2 }\n");
    assert_eq!(
        doc.evaluate(&ctx(json!({"x": 1})), GENEROUS).unwrap(),
        json!(true)
    );
    let doc = compile(
        "package p\n\nimport input.subject as who\n\ncondition if { who.id == \"a\" with input as {\"subject\": {\"id\": \"a\"}} }\n",
    );
    assert_eq!(
        doc.evaluate(&ctx(json!({})), GENEROUS).unwrap(),
        json!(true)
    );
    compile("package p\n\nv := 1\n\ncondition if { data.p.v == 2 with data.p.v as 2 }\n");
}

#[test]
fn a_value_returned_after_the_limit_is_a_bound_exceedance() {
    let limit = Duration::from_millis(10);
    let long_ago = Instant::now()
        .checked_sub(Duration::from_millis(50))
        .unwrap();
    assert_eq!(
        within_bound(long_ago, limit),
        Err(EvaluationError::BoundExceeded { limit })
    );
    assert_eq!(within_bound(Instant::now(), Duration::from_secs(5)), Ok(()));
}

// ---- nesting depth (F-002) -------------------------------------------------------

/// Runs `f` on a thread with a 2 MiB stack — the default of spawned threads
/// and of async runtimes' workers — so a stack overflow aborts the test.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

/// `condition := <head><link>...<link><tail>` with `n` links. With `wrap`,
/// a line break precedes every 200th link so that no line exceeds the
/// lexer's column limit (only valid where the grammar allows a break before
/// the link).
fn chain(head: &str, link: &str, n: usize, tail: &str, wrap: bool) -> String {
    let mut source = format!("package p\n\ncondition := {head}");
    for i in 0..n {
        if wrap && i % 200 == 0 {
            source.push('\n');
        }
        source.push_str(link);
    }
    source.push_str(tail);
    source.push('\n');
    source
}

#[test]
fn deep_documents_are_refused_instead_of_overflowing_the_stack() {
    let mut shapes = vec![
        // the review probe: a left-associative arithmetic chain
        chain("[0", "+1", 30_000, "]", true),
        chain("0", " + 1", 30_000, "", true),
        chain("0", " -1", 30_000, "", true),
        chain("0", "\n+ 1", 15_000, "", false),
        chain("0", " == 1", 30_000, "", true),
        chain("", "- ", 30_000, "1", true),
        chain("", "f(set(), ", 5_000, "", true),
        // reference chains cannot break lines between links, except inside
        // brackets
        chain("input", ".a", 500, "", false),
        chain("input", ".if", 330, "", false),
        chain("input", "[0]", 330, "", false),
        chain("input", "[\n0]", 15_000, "", false),
    ];
    let mut every = String::from("package p\n\ncondition if {\n");
    every.push_str(&"every a in [1] {\n".repeat(5_000));
    every.push_str("true\n");
    every.push_str(&"}\n".repeat(5_001));
    shapes.push(every);

    for source in shapes {
        let head: String = source.chars().take(60).collect();
        let (validated, compiled) = on_small_stack(move || {
            let backend = RegoBackend::new();
            (
                backend
                    .validate_syntax(&source)
                    .map_err(|err| format!("{err}")),
                backend
                    .compile("d.rego", &source, "condition")
                    .map(drop)
                    .map_err(|err| format!("{err}")),
            )
        });
        for outcome in [validated, compiled] {
            // Whichever guard fires first — the chain-nesting bound or the
            // bracket-nesting cap (F-006) — both refuse with "maximum
            // supported" in the message.
            assert!(
                outcome
                    .as_ref()
                    .is_err_and(|message| message.contains("maximum supported")),
                "{head:?}: {outcome:?}"
            );
        }
    }

    let err = on_small_stack(|| {
        RegoBackend::new()
            .compile("d.rego", &chain("[0", "+1", 30_000, "]", true), "condition")
            .map(drop)
    })
    .unwrap_err();
    assert!(
        matches!(&err, CompileError::Unsupported { message } if message.contains(&MAX_AST_DEPTH.to_string())),
        "{err:?}"
    );
}

#[test]
fn a_hundred_deep_expression_still_compiles_and_evaluates() {
    let source = chain("0", " + 1", 100, "", false);
    let value = on_small_stack(move || {
        let backend = RegoBackend::new();
        backend.validate_syntax(&source).unwrap();
        backend
            .compile("d.rego", &source, "condition")
            .unwrap()
            .evaluate(&ctx(json!({})), GENEROUS)
            .unwrap()
    });
    assert_eq!(value, json!(100));

    let refs = chain("input", ".a", 100, "", false);
    on_small_stack(move || {
        RegoBackend::new()
            .compile("d.rego", &refs, "condition")
            .map(drop)
    })
    .unwrap();
}

/// `n` concentric literals of one bracket kind around a scalar, as
/// `condition := ` followed by the nesting.
fn nested_brackets(open: &str, close: &str, n: usize) -> String {
    let mut source = String::from("package p\n\ncondition := ");
    source.push_str(&open.repeat(n));
    source.push('1');
    source.push_str(&close.repeat(n));
    source.push('\n');
    source
}

fn nested_object(n: usize) -> String {
    let mut source = String::from("package p\n\ncondition := ");
    source.push_str(&"{\"a\": ".repeat(n));
    source.push('1');
    source.push_str(&"}".repeat(n));
    source.push('\n');
    source
}

/// Documents nesting `[`, `{` or `(` literals past [`MAX_BRACKET_NESTING`]
/// are refused without ever giving `regorus`' backtracking parser a chance to
/// run: the check is a single lexer pass, so refusal is near-instant even
/// though, unrefused, the same shapes take seconds to parse (see
/// [`MAX_BRACKET_NESTING`]'s doc comment for the measurements).
#[test]
fn deeply_bracket_nested_documents_are_refused_quickly() {
    let depth = MAX_BRACKET_NESTING + 14; // ~24 levels, an exponential shape that would otherwise never finish.
    let shapes = [
        nested_brackets("[", "]", depth),
        nested_object(depth),
        nested_brackets("(", ")", depth),
    ];
    let backend = RegoBackend::new();
    for source in shapes {
        let head: String = source.chars().take(40).collect();

        let started = Instant::now();
        let validated = backend.validate_syntax(&source);
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "{head:?}: validate_syntax took {:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                &validated,
                Err(CompileError::Unsupported { message })
                    if message.contains("bracket nesting")
                        && message.contains(&MAX_BRACKET_NESTING.to_string())
            ),
            "{head:?}: {validated:?}"
        );

        let started = Instant::now();
        let compiled = backend.compile("d.rego", &source, "condition").map(drop);
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "{head:?}: compile took {:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                &compiled,
                Err(CompileError::Unsupported { message })
                    if message.contains("bracket nesting")
                        && message.contains(&MAX_BRACKET_NESTING.to_string())
            ),
            "{head:?}: {compiled:?}"
        );
    }
}

#[test]
fn a_document_at_the_bracket_nesting_cap_still_compiles() {
    let backend = RegoBackend::new();
    for source in [
        nested_brackets("[", "]", MAX_BRACKET_NESTING),
        nested_object(MAX_BRACKET_NESTING),
        nested_brackets("(", ")", MAX_BRACKET_NESTING),
    ] {
        backend.validate_syntax(&source).unwrap();
        backend
            .compile("d.rego", &source, "condition")
            .unwrap_or_else(|err| panic!("document at the cap was refused: {err}"));
    }
}

#[test]
fn long_flat_documents_are_not_mistaken_for_deep_ones() {
    let mut source = String::from("package p\n\ncondition if {\n");
    for i in 0..400 {
        writeln!(source, "    input.a.b.c != \"{i}\"").unwrap();
    }
    source.push_str("}\n");
    for i in 0..400 {
        writeln!(source, "r{i} := {{\"k\": [1, 2, -3], \"v\": input.x.y}}").unwrap();
    }
    RegoBackend::new().validate_syntax(&source).unwrap();
    compile(&source);
}

// ---- rule dependencies -----------------------------------------------------------

/// `condition` heading a chain of `links` rules and functions in all (itself
/// included), each referencing the next through one of several spellings,
/// the last `true`.
fn rule_chain(links: usize) -> String {
    let mut source = String::from("package p\n\nimport data.p.r1 as first\n\ncondition := first\n");
    for i in 1..links {
        let next = i + 1;
        let body = if next == links {
            "true".to_owned()
        } else {
            match i % 4 {
                0 => format!("r{next}"),
                1 => format!("data.p.r{next}"),
                2 => format!("data.p[\"r{next}\"]"),
                _ => format!("f{next}(1)"),
            }
        };
        if i % 4 == 0 && i > 1 {
            // Reached through `f<i>(1)` from the previous link.
            writeln!(source, "f{i}(x) := {body} if x == 1").unwrap();
        } else {
            writeln!(source, "r{i} := {body}").unwrap();
        }
    }
    source
}

#[test]
fn dependency_cycles_are_refused_through_every_kind_of_reference() {
    let backend = RegoBackend::new();
    let cycles = [
        // a rule referencing itself
        "condition := r\nr if { r }",
        // two rules
        "condition := a\na := b\nb := a",
        // functions, which the interpreter does not detect recursion through
        "condition := f(1)\nf(x) := g(x)\ng(x) := f(x)",
        // a `data.` path
        "condition := data.p.a\na := data.p.b\nb := data.p.a",
        // bracket access
        "condition := a\na := data.p[\"b\"]\nb := data.p[\"a\"]",
        // an import alias
        "import data.p.a as alias\n\ncondition := a\na := alias",
        // a parent path: `data.p` contains `a` itself
        "condition := a\na := count(data.p)",
    ];
    for body in cycles {
        let source = format!("package p\n\n{body}\n");
        let compiled = on_small_stack({
            let source = source.clone();
            move || RegoBackend::new().compile("d.rego", &source, "condition")
        });
        assert_unsupported(compiled, "depends on itself");
        assert!(
            matches!(
                backend.validate_syntax(&source),
                Err(CompileError::Unsupported { .. })
            ),
            "{body}"
        );
    }
}

#[test]
fn a_dependency_chain_at_the_cap_compiles_and_evaluates_one_past_it_is_refused() {
    let at_cap = rule_chain(MAX_RULE_DEPENDENCY_DEPTH);
    let value = on_small_stack(move || {
        RegoBackend::new()
            .compile("d.rego", &at_cap, "condition")
            .unwrap()
            .evaluate(&ctx(json!({})), GENEROUS)
            .unwrap()
    });
    assert_eq!(value, json!(true));

    let past_cap = rule_chain(MAX_RULE_DEPENDENCY_DEPTH + 1);
    let compiled =
        on_small_stack(move || RegoBackend::new().compile("d.rego", &past_cap, "condition"));
    assert_unsupported(
        compiled,
        &format!("the maximum supported rule dependency depth is {MAX_RULE_DEPENDENCY_DEPTH}"),
    );
}

#[test]
fn a_short_chain_of_deep_rules_past_the_evaluation_depth_is_refused() {
    // Each link adds a reference at the bottom of a 60-level expression:
    // ten links stay under the link cap but not under the evaluation depth.
    let links = 10;
    let mut source = String::from("package p\n\ncondition := r1\n");
    for i in 1..links {
        let tail = if i + 1 == links {
            "1".to_owned()
        } else {
            format!("r{}", i + 1)
        };
        writeln!(source, "r{i} := {tail}{}", " + 1".repeat(60)).unwrap();
    }
    assert!(links < MAX_RULE_DEPENDENCY_DEPTH);
    let compiled =
        on_small_stack(move || RegoBackend::new().compile("d.rego", &source, "condition"));
    assert_unsupported(
        compiled,
        &format!("the maximum supported evaluation depth is {MAX_EVALUATION_DEPTH}"),
    );
}

// ---- resource safety (F-003) -------------------------------------------------------

#[test]
fn resource_denylist_names_registered_builtins_and_is_screened() {
    let registered = registered_builtins();
    for name in REGO_RESOURCE_DENYLIST {
        assert!(registered.contains(*name), "{name} not registered");
        assert!(!REGO_DENYLIST.contains(name), "{name} is in both denylists");
    }
    let doc = compile(
        "package p\n\ncondition if { count(numbers.range(1, 3)) == 3; sprintf(\"%d\", [1]) == \"1\"; bits.lsh(1, 2) == 4 }\n",
    );
    assert_eq!(
        screen_denylist(doc.as_ref()),
        set(&["bits.lsh", "numbers.range", "sprintf"])
    );
}

#[test]
fn a_panic_inside_the_backend_is_a_failure() {
    // `numbers.range` pre-allocates `2^63` elements: a capacity-overflow
    // panic (compile does not screen; activation would refuse it).
    let doc = compile("package p\n\ncondition := count(numbers.range(0, 9223372036854775806))\n");
    let err = doc.evaluate(&ctx(json!({})), GENEROUS).unwrap_err();
    assert!(
        matches!(&err, EvaluationError::Failed { message } if message.contains("panicked")),
        "{err:?}"
    );
}

#[test]
fn a_panic_during_validate_syntax_or_compile_is_unsupported_not_a_crash() {
    // `catch_compile_panic` is what protects `validate_syntax` and `compile`
    // (R-014); no known input panics the parser or analyser today, so this
    // exercises the mapping directly, the way `evaluate`'s panic path is
    // exercised through a real panicking input above.
    let result: Result<(), CompileError> = catch_compile_panic(std::panic::AssertUnwindSafe(
        || -> Result<(), CompileError> { panic!("backend exploded") },
    ));
    assert!(
        matches!(&result, Err(CompileError::Unsupported { message }) if message == "backend panicked"),
        "{result:?}"
    );

    // No unwind escapes `validate_syntax` or `compile` themselves either.
    let backend = RegoBackend::new();
    assert!(
        backend
            .validate_syntax("package p\n\ncondition := 1\n")
            .is_ok()
    );
    assert!(
        backend
            .compile("d.rego", "package p\n\ncondition := 1\n", "condition")
            .is_ok()
    );
}

// ---- failure classification (F-004) -------------------------------------------------

#[test]
fn time_limit_text_in_document_source_is_not_a_bound_exceedance() {
    // The review probe: the error chain quotes the source line, whose
    // comment carries the marker; a generous bound leaves no doubt that it
    // never actually elapsed, so any BoundExceeded here could only come from
    // the quoted text.
    let source = "package p\n\ncondition := to_number(\"nope\") # execution exceeded time limit (elapsed=1ns, limit=1ns)\n";
    let err = compile(source)
        .evaluate(&ctx(json!({})), GENEROUS)
        .unwrap_err();
    assert!(matches!(err, EvaluationError::Failed { .. }), "{err:?}");

    // Quoted marker text is irrelevant either way (R-015): what decides is
    // the measured elapsed time, and here it is strictly under the limit.
    let quoted = anyhow::anyhow!(
        "\n--> d.rego:5:20\n  |\n5 | x := 1 # execution exceeded time limit (elapsed=1ns, limit=1ns)\n  |\nerror: boom"
    );
    let limit = Duration::from_millis(10);
    assert!(matches!(
        classify_failure(
            &quoted,
            limit,
            limit.saturating_sub(Duration::from_millis(1))
        ),
        EvaluationError::Failed { .. }
    ));
    // Once the bound really elapsed, the very same message is a bound
    // exceedance — message shape never mattered, only the measurement.
    assert_eq!(
        classify_failure(&quoted, limit, limit),
        EvaluationError::BoundExceeded { limit }
    );
}

// ---- result representability (F-005) -----------------------------------------------

#[test]
fn results_json_cannot_represent_are_failures() {
    let unrepresentable = EvaluationError::Failed {
        message: "unrepresentable result".to_owned(),
    };
    for value in [
        "{1: \"a\", \"1\": \"b\"}",
        "[{\"k\": {[1]: true}}]",
        "100000000000000000000000000000",
        "{\"n\": -100000000000000000000000000000}",
    ] {
        let doc = compile(&format!("package p\n\ncondition := {value}\n"));
        assert_eq!(
            doc.evaluate(&ctx(json!({})), GENEROUS).unwrap_err(),
            unrepresentable,
            "{value}"
        );
    }

    let doc = compile(
        "package p\n\ncondition := {\"f\": 1.5, \"u\": 18446744073709551615, \"i\": -9223372036854775808, \"s\": {3, 1, 2}, \"n\": null}\n",
    );
    assert_eq!(
        doc.evaluate(&ctx(json!({})), GENEROUS).unwrap(),
        json!({
            "f": 1.5,
            "u": u64::MAX,
            "i": i64::MIN,
            "s": [1, 2, 3],
            "n": null,
        })
    );
}

#[test]
fn deep_results_convert_without_recursion() {
    // A 120-deep value taken from the input, converted iteratively.
    let mut nested = json!(0);
    for _ in 0..120 {
        nested = json!([nested]);
    }
    let input = ctx(json!({ "v": nested }));
    let value = on_small_stack(move || {
        compile("package p\n\ncondition := input.v\n")
            .evaluate(&input, GENEROUS)
            .unwrap()
    });
    let mut depth = 0;
    let mut current = &value;
    while let Some(inner) = current.as_array().and_then(|a| a.first()) {
        depth += 1;
        current = inner;
    }
    assert_eq!(depth, 120);
}

/// `n` arrays nested around a scalar `0`, taken from the input.
fn nested_input_value(n: usize) -> serde_json::Value {
    let mut nested = json!(0);
    for _ in 0..n {
        nested = json!([nested]);
    }
    nested
}

#[test]
fn a_result_exactly_at_the_max_depth_still_converts() {
    let input = ctx(json!({ "v": nested_input_value(MAX_RESULT_DEPTH) }));
    let value = on_small_stack(move || {
        compile("package p\n\ncondition := input.v\n")
            .evaluate(&input, GENEROUS)
            .unwrap()
    });
    let mut depth = 0;
    let mut current = &value;
    while let Some(inner) = current.as_array().and_then(|a| a.first()) {
        depth += 1;
        current = inner;
    }
    assert_eq!(depth, MAX_RESULT_DEPTH);
}

#[test]
fn results_nested_past_the_max_depth_are_unrepresentable() {
    let input = ctx(json!({ "v": nested_input_value(MAX_RESULT_DEPTH + 1) }));
    let err = on_small_stack(move || {
        compile("package p\n\ncondition := input.v\n")
            .evaluate(&input, GENEROUS)
            .unwrap_err()
    });
    assert_eq!(
        err,
        EvaluationError::Failed {
            message: "unrepresentable result".to_owned(),
        }
    );
}
