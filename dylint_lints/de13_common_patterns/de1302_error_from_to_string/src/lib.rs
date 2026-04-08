// Created: 2026-03-13 by Constructor Tech
// Updated: 2026-03-16 by Constructor Tech
#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use clippy_utils::ty::implements_trait;
use rustc_hir::{self as hir, Expr, ExprKind, ImplItemKind, ItemKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::ty::{Ty, TypeckResults};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Detects `.to_string()` calls inside `fn from()` bodies within
    /// `impl From<X> for Y` blocks where X or Y implements `std::error::Error`,
    /// which silently destroys the error chain.
    ///
    /// ### Why is this bad?
    ///
    /// When you call `e.to_string()` inside a `From` impl, you convert the original
    /// error to a string and discard it. The resulting error:
    /// - Has no `.source()` (error chain is broken)
    /// - Cannot be matched or downcast by callers
    /// - Loses structured metadata (error codes, fields, etc.)
    ///
    /// Tools like `anyhow`, `thiserror`'s `#[from]`, or storing the error directly
    /// preserve the chain without any extra effort.
    ///
    /// Unlike the early-pass version, this lint gates on whether the source or target
    /// type actually implements `std::error::Error`, eliminating false positives from
    /// name-based heuristics.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // Bad - DatabaseError is swallowed; callers can't inspect the root cause
    /// impl From<DatabaseError> for AppError {
    ///     fn from(e: DatabaseError) -> Self {
    ///         AppError::Internal(e.to_string())  // chain lost!
    ///     }
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// // Good - store the source error; chain preserved
    /// #[derive(thiserror::Error, Debug)]
    /// enum AppError {
    ///     #[error(transparent)]
    ///     Database(#[from] DatabaseError),
    /// }
    /// ```
    pub DE1302_ERROR_FROM_TO_STRING,
    Deny,
    "calling .to_string() in From<XxxError> impl destroys the error chain (DE1302)"
}

/// Returns true if `ty` implements `std::error::Error`.
///
/// Uses the `rustc_diagnostic_item = "Error"` marker and `clippy_utils::ty::implements_trait`
/// for proper trait resolution. Handles ADTs, type aliases, and generic params with bounds.
fn implements_error<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    let Some(error_did) = cx.tcx.get_diagnostic_item(rustc_span::sym::Error) else {
        return false;
    };
    implements_trait(cx, ty, error_did, &[])
}

struct ToStringVisitor<'tcx, 'cx> {
    cx: &'cx LateContext<'tcx>,
    /// Typeck results for the `fn from` body being walked.
    /// Used to check the receiver type of `.to_string()` calls so we only flag
    /// calls on Error-implementing types, not on `&str` or other non-error values.
    typeck: &'tcx TypeckResults<'tcx>,
}

impl<'tcx> hir::intravisit::Visitor<'tcx> for ToStringVisitor<'tcx, '_> {
    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        if let ExprKind::MethodCall(seg, recv, args, _) = expr.kind {
            if seg.ident.name.as_str() == "to_string" && args.is_empty() {
                // Only flag .to_string() when the receiver itself implements Error.
                // This prevents false positives for calls like `"literal".to_string()`
                // or `some_string.to_string()` inside a From<XxxError> body.
                let recv_ty = self.typeck.expr_ty(recv);
                if implements_error(self.cx, recv_ty) {
                    self.cx.span_lint(DE1302_ERROR_FROM_TO_STRING, expr.span, |diag| {
                        diag.primary_message(
                            "`.to_string()` in `From` impl destroys the error chain (DE1302)",
                        );
                        diag.help(
                            "store the source error directly, use an enum variant, or use `#[from]` with thiserror",
                        );
                        diag.note(
                            "`.to_string()` discards the original error type: `.source()` returns None and the error cannot be downcast",
                        );
                    });
                }
            }
        }
        hir::intravisit::walk_expr(self, expr);
    }
}

impl<'tcx> LateLintPass<'tcx> for De1302ErrorFromToString {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx hir::Item<'tcx>) {
        let ItemKind::Impl(impl_block) = item.kind else {
            return;
        };

        // Only examine `impl From<X> for Y` blocks
        let Some(trait_ref) = impl_block.of_trait else {
            return;
        };
        let Some(last_seg) = trait_ref.trait_ref.path.segments.last() else {
            return;
        };
        if last_seg.ident.name.as_str() != "From" {
            return;
        }

        // Resolve the actual types from the type system.
        // For `impl From<X> for Y`: args[0] = Y (Self), args[1] = X (source type).
        let impl_def_id = item.owner_id.def_id;
        let Some(impl_trait_ref) = cx.tcx.impl_trait_ref(impl_def_id) else {
            return;
        };
        let impl_trait_ref = impl_trait_ref.instantiate_identity();
        let source_ty = impl_trait_ref.args.type_at(1); // X
        let target_ty = impl_trait_ref.args.type_at(0); // Y = Self

        // Gate: at least one of source or target must actually implement std::error::Error.
        // This replaces name heuristics, eliminating false positives like
        // `impl From<String> for ParseError` where String is not an Error.
        if !implements_error(cx, source_ty) && !implements_error(cx, target_ty) {
            return;
        }

        // Walk every `fn from` body looking for .to_string() calls.
        // tcx.hir() was removed in nightly-2025-09-18; use hir_node_by_def_id instead.
        for item_ref in impl_block.items {
            let node = cx.tcx.hir_node_by_def_id(item_ref.owner_id.def_id);
            let hir::Node::ImplItem(impl_item) = node else {
                continue;
            };
            if impl_item.ident.name.as_str() != "from" {
                continue;
            }
            let ImplItemKind::Fn(_, body_id) = impl_item.kind else {
                continue;
            };
            let body = cx.tcx.hir_body(body_id);
            let typeck = cx.tcx.typeck(item_ref.owner_id.def_id);
            let mut visitor = ToStringVisitor { cx, typeck };
            hir::intravisit::walk_expr(&mut visitor, body.value);
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
        lint_utils::test_comment_annotations_match_stderr(&ui_dir, "DE1302", "to_string");
    }
}
