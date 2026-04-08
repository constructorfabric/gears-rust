// Created: 2026-03-13 by Constructor Tech
// Updated: 2026-03-17 by Constructor Tech
#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_hir::def_id::DefId;
use rustc_hir::{self as hir, Expr, ExprKind, ImplItemKind, ItemKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::ty::{Ty, TypeckResults};
use rustc_span::symbol::Symbol;

/// Pre-interned symbols used in `is_ptr_write_bytes`. Initialized once at first call
/// rather than re-interning on every lint invocation.
static SYM_CORE: std::sync::LazyLock<Symbol> =
    std::sync::LazyLock::new(|| Symbol::intern("core"));
static SYM_STD: std::sync::LazyLock<Symbol> =
    std::sync::LazyLock::new(|| Symbol::intern("std"));
static SYM_WRITE_BYTES: std::sync::LazyLock<Symbol> =
    std::sync::LazyLock::new(|| Symbol::intern("write_bytes"));

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Detects manual byte-zeroing (`*b = 0` or `.fill(0)`) inside `impl Drop`
    /// implementations, which the LLVM optimizer may legally eliminate.
    ///
    /// ### Why is this bad?
    ///
    /// The LLVM optimizer performs dead-store elimination: if it can prove that
    /// a write to memory is never read again before the memory is freed, it may
    /// remove the write entirely. Manual zeroing in `Drop::drop` is almost always
    /// a dead store from the optimizer's perspective. The `secrecy` and `zeroize`
    /// crates work around this using a compiler memory fence to prevent removal.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // Bad - may be silently optimized away
    /// impl Drop for SecretKey {
    ///     fn drop(&mut self) {
    ///         self.data.fill(0);  // LLVM may remove this!
    ///     }
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// // Good (preferred for secrets) - secrecy provides zeroization + redacted Debug
    /// use secrecy::{ExposeSecret, SecretBox};
    /// pub type SecretKey = SecretBox<Vec<u8>>;
    ///
    /// // Good (alternative) - zeroize when only wiping is needed
    /// use zeroize::Zeroize;
    /// impl Drop for SecretKey {
    ///     fn drop(&mut self) {
    ///         self.data.zeroize();
    ///     }
    /// }
    /// ```
    ///
    /// ### Limitations
    ///
    /// This lint only inspects the immediate body of `Drop::drop` and does **not**
    /// perform interprocedural analysis. Zeroing delegated to a helper function will
    /// not be detected:
    ///
    /// ```rust,ignore
    /// fn secure_erase(buf: &mut Vec<u8>) {
    ///     buf.fill(0); // not flagged — outside Drop::drop
    /// }
    ///
    /// impl Drop for SecretKey {
    ///     fn drop(&mut self) {
    ///         secure_erase(&mut self.data); // not flagged — indirect call
    ///     }
    /// }
    /// ```
    ///
    /// The helper call itself escapes the lint, but the underlying zeroing is still
    /// at risk: LLVM may inline `secure_erase` and then eliminate the dead store.
    /// Use `zeroize` or `secrecy` in all cases to ensure the compiler fence is in place.
    pub DE0707_DROP_ZEROIZE,
    Deny,
    "manual byte-zeroing in Drop may be optimized away; use `secrecy::SecretBox` or the `zeroize` crate (DE0707)"
}

/// Returns true if `expr` is the integer literal `0` (with any type suffix, e.g. `0u8`).
fn is_zero_literal(expr: &Expr<'_>) -> bool {
    if let ExprKind::Lit(lit) = expr.kind {
        if let rustc_ast::ast::LitKind::Int(n, _) = lit.node {
            return n.get() == 0;
        }
    }
    false
}

