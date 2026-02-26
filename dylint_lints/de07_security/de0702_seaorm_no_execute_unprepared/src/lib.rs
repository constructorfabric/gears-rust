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
    /// Detects raw SQL usage in module code:
    /// - `ConnectionTrait::execute_unprepared(...)` — always denied
    /// - `Statement::from_string(...)` — denied as it constructs raw SQL
    ///
    /// ### Why is this bad?
    ///
    /// Raw SQL bypasses the Secure ORM layer entirely:
    /// - No tenant isolation enforcement
    /// - No access scope filtering
    /// - Prone to SQL injection if inputs are not carefully sanitized
    /// - Harder to audit and maintain
    ///
    /// ### Known Exclusions
    ///
    /// This lint does NOT apply to:
    /// - `libs/modkit-db/` — the secure wrapper library itself
    /// - `libs/` in general — infrastructure libraries may need raw SQL for migrations
    /// - `apps/hyperspot-server/` — server setup code
    ///
    /// ### Known Limitations
    ///
    /// This lint uses method-name and path-segment matching, not DefId-based type resolution.
    /// - `execute_unprepared` matches any method with that name regardless of the receiver type.
    /// - `Statement::from_string` matches path segments as strings; aliasing (e.g.,
    ///   `use sea_orm::Statement as Stmt; Stmt::from_string(...)`) is not detected.
    ///
    /// This is an intentional tradeoff: full cross-crate type resolution is not reliably
    /// available in Dylint `LateLintPass`. The lint acts as a guardrail, not a security
    /// boundary — deliberate circumvention is caught by code review.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // Bad — raw SQL
    /// conn.execute_unprepared("DELETE FROM users WHERE id = 1").await?;
    ///
    /// // Bad — raw SQL via Statement
    /// let stmt = Statement::from_string(DbBackend::Postgres, "SELECT * FROM users");
    /// conn.execute(stmt).await?;
    ///
    /// // Good — use Secure ORM
    /// user::Entity::find().secure().scope_with(&scope).all(conn).await?;
    /// ```
    pub DE0702_SEAORM_NO_EXECUTE_UNPREPARED,
    Deny,
    "raw SQL usage detected; use Secure ORM queries instead (DE0702)"
}

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

/// Check if a QPath ends with a specific function name.
fn qpath_ends_with(qpath: &rustc_hir::QPath<'_>, name: &str) -> bool {
    match qpath {
        rustc_hir::QPath::Resolved(_, path) => {
            path.segments.last().is_some_and(|s| s.ident.name.as_str() == name)
        }
        rustc_hir::QPath::TypeRelative(_ty, seg) => seg.ident.name.as_str() == name,
        _ => false,
    }
}

/// Check if a QPath matches `Statement::from_string` or `sea_orm::Statement::from_string`.
fn is_statement_from_string(qpath: &rustc_hir::QPath<'_>) -> bool {
    match qpath {
        rustc_hir::QPath::TypeRelative(ty, seg) => {
            if seg.ident.name.as_str() != "from_string" {
                return false;
            }
            // Check if the type is `Statement`
            if let rustc_hir::TyKind::Path(inner_qpath) = &ty.kind {
                return qpath_ends_with(inner_qpath, "Statement");
            }
            false
        }
        rustc_hir::QPath::Resolved(_, path) => {
            let segments: Vec<&str> = path.segments.iter().map(|s| s.ident.name.as_str()).collect();
            // Match Statement::from_string or sea_orm::Statement::from_string
            segments.len() >= 2
                && segments[segments.len() - 1] == "from_string"
                && segments[segments.len() - 2] == "Statement"
        }
        _ => false,
    }
}

impl<'tcx> LateLintPass<'tcx> for De0702SeaormNoExecuteUnprepared {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        match &expr.kind {
            // Detect .execute_unprepared(...) method calls
            ExprKind::MethodCall(seg, _receiver, _args, call_span) => {
                let method_name = seg.ident.name.as_str();

                if method_name != "execute_unprepared" {
                    return;
                }

                if !should_lint_file(cx.sess().source_map(), *call_span) {
                    return;
                }

                cx.span_lint(
                    DE0702_SEAORM_NO_EXECUTE_UNPREPARED,
                    expr.span,
                    |diag| {
                        diag.primary_message(
                            "execute_unprepared() detected — raw SQL bypasses Secure ORM (DE0702)",
                        );
                        diag.help("use SecureSelect, SecureUpdateMany, or SecureDeleteMany instead");
                        diag.note(
                            "raw SQL bypasses tenant isolation and access scoping; use the Secure ORM layer",
                        );
                    },
                );
            }
            // Detect Statement::from_string(...) calls
            ExprKind::Call(func, _args) => {
                if let ExprKind::Path(qpath) = &func.kind {
                    if !is_statement_from_string(qpath) {
                        return;
                    }

                    if !should_lint_file(cx.sess().source_map(), func.span) {
                        return;
                    }

                    cx.span_lint(
                        DE0702_SEAORM_NO_EXECUTE_UNPREPARED,
                        expr.span,
                        |diag| {
                            diag.primary_message(
                                "Statement::from_string() detected — raw SQL construction bypasses Secure ORM (DE0702)",
                            );
                            diag.help(
                                "use Entity::find().secure().scope_with() or other Secure ORM wrappers instead",
                            );
                            diag.note(
                                "raw SQL bypasses tenant isolation and access scoping; use the Secure ORM layer",
                            );
                        },
                    );
                }
            }
            _ => {}
        }
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
        lint_utils::test_comment_annotations_match_stderr(&ui_dir, "DE0702", "raw SQL");
    }
}
