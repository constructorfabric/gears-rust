//! Rego backend over `regorus`.
//!
//! Each document is compiled into its own sandboxed engine: no data document,
//! no extensions, `print` captured, strict builtin errors. Documents that
//! nest too deeply, use `with` on functions or builtins, or fail the rule
//! dependency guard are refused at compile time. [`REGO_DENYLIST`] (clock,
//! randomness, I/O, runtime) and [`REGO_RESOURCE_DENYLIST`] (builtins whose
//! single call allocates in proportion to an argument's value) are screened
//! with [`screen_denylist`].
//!
//! Evaluation runs under a wall-clock [`CostBound`]. The bound is enforced
//! cooperatively by `regorus` (checked every [`CHECK_INTERVAL_UNITS`] units
//! of work) and re-checked against the facility's own clock after the
//! evaluation and during result conversion. Memory is not limited beyond the
//! content limits the caller imposes and the resource denylist. A panic
//! inside `regorus` is caught and reported as a failure.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU32;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::time::{Duration, Instant};

use regorus::unstable::{BUILTINS, Expr, Module, Parser, Query, Ref, Rule, RuleHead, WithModifier};
use regorus::utils::get_path_string;
use regorus::utils::limits::ExecutionTimerConfig;
use serde::Deserialize;
use toolkit_gts::gts_id;

use crate::backend::{
    CompileError, CompiledDocument, CostBound, EvaluationBackend, EvaluationContext,
    EvaluationError,
};

#[path = "rego_guard.rs"]
mod guard;

pub use guard::{
    MAX_AST_DEPTH, MAX_BRACKET_NESTING, MAX_EVALUATION_DEPTH, MAX_RULE_DEPENDENCY_DEPTH,
};

/// Id of the Rego backend carried by this crate.
pub const REGO_BACKEND_ID: &str =
    gts_id!("cf.core.policy_evaluation.backend.v1~cf.core.policy_evaluation.rego_regorus.v1");

/// Builtins refused in Rego documents for determinism and isolation: clock
/// readers, random value and identifier generators, I/O and runtime
/// introspection. Includes names this build does not register so that a
/// feature change cannot admit them.
pub const REGO_DENYLIST: &[&str] = &[
    "http.send",
    "opa.runtime",
    "print",
    "rand.intn",
    "test.sleep",
    "time.now_ns",
    "trace",
    "uuid.rfc4122",
];

/// Builtins refused in Rego documents for resource safety: a single call's
/// work or allocation is driven by the value of an argument rather than by
/// the size of its arguments, so it escapes the cooperative bound and can
/// abort the process on allocation failure.
pub const REGO_RESOURCE_DENYLIST: &[&str] = &[
    "bits.lsh",
    "net.cidr_expand",
    "numbers.range",
    "numbers.range_step",
    "sprintf",
    "units.parse",
    "units.parse_bytes",
];

/// Builtins the interpreter dispatches itself rather than through the
/// registered builtin map.
const INTERPRETER_INTRINSICS: &[&str] = &["print"];

/// Units of interpreter work between two checks of the bound's clock.
const CHECK_INTERVAL_UNITS: NonZeroU32 = NonZeroU32::MIN.saturating_add(31);

/// Nodes converted to JSON between two checks of the bound's clock.
const CONVERSION_CHECK_INTERVAL: u32 = 1024;

/// Maximum nesting depth [`to_json`] represents, counted in array, set and
/// object levels below the entrypoint's own value (which is depth 0). A
/// result nested deeper is refused as [`EvaluationError::Failed`] with
/// [`UNREPRESENTABLE_RESULT`], the same as any other value JSON cannot
/// represent faithfully — `to_json` is iterative and cannot overflow the
/// stack on such a value, but nothing downstream of this facility is assumed
/// to walk an unbounded result recursively either.
const MAX_RESULT_DEPTH: usize = 1024;

/// Message of the failure reported for a value JSON cannot represent.
const UNREPRESENTABLE_RESULT: &str = "unrepresentable result";

/// The Rego backend.
#[derive(Debug, Clone, Default)]
pub struct RegoBackend;

