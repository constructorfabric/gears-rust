#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_span;

use lint_utils::{filename_str, is_temp_path};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Detects unscoped SeaORM queries in module code. Specifically flags:
    /// - `Entity::find().all(conn)` / `.one(conn)` / `.count(conn)` without `.secure().scope_with()`
    /// - `Entity::update_many().exec(conn)` without `.secure().scope_with()`
    /// - `Entity::delete_many().exec(conn)` without `.secure().scope_with()`
    ///
    /// ### Why is this bad?
    ///
    /// Plain SeaORM queries bypass the Secure ORM layer (`SecureSelect`, `SecureUpdateMany`,
    /// `SecureDeleteMany`), which enforces tenant isolation and access scoping via typestate.
    /// Unscoped queries can leak data across tenants or allow unauthorized modifications.
    ///
    /// ### Known Exclusions
    ///
    /// This lint does NOT apply to:
    /// - `libs/modkit-db/` — the secure wrapper library itself
    /// - `libs/` in general — infrastructure libraries may need raw access
    /// - `apps/hyperspot-server/` — server setup code
    ///
    /// ### Known Limitations
    ///
    /// This lint uses method-name matching on the HIR call chain, not DefId-based type
    /// resolution. A local trait that defines `.secure()` and `.scope_with()` methods with
    /// matching names would satisfy the lint without providing actual Secure ORM scoping.
    /// This is an intentional tradeoff: full type resolution for cross-crate traits is not
    /// reliably available in Dylint `LateLintPass`. The lint acts as a guardrail, not a
    /// security boundary — deliberate circumvention is caught by code review.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // Bad — unscoped read
    /// let users = user::Entity::find().all(conn).await?;
    ///
    /// // Good — scoped read
    /// let users = user::Entity::find()
    ///     .secure()
    ///     .scope_with(&scope)
    ///     .all(conn)
    ///     .await?;
    /// ```
    pub DE0701_SEAORM_NO_UNSCOPED_QUERY,
    Deny,
    "unscoped SeaORM query detected; use .secure().scope_with() instead (DE0701)"
}

/// Terminal methods on `Select` that execute a query.
const SELECT_TERMINALS: &[&str] = &["all", "one", "count"];

/// Terminal methods on `UpdateMany` / `DeleteMany` that execute a mutation.
const EXEC_TERMINALS: &[&str] = &["exec"];

/// Methods in the call chain that BOTH must be present to indicate secure usage.
/// `.secure()` creates the typestate wrapper, `.scope_with()` applies the tenant filter.
const SECURE_MARKERS: &[&str] = &["secure", "scope_with"];

/// Entry-point methods that start a SeaORM query chain we care about.
/// `find` → Select chain, `update_many` / `delete_many` → mutation chain.
const CHAIN_ENTRY_POINTS: &[&str] = &["find", "find_by_id", "update_many", "delete_many"];

/// Check if a file path should be linted.
/// We only lint module crate code (under `modules/`).
fn should_lint_file(source_map: &rustc_span::source_map::SourceMap, span: rustc_span::Span) -> bool {
    let Some(path) = filename_str(source_map, span) else {
        return false;
    };

    // Always lint files in temp directories (UI tests)
    if is_temp_path(&path) {
        return true;
    }

    // Normalize path separators for cross-platform compatibility (Windows uses `\`)
    let normalized = path.replace('\\', "/");

    // Skip migrations — they legitimately need raw DDL
    if normalized.contains("migrations/") {
        return false;
    }

    // Only lint files under modules/
    normalized.contains("modules/")
}

/// Result of walking a method-call chain.
struct ChainInfo<'tcx> {
    /// Method names from root to terminal, e.g. `["find", "filter", "all"]`.
    methods: Vec<&'tcx str>,
    /// `true` when the root of the chain is an associated function call
    /// (e.g. `Entity::find()`, `Model::find_by_id()`), meaning it has the
    /// `Type::func()` form. This distinguishes SeaORM entry points from
    /// arbitrary method calls that happen to share the same name.
    root_is_associated_call: bool,
}

/// Walk a method-call chain backwards (from receiver to origin) collecting method names.
/// Returns a [`ChainInfo`] with the ordered list and whether the root is an associated call.
///
/// For `Entity::find().filter(x).all(conn)`:
///  → methods: `["find", "filter", "all"]`, root_is_associated_call: true
fn collect_chain_info<'tcx>(expr: &'tcx Expr<'tcx>) -> ChainInfo<'tcx> {
    let mut methods = Vec::new();
    let mut current = expr;
    let mut root_is_associated_call = false;

    loop {
        match &current.kind {
            ExprKind::MethodCall(seg, receiver, _args, _span) => {
                methods.push(seg.ident.name.as_str());
                current = receiver;
            }
            ExprKind::Call(func, _args) => {
                // Associated function call like Entity::find()
                if let ExprKind::Path(qpath) = &func.kind {
                    if let Some(last_seg) = last_path_segment(qpath) {
                        methods.push(last_seg);
                        root_is_associated_call = true;
                    }
                }
                break;
            }
            _ => break,
        }
    }

    methods.reverse();
    ChainInfo { methods, root_is_associated_call }
}