/// Returns true if `ty` is a raw pointer or reference to `u8` (`*mut u8`, `*const u8`,
/// `&u8`, or `&mut u8`). Used to validate `*ptr = 0` deref-assign patterns.
fn is_u8_ptr_or_ref(ty: Ty<'_>) -> bool {
    let pointee = match ty.kind() {
        rustc_middle::ty::TyKind::RawPtr(pointee, _) => pointee,
        rustc_middle::ty::TyKind::Ref(_, pointee, _) => pointee,
        _ => return false,
    };
    matches!(
        pointee.kind(),
        rustc_middle::ty::TyKind::Uint(rustc_middle::ty::UintTy::U8)
    )
}

/// Returns true if the adjusted type (after auto-deref coercions) has `u8` as its element
/// type — i.e., `[u8]` or `[u8; N]`. Used to validate `slice.fill(0)` patterns.
fn has_u8_element(ty: Ty<'_>) -> bool {
    // peel_refs strips &/&mut wrappers left over after auto-deref
    let ty = ty.peel_refs();
    match ty.kind() {
        rustc_middle::ty::TyKind::Slice(elem) | rustc_middle::ty::TyKind::Array(elem, _) => {
            matches!(
                elem.kind(),
                rustc_middle::ty::TyKind::Uint(rustc_middle::ty::UintTy::U8)
            )
        }
        _ => false,
    }
}

/// Returns true if `def_id` resolves to `core::ptr::write_bytes` or its intrinsic definition,
/// guarding against user-defined functions with the same name.
///
/// Checks the crate origin (must be `core` or `std`) and the item name to avoid matching
/// user-defined `write_bytes` helpers.
fn is_ptr_write_bytes(cx: &LateContext<'_>, def_id: DefId) -> bool {
    let krate = cx.tcx.crate_name(def_id.krate);
    if krate != *SYM_CORE && krate != *SYM_STD {
        return false;
    }
    cx.tcx.item_name(def_id) == *SYM_WRITE_BYTES
}

struct ZeroingVisitor<'tcx, 'cx> {
    cx: &'cx LateContext<'tcx>,
    /// Typeck results for the `fn drop` body being walked.
    typeck: &'tcx TypeckResults<'tcx>,
}