impl RegoBackend {
    /// The Rego backend.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Every builtin the running `regorus` build registers.
fn registered_builtins() -> BTreeSet<String> {
    BUILTINS
        .keys()
        .copied()
        .chain(INTERPRETER_INTRINSICS.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// Returns the builtins `doc` references that either denylist
/// ([`REGO_DENYLIST`], [`REGO_RESOURCE_DENYLIST`]) names. An empty set means
/// the document passes both screens.
///
/// Works on the parsed-form introspection
/// ([`CompiledDocument::referenced_builtins`]), never on source text.
#[must_use]
pub fn screen_denylist(doc: &dyn CompiledDocument) -> BTreeSet<String> {
    doc.referenced_builtins()
        .iter()
        .filter(|builtin| {
            REGO_DENYLIST.contains(&builtin.as_str())
                || REGO_RESOURCE_DENYLIST.contains(&builtin.as_str())
        })
        .cloned()
        .collect()
}

impl EvaluationBackend for RegoBackend {
    fn validate_syntax(&self, source: &str) -> Result<(), CompileError> {
        catch_compile_panic(AssertUnwindSafe(|| {
            let mut engine = sandboxed_engine();
            add_guarded_policy(&mut engine, "document.rego", source)?;
            scan_modules(&mut engine, &registered_builtins()).map(drop)
        }))
    }

    fn compile(
        &self,
        document_name: &str,
        source: &str,
        entrypoint: &str,
    ) -> Result<Arc<dyn CompiledDocument>, CompileError> {
        catch_compile_panic(AssertUnwindSafe(|| {
            if !is_rule_name(entrypoint) {
                return Err(CompileError::Unsupported {
                    message: format!("entrypoint `{entrypoint}` is not a Rego rule name"),
                });
            }

            let mut engine = sandboxed_engine();
            let package = add_guarded_policy(&mut engine, document_name, source)?;
            let rule_path = format!("{package}.{entrypoint}");

            let registered = registered_builtins();
            let scan = scan_modules(&mut engine, &registered)?;
            if !engine
                .get_modules()
                .iter()
                .any(|module| defines_rule(module, entrypoint))
            {
                return Err(CompileError::Unsupported {
                    message: format!("document does not define the entrypoint rule `{rule_path}`"),
                });
            }

            // Runs the analyzer and prepares the engine; evaluates nothing.
            engine
                .compile_with_entrypoint(&regorus::Rc::from(rule_path.as_str()))
                .map_err(|err| guard::syntax_error(&err))?;

            Ok(Arc::new(RegoDocument {
                prepared: engine,
                rule_path,
                builtins: scan.builtins,
                input_paths: scan.input_paths,
            }) as Arc<dyn CompiledDocument>)
        }))
    }
}

/// Message of the error reported when [`EvaluationBackend::validate_syntax`]
/// or [`EvaluationBackend::compile`] panics.
const BACKEND_PANICKED: &str = "backend panicked";

/// Runs `f`, mapping a panic to [`CompileError::Unsupported`] instead of
/// letting it unwind into the caller — the same protection
/// [`RegoDocument::evaluate`] gives evaluation. `regorus` is a large,
/// evolving dependency; nothing here relies on it never panicking on
/// adversarial input.
fn catch_compile_panic<T>(
    f: impl FnOnce() -> Result<T, CompileError> + panic::UnwindSafe,
) -> Result<T, CompileError> {
    panic::catch_unwind(f).unwrap_or_else(|_| {
        Err(CompileError::Unsupported {
            message: BACKEND_PANICKED.to_owned(),
        })
    })
}

/// An engine with every ambient channel closed: strict builtin errors and
/// `print` captured in memory instead of written to standard error.
fn sandboxed_engine() -> regorus::Engine {
    let mut engine = regorus::Engine::new();
    engine.set_strict_builtin_errors(true);
    engine.set_gather_prints(true);
    engine
}

/// Parses `source` into `engine` behind both depth guards: the token-level
/// bound before `regorus` sees the text, the parsed-tree depth before
/// anything walks the tree recursively. Returns the document's package path.
fn add_guarded_policy(
    engine: &mut regorus::Engine,
    document_name: &str,
    source: &str,
) -> Result<String, CompileError> {
    guard::check_source(document_name, source)?;
    let package = engine
        .add_policy(document_name.to_owned(), source.to_owned())
        .map_err(|err| guard::syntax_error(&err))?;
    guard::check_modules(engine.get_modules())?;
    guard::check_rule_dependencies(engine.get_modules())?;
    Ok(package)
}

/// Runs the introspection walk over every module of `engine` and refuses the
/// document if it holds an unsupported `with` modifier. Only called on trees
/// that passed [`add_guarded_policy`], so the walk's recursion is bounded by
/// [`MAX_AST_DEPTH`].
fn scan_modules<'a>(
    engine: &mut regorus::Engine,
    registered: &'a BTreeSet<String>,
) -> Result<AstScan<'a>, CompileError> {
    let modules = engine.get_modules();
    let mut scan = AstScan::new(registered, function_paths(modules));
    for module in modules {
        scan.module(module);
    }
    match scan.with_refusal.take() {
        Some(message) => Err(CompileError::Unsupported { message }),
        None => Ok(scan),
    }
}

/// Full paths (`data.<package>.<name>`) of every function the modules
/// define, including `default` functions — the names a `with` target is
/// resolved against by the interpreter.
fn function_paths(modules: &[Ref<Module>]) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for module in modules {
        let Ok(module_path) = get_path_string(&module.package.refr, Some("data")) else {
            continue;
        };
        for rule in &module.policy {
            let refr = match rule.as_ref() {
                Rule::Spec {
                    head: RuleHead::Func { refr, .. },
                    ..
                } => refr,
                Rule::Default { refr, args, .. } if !args.is_empty() => refr,
                Rule::Spec { .. } | Rule::Default { .. } => continue,
            };
            if let Ok(path) = get_path_string(refr, Some(&module_path)) {
                paths.insert(path);
            }
        }
    }
    paths
}