/// Extract the last segment name from a QPath.
fn last_path_segment<'tcx>(qpath: &'tcx rustc_hir::QPath<'tcx>) -> Option<&'tcx str> {
    match qpath {
        rustc_hir::QPath::Resolved(_, path) => {
            path.segments.last().map(|s| s.ident.name.as_str())
        }
        rustc_hir::QPath::TypeRelative(_ty, seg) => Some(seg.ident.name.as_str()),
        _ => None,
    }
}

/// Determine if a method chain contains ALL required secure markers
/// (both `.secure()` and `.scope_with()`).
fn chain_has_secure_marker(methods: &[&str]) -> bool {
    SECURE_MARKERS.iter().all(|marker| methods.contains(marker))
}

/// Determine if the chain starts with a SeaORM entry point we care about.
/// Only matches when the root of the chain is an associated function call
/// (e.g. `Entity::find()`) whose name is in [`CHAIN_ENTRY_POINTS`].
fn chain_has_entry_point(info: &ChainInfo<'_>) -> bool {
    info.root_is_associated_call
        && info.methods.first().is_some_and(|m| CHAIN_ENTRY_POINTS.contains(m))
}

/// Get the operation-specific help message based on the entry point and terminal method.
fn help_message(methods: &[&str], terminal: &str) -> &'static str {
    if methods.contains(&"update_many") {
        "use Entity::update_many().secure().scope_with(&scope).exec(conn) instead"
    } else if methods.contains(&"delete_many") {
        "use Entity::delete_many().secure().scope_with(&scope).exec(conn) instead"
    } else {
        match terminal {
            "all" => "use Entity::find().secure().scope_with(&scope).all(conn) instead",
            "one" => "use Entity::find().secure().scope_with(&scope).one(conn) instead",
            "count" => "use Entity::find().secure().scope_with(&scope).count(conn) instead",
            _ => "use Entity::find().secure().scope_with(&scope).all(conn) instead",
        }
    }
}

/// Get the terminal kind for the error message.
fn terminal_kind(terminal: &str) -> &'static str {
    match terminal {
        "all" => "unscoped .all() query",
        "one" => "unscoped .one() query",
        "count" => "unscoped .count() query",
        "exec" => "unscoped .exec() on update/delete",
        _ => "unscoped query",
    }
}

impl<'tcx> LateLintPass<'tcx> for De0701SeaormNoUnscopedQuery {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        // Only check method calls
        let ExprKind::MethodCall(method_seg, _receiver, _args, call_span) = &expr.kind else {
            return;
        };

        let method_name = method_seg.ident.name.as_str();

        // Quick check: is this a terminal method we care about?
        let is_select_terminal = SELECT_TERMINALS.contains(&method_name);
        let is_exec_terminal = EXEC_TERMINALS.contains(&method_name);

        if !is_select_terminal && !is_exec_terminal {
            return;
        }

        // Check file path filter
        if !should_lint_file(cx.sess().source_map(), *call_span) {
            return;
        }

        // Collect the full method chain
        let info = collect_chain_info(expr);

        // Must start with a SeaORM associated-call entry point
        if !chain_has_entry_point(&info) {
            return;
        }

        let chain = &info.methods;

        // For .exec() terminals, only flag if chain contains update_many or delete_many
        if is_exec_terminal
            && !chain.contains(&"update_many")
            && !chain.contains(&"delete_many")
        {
            return;
        }

        // If chain already has .secure() and .scope_with(), this is a properly scoped query
        if chain_has_secure_marker(chain) {
            return;
        }

        // Emit the lint
        let kind = terminal_kind(method_name);
        cx.span_lint(
            DE0701_SEAORM_NO_UNSCOPED_QUERY,
            expr.span,
            |diag| {
                diag.primary_message(format!("{kind} detected — bypasses Secure ORM scoping (DE0701)"));
                diag.help(help_message(chain, method_name));
                diag.note(
                    "unscoped queries bypass tenant isolation; wrap with .secure().scope_with(&scope)",
                );
            },
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ui_examples() {
        dylint_testing::ui_test_examples(env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn test_comment_annotations_match_stderr() {
        let ui_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
        lint_utils::test_comment_annotations_match_stderr(&ui_dir, "DE0701", "unscoped query");
    }
}