impl<'tcx> hir::intravisit::Visitor<'tcx> for ZeroingVisitor<'tcx, '_> {
    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        match expr.kind {
            // Pattern: *buf = 0 (deref-assign to zero).
            // Only flagged when the inner expression is a `u8` pointer/reference.
            ExprKind::Assign(lhs, rhs, _) => {
                if let ExprKind::Unary(hir::UnOp::Deref, inner) = lhs.kind {
                    if is_zero_literal(rhs) {
                        let inner_ty = self.typeck.expr_ty(inner);
                        if is_u8_ptr_or_ref(inner_ty) {
                            self.cx.span_lint(DE0707_DROP_ZEROIZE, expr.span, |diag| {
                                diag.primary_message(
                                    "manual byte-zeroing in `Drop::drop` may be eliminated by the optimizer (DE0707)",
                                );
                                diag.help(
                                    "use `secrecy::SecretBox` or `zeroize`: `.zeroize()` / `#[derive(ZeroizeOnDrop)]`",
                                );
                                diag.note(
                                    "LLVM dead-store elimination can legally remove writes that are never read; `zeroize` uses a compiler fence to prevent this",
                                );
                            });
                        }
                    }
                }
            }
            // Pattern: slice.fill(0).
            // Only flagged when the method resolves to core/std (not a custom `fill` method)
            // and the auto-deref'd receiver type is a `[u8]` or `[u8; N]` byte slice.
            ExprKind::MethodCall(seg, recv, args, _) => {
                if seg.ident.name.as_str() == "fill" {
                    if let Some(arg) = args.first() {
                        if is_zero_literal(arg) {
                            let method_in_std = self
                                .typeck
                                .type_dependent_def_id(expr.hir_id)
                                .map_or(false, |did| {
                                    let krate = self.cx.tcx.crate_name(did.krate);
                                    krate == *SYM_CORE || krate == *SYM_STD
                                });
                            // Use adjusted type so Vec<u8> auto-derefs to [u8]
                            let recv_ty = self.typeck.expr_ty_adjusted(recv);
                            if method_in_std && has_u8_element(recv_ty) {
                                self.cx.span_lint(DE0707_DROP_ZEROIZE, expr.span, |diag| {
                                    diag.primary_message(
                                        "manual byte-zeroing in `Drop::drop` may be eliminated by the optimizer (DE0707)",
                                    );
                                    diag.help(
                                        "use `secrecy::SecretBox` or `zeroize`: `.zeroize()` / `#[derive(ZeroizeOnDrop)]`",
                                    );
                                    diag.note(
                                        "LLVM dead-store elimination can legally remove writes that are never read; `zeroize` uses a compiler fence to prevent this",
                                    );
                                });
                            }
                        }
                    }
                }
            }
            // Pattern: ptr::write_bytes(ptr, 0, len).
            // Only flagged when the function resolves to `core::ptr::write_bytes`,
            // not a user-defined helper with the same name.
            ExprKind::Call(func, args) => {
                if args.len() >= 2 {
                    if let Some(fill_byte) = args.get(1) {
                        if is_zero_literal(fill_byte) {
                            if let ExprKind::Path(qpath) = &func.kind {
                                if let Some(def_id) =
                                    self.cx.qpath_res(qpath, func.hir_id).opt_def_id()
                                {
                                    if is_ptr_write_bytes(self.cx, def_id) {
                                        self.cx.span_lint(
                                            DE0707_DROP_ZEROIZE,
                                            expr.span,
                                            |diag| {
                                                diag.primary_message(
                                                    "manual byte-zeroing in `Drop::drop` may be eliminated by the optimizer (DE0707)",
                                                );
                                                diag.help(
                                                    "use `secrecy::SecretBox` or `zeroize`: `.zeroize()` / `#[derive(ZeroizeOnDrop)]`",
                                                );
                                                diag.note(
                                                    "LLVM dead-store elimination can legally remove writes that are never read; `zeroize` uses a compiler fence to prevent this",
                                                );
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        // Always recurse so nested blocks (for loops, unsafe blocks, closures) are visited.
        hir::intravisit::walk_expr(self, expr);
    }
}

impl<'tcx> LateLintPass<'tcx> for De0707DropZeroize {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx hir::Item<'tcx>) {
        let ItemKind::Impl(impl_block) = item.kind else {
            return;
        };

        // Only examine `impl Drop for X` blocks — resolved semantically via lang items
        // to prevent false positives from a custom `Drop` trait with the same name.
        let Some(_) = impl_block.of_trait else {
            return;
        };
        let impl_def_id = item.owner_id.def_id;
        let Some(impl_trait_ref) = cx.tcx.impl_trait_ref(impl_def_id) else {
            return;
        };
        let impl_trait_ref = impl_trait_ref.instantiate_identity();
        let Some(drop_trait_did) = cx.tcx.lang_items().drop_trait() else {
            return;
        };
        if impl_trait_ref.def_id != drop_trait_did {
            return;
        }

        // Walk every `fn drop` body looking for byte-zeroing patterns.
        for item_ref in impl_block.items {
            let node = cx.tcx.hir_node_by_def_id(item_ref.owner_id.def_id);
            let hir::Node::ImplItem(impl_item) = node else {
                continue;
            };
            if impl_item.ident.name.as_str() != "drop" {
                continue;
            }
            let ImplItemKind::Fn(_, body_id) = impl_item.kind else {
                continue;
            };
            let body = cx.tcx.hir_body(body_id);
            let typeck = cx.tcx.typeck(item_ref.owner_id.def_id);
            let mut visitor = ZeroingVisitor { cx, typeck };
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
        lint_utils::test_comment_annotations_match_stderr(&ui_dir, "DE0707", "manual zeroing");
    }
}