fn is_rule_name(entrypoint: &str) -> bool {
    let mut chars = entrypoint.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `module` defines a value rule (not a function) named `name`.
fn defines_rule(module: &Module, name: &str) -> bool {
    module.policy.iter().any(|rule| {
        let refr = match rule.as_ref() {
            Rule::Spec {
                head: RuleHead::Compr { refr, .. } | RuleHead::Set { refr, .. },
                ..
            } => refr,
            Rule::Default { refr, args, .. } if args.is_empty() => refr,
            Rule::Spec { .. } | Rule::Default { .. } => return false,
        };
        get_path_string(refr, None).is_ok_and(|path| path == name)
    })
}

/// A compiled Rego document: a prepared engine plus its introspection.
struct RegoDocument {
    prepared: regorus::Engine,
    rule_path: String,
    builtins: BTreeSet<String>,
    input_paths: BTreeSet<String>,
}

impl fmt::Debug for RegoDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegoDocument")
            .field("rule_path", &self.rule_path)
            .field("builtins", &self.builtins)
            .field("input_paths", &self.input_paths)
            .finish_non_exhaustive()
    }
}

impl CompiledDocument for RegoDocument {
    fn referenced_builtins(&self) -> &BTreeSet<String> {
        &self.builtins
    }

    fn referenced_input_paths(&self) -> &BTreeSet<String> {
        &self.input_paths
    }

    fn evaluate(
        &self,
        ctx: &EvaluationContext,
        bound: CostBound,
    ) -> Result<serde_json::Value, EvaluationError> {
        // The engine is a per-call clone and is dropped on unwind, so no
        // state a panic may have left half-updated is observed afterwards.
        panic::catch_unwind(AssertUnwindSafe(|| self.evaluate_unguarded(ctx, bound)))
            .unwrap_or_else(|_| {
                Err(EvaluationError::Failed {
                    message: "evaluation aborted: the backend panicked".to_owned(),
                })
            })
    }
}

impl RegoDocument {
    fn evaluate_unguarded(
        &self,
        ctx: &EvaluationContext,
        bound: CostBound,
    ) -> Result<serde_json::Value, EvaluationError> {
        let input =
            regorus::Value::deserialize(ctx.document()).map_err(|err| EvaluationError::Failed {
                message: format!("evaluation input is not representable: {err}"),
            })?;

        let mut engine = self.prepared.clone();
        engine.set_execution_timer_config(ExecutionTimerConfig {
            limit: bound.limit,
            check_interval: CHECK_INTERVAL_UNITS,
        });
        engine.set_input(input);

        let started = Instant::now();
        let value = engine
            .eval_rule(self.rule_path.clone())
            .map_err(|err| classify_failure(&err, bound.limit, started.elapsed()))?;
        // The interpreter discards some errors on its own (it did so for the
        // time-limit error inside `with` replacements); a value obtained
        // after the limit elapsed is never reported as one.
        within_bound(started, bound.limit)?;

        if value == regorus::Value::Undefined {
            return Ok(serde_json::Value::Null);
        }
        to_json(&value, started, bound.limit)
    }
}

/// [`EvaluationError::BoundExceeded`] once more than `limit` has elapsed
/// since `started`.
fn within_bound(started: Instant, limit: Duration) -> Result<(), EvaluationError> {
    if started.elapsed() > limit {
        Err(EvaluationError::BoundExceeded { limit })
    } else {
        Ok(())
    }
}

/// Maps a regorus error to the facility's taxonomy. A time-limit error is
/// recognised by type (`regorus::LimitError::TimeLimitExceeded`, including
/// where the interpreter re-wraps it as text inside a function body) or,
/// independently of how the error renders, whenever `elapsed` — measured by
/// this facility with its own clock, never parsed from document-controlled
/// error text — already reached `limit`: an evaluation that ran that long is
/// reported as having exceeded its bound whatever the interpreter's error
/// happened to say, since the wall-clock measurement is the fact that
/// matters to the caller and cannot be forged by the document.
fn classify_failure(err: &anyhow::Error, limit: Duration, elapsed: Duration) -> EvaluationError {
    let typed = err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<regorus::LimitError>(),
            Some(regorus::LimitError::TimeLimitExceeded { .. })
        )
    });
    if typed || elapsed >= limit {
        EvaluationError::BoundExceeded { limit }
    } else {
        EvaluationError::Failed {
            message: format!("{err:#}").trim().to_owned(),
        }
    }
}

fn unrepresentable() -> EvaluationError {
    EvaluationError::Failed {
        message: UNREPRESENTABLE_RESULT.to_owned(),
    }
}

/// A pending step of [`to_json`].
enum Conversion<'a> {
    /// Convert this value, nested `depth` container levels below the
    /// entrypoint's own value, and push the result.
    Visit(&'a regorus::Value, usize),
    /// Pop this many converted values into an array.
    Array(usize),
    /// Pop one converted value per key into an object.
    Object(Vec<String>),
}

/// Converts an evaluation result to JSON iteratively (a deep value cannot
/// overflow the stack) under the bound's clock (a large shared-structure
/// value cannot run unbounded). Refuses values JSON does not represent
/// faithfully: object keys that are not strings, integers outside the
/// `i64`/`u64` range, non-finite numbers, nested undefined values and results
/// nested deeper than [`MAX_RESULT_DEPTH`]. Sets become arrays in sorted
/// order.
fn to_json(
    value: &regorus::Value,
    started: Instant,
    limit: Duration,
) -> Result<serde_json::Value, EvaluationError> {
    let mut pending = vec![Conversion::Visit(value, 0)];
    let mut converted: Vec<serde_json::Value> = Vec::new();
    let mut visited: u32 = 0;

    while let Some(step) = pending.pop() {
        match step {
            Conversion::Visit(value, depth) => {
                visited = visited.wrapping_add(1);
                if visited.is_multiple_of(CONVERSION_CHECK_INTERVAL) {
                    within_bound(started, limit)?;
                }
                if depth > MAX_RESULT_DEPTH {
                    return Err(unrepresentable());
                }
                visit(value, depth, &mut pending, &mut converted)?;
            }
            Conversion::Array(len) => {
                let items = pop_converted(&mut converted, len)?;
                converted.push(serde_json::Value::Array(items));
            }
            Conversion::Object(keys) => {
                let values = pop_converted(&mut converted, keys.len())?;
                converted.push(serde_json::Value::Object(
                    keys.into_iter().zip(values).collect(),
                ));
            }
        }
    }
    match (converted.pop(), converted.is_empty()) {
        (Some(json), true) => Ok(json),
        _ => Err(unrepresentable()),
    }
}

fn visit<'a>(
    value: &'a regorus::Value,
    depth: usize,
    pending: &mut Vec<Conversion<'a>>,
    converted: &mut Vec<serde_json::Value>,
) -> Result<(), EvaluationError> {
    match value {
        regorus::Value::Null => converted.push(serde_json::Value::Null),
        regorus::Value::Bool(b) => converted.push(serde_json::Value::Bool(*b)),
        regorus::Value::String(s) => converted.push(serde_json::Value::String(s.to_string())),
        regorus::Value::Number(n) => {
            let number = if let Some(i) = n.as_i64() {
                serde_json::Number::from(i)
            } else if let Some(u) = n.as_u64() {
                serde_json::Number::from(u)
            } else {
                // `as_f64` is `None` for integers beyond 2^53, which no JSON
                // double represents exactly.
                n.as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .ok_or_else(unrepresentable)?
            };
            converted.push(serde_json::Value::Number(number));
        }
        regorus::Value::Array(items) => {
            let items: Vec<&regorus::Value> = items.iter().collect();
            pending.push(Conversion::Array(items.len()));
            pending.extend(
                items
                    .into_iter()
                    .rev()
                    .map(|item| Conversion::Visit(item, depth + 1)),
            );
        }
        regorus::Value::Set(items) => {
            pending.push(Conversion::Array(items.len()));
            pending.extend(
                items
                    .iter_sorted()
                    .rev()
                    .map(|item| Conversion::Visit(item, depth + 1)),
            );
        }
        regorus::Value::Object(fields) => {
            let mut keys = Vec::with_capacity(fields.len());
            let mut values = Vec::with_capacity(fields.len());
            for (key, value) in fields.iter_sorted() {
                let regorus::Value::String(key) = key else {
                    return Err(unrepresentable());
                };
                keys.push(key.to_string());
                values.push(value);
            }
            pending.push(Conversion::Object(keys));
            pending.extend(
                values
                    .into_iter()
                    .rev()
                    .map(|item| Conversion::Visit(item, depth + 1)),
            );
        }
        regorus::Value::Undefined => return Err(unrepresentable()),
    }
    Ok(())
}

fn pop_converted(
    converted: &mut Vec<serde_json::Value>,
    len: usize,
) -> Result<Vec<serde_json::Value>, EvaluationError> {
    let at = converted
        .len()
        .checked_sub(len)
        .ok_or_else(unrepresentable)?;
    Ok(converted.split_off(at))
}

/// Parsed-form walk collecting referenced builtins and static input paths,
/// and refusing unsupported `with` modifiers.
///
/// The walk is recursive; it only ever runs on trees that passed the depth
/// guards (see `add_guarded_policy`), so its recursion is bounded by
/// [`MAX_AST_DEPTH`].
///
/// Structure (rule roots, statement and expression children) comes from the
/// depth guard's enumeration, so the guard measures exactly the tree this walk
/// inspects. Its match over [`Expr`] is exhaustive on purpose: a `regorus`
/// feature that adds an expression kind fails the build there instead of
/// hiding calls from the denylist screen.
struct AstScan<'a> {
    registered: &'a BTreeSet<String>,
    /// Full paths of every function the document defines.
    functions: BTreeSet<String>,
    /// `data.<package>` of the module being walked.
    module_path: String,
    /// Import aliases of the module being walked (alias -> full path
    /// components, rooted at `input` or `data`).
    imports: BTreeMap<String, Vec<String>>,
    /// Import aliases bound to a path inside `input` (alias -> segments).
    input_aliases: BTreeMap<String, Vec<String>>,
    builtins: BTreeSet<String>,
    input_paths: BTreeSet<String>,
    /// Why the first unsupported `with` modifier found is refused.
    with_refusal: Option<String>,
}

impl<'a> AstScan<'a> {
    const fn new(registered: &'a BTreeSet<String>, functions: BTreeSet<String>) -> Self {
        Self {
            registered,
            functions,
            module_path: String::new(),
            imports: BTreeMap::new(),
            input_aliases: BTreeMap::new(),
            builtins: BTreeSet::new(),
            input_paths: BTreeSet::new(),
            with_refusal: None,
        }
    }

    fn module(&mut self, module: &Module) {
        self.module_path = get_path_string(&module.package.refr, Some("data")).unwrap_or_default();
        self.imports = guard::import_aliases(module);
        self.input_aliases = self
            .imports
            .iter()
            .filter_map(|(alias, path)| match path.split_first() {
                Some((root, segments)) if root == "input" => {
                    Some((alias.clone(), segments.to_vec()))
                }
                _ => None,
            })
            .collect();
        for rule in &module.policy {
            self.rule(rule);
        }
    }

    fn rule(&mut self, rule: &Rule) {
        let mut roots = Vec::new();
        guard::rule_roots(rule, &mut roots);
        self.nodes(roots);
    }

    fn query(&mut self, query: &Query) {
        let mut children = Vec::new();
        for stmt in &query.stmts {
            guard::literal_children(&stmt.literal, &mut children);
            self.nodes(children.drain(..));
            for modifier in &stmt.with_mods {
                self.with_modifier(modifier);
            }
        }
    }

    fn nodes<'n>(&mut self, nodes: impl IntoIterator<Item = guard::Node<'n>>) {
        for node in nodes {
            match node {
                guard::Node::Expr(expr) => self.expr(expr),
                guard::Node::Query(query) => self.query(query),
            }
        }
    }

    /// `with <target> as <replacement>`: the target is overridden rather than
    /// read, so only the replacement is scanned. A replacement that names a
    /// builtin is counted as referenced. The target must be a document the
    /// interpreter replaces as data (see [`Self::with_target_refusal`]).
    fn with_modifier(&mut self, modifier: &WithModifier) {
        if self.with_refusal.is_none() {
            self.with_refusal = self.with_target_refusal(&modifier.refr);
        }
        if let Ok(path) = get_path_string(&modifier.r#as, None) {
            self.note_builtin(path);
        }
        self.expr(&modifier.r#as);
    }

    /// Why a `with` target is refused, if it is. The target is resolved the
    /// way the interpreter resolves it — static path components, a leading
    /// import alias rewritten to its import — and must then be `input`,
    /// `input.*` or `data.*` without naming a function or builtin. A function
    /// target is refused because the interpreter evaluates its replacement
    /// with every error, the bound's time-limit error included, discarded.
    fn with_target_refusal(&self, refr: &Ref<Expr>) -> Option<String> {
        let Ok(components) = Parser::get_path_ref_components(refr) else {
            return Some(format!(
                "`with` target `{}` is not a static reference",
                refr.span().text()
            ));
        };
        let mut path: Vec<String> = components
            .iter()
            .map(|span| span.text().to_owned())
            .collect();
        if let Some(head) = path.first()
            && head != "data"
            && let Some(import) = self.imports.get(head)
        {
            path.splice(0..1, import.iter().cloned());
        }
        let target = path.join(".");

        let is_function = self.registered.contains(&target)
            || self.functions.contains(&target)
            || self
                .functions
                .contains(&format!("{}.{target}", self.module_path));
        if is_function {
            return Some(format!(
                "`with` target `{target}` is a function or builtin; only `input`, \
                 `input.*` and non-function `data.*` documents may be replaced"
            ));
        }
        let is_document =
            target == "input" || target.starts_with("input.") || target.starts_with("data.");
        (!is_document).then(|| {
            format!("`with` target `{target}` is not `input`, `input.*` or a `data.*` document")
        })
    }

    fn exprs<'e>(&mut self, exprs: impl IntoIterator<Item = &'e Expr>) {
        for expr in exprs {
            self.expr(expr);
        }
    }

    fn note_builtin(&mut self, path: String) {
        if self.registered.contains(&path) {
            self.builtins.insert(path);
        }
    }

    /// Variables, calls and references are handled here; every other kind
    /// is walked through [`guard::expr_children`], the facility's one
    /// exhaustive match over [`Expr`].
    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Var { span, .. } => {
                if let Some(prefix) = self.input_aliases.get(span.text())
                    && !prefix.is_empty()
                {
                    self.input_paths.insert(prefix.join("."));
                }
            }
            // The target is a name, not a read: only its dynamic indices are
            // evaluated.
            Expr::Call { fcn, params, .. } => {
                if let Ok(path) = get_path_string(fcn, None) {
                    self.note_builtin(path);
                }
                self.exprs(guard::reference(fcn).dynamic);
                self.exprs(params.iter().map(|param| &**param));
            }
            Expr::RefDot { .. } | Expr::RefBrack { .. } => self.reference(expr),
            other => {
                let mut children = Vec::new();
                guard::expr_children(other, &mut children);
                self.nodes(children);
            }
        }
    }

    /// Records the longest static prefix of a reference rooted at `input`
    /// (directly or through an import alias) and scans every dynamic part.
    fn reference(&mut self, expr: &Expr) {
        let found = guard::reference(expr);
        self.exprs(found.dynamic);
        let Some((root, rest)) = found.path.as_deref().and_then(<[String]>::split_first) else {
            return;
        };
        let prefix: &[String] = if root == "input" {
            &[]
        } else {
            match self.input_aliases.get(root) {
                Some(prefix) => prefix,
                None => return,
            }
        };
        let path: Vec<&str> = prefix.iter().chain(rest).map(String::as_str).collect();
        if !path.is_empty() {
            self.input_paths.insert(path.join("."));
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "rego_tests.rs"]
mod rego_tests;
