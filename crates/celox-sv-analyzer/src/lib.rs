//! SystemVerilog analyzer for Celox.
//!
//! This crate intentionally mirrors the role of `veryl-analyzer`: it owns
//! SystemVerilog syntax parsing, semantic analysis, elaboration, and analyzer
//! IR. It does not depend on `celox` and must not know about SLT or SIR.

use std::path::Path;

use fxhash::FxHashMap as HashMap;
use thiserror::Error;

pub mod analyze;
pub mod ast;
pub mod ir;
pub mod symbol;
pub mod syntax;
pub mod typecheck;

pub use ir::Ir;

/// Internal marker used to defer division-by-zero state handling until the
/// simulator's two-state/four-state mode is known.
pub const DIV_ZERO_UNKNOWN_LITERAL: &str = "$celox_div_zero_unknown";

/// Errors reported by the SystemVerilog analyzer.
#[derive(Debug, Error)]
pub enum AnalyzerError {
    #[error("SystemVerilog parse error: {0}")]
    Parse(String),
    #[error("Unsupported SystemVerilog construct: {0}")]
    Unsupported(String),
    #[error("Duplicate module declaration: {name}")]
    DuplicateModule { name: String },
    #[error("Duplicate port declaration in module `{module}`: {name}")]
    DuplicatePort { module: String, name: String },
    #[error("Duplicate parameter declaration in module `{module}`: {name}")]
    DuplicateParameter { module: String, name: String },
    #[error("Duplicate instance declaration in module `{module}`: {name}")]
    DuplicateInstance { module: String, name: String },
}

impl miette::Diagnostic for AnalyzerError {}

/// Parse and analyze a SystemVerilog source string.
pub fn analyze_source(code: &str, path: &Path) -> Result<Ir, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    let source = ast::Source::from_syntax(&syntax_tree)?;
    analyze::analyze_source(source)
}

/// Parse and analyze a SystemVerilog source with parameter overrides applied
/// to one module before generate elaboration.
pub fn analyze_source_with_module_parameter_overrides(
    code: &str,
    path: &Path,
    module_name: &str,
    parameter_overrides: &HashMap<String, i128>,
) -> Result<Ir, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    let source = ast::Source::from_syntax_with_module_parameter_overrides(
        &syntax_tree,
        module_name,
        parameter_overrides,
    )?;
    analyze::analyze_source(source)
}

/// Return the module names declared in a SystemVerilog source without
/// performing semantic lowering of their bodies or port declarations.
pub fn source_module_names(code: &str, path: &Path) -> Result<Vec<String>, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    ast::Source::module_names_from_syntax(&syntax_tree)
}

/// Return whether implicit nets are enabled when each module is declared.
#[doc(hidden)]
pub fn source_module_implicit_net_permissions(
    code: &str,
    path: &Path,
) -> Result<Vec<(String, bool)>, AnalyzerError> {
    syntax::source_module_implicit_net_permissions(code, path)
}

/// Analyze only one module from a source file, applying its parameter
/// overrides before generate elaboration.
pub fn analyze_source_module_with_parameter_overrides(
    code: &str,
    path: &Path,
    module_name: &str,
    parameter_overrides: &HashMap<String, i128>,
) -> Result<Ir, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    let source = ast::Source::from_syntax_module_with_parameter_overrides(
        &syntax_tree,
        module_name,
        parameter_overrides,
    )?;
    analyze::analyze_source(source)
}

/// Analyze only one module from a source file while preserving the literal
/// types of its parameter override expressions.
pub fn analyze_source_module_with_parameter_expr_overrides(
    code: &str,
    path: &Path,
    module_name: &str,
    parameter_overrides: &HashMap<String, ir::ConstExpr>,
) -> Result<Ir, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    let parameter_overrides = parameter_overrides
        .iter()
        .map(|(name, value)| (name.clone(), value.clone().into()))
        .collect();
    let source = ast::Source::from_syntax_module_with_parameter_expr_overrides(
        &syntax_tree,
        module_name,
        &parameter_overrides,
    )?;
    analyze::analyze_source(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_test_const_expr(expr: &ir::ConstExpr) -> Option<i128> {
        match expr {
            ir::ConstExpr::Literal(value) => value.parse().ok(),
            ir::ConstExpr::Binary { left, op, right } => {
                let left = eval_test_const_expr(left)?;
                let right = eval_test_const_expr(right)?;
                Some(match op {
                    ir::BinaryOp::Add => left + right,
                    ir::BinaryOp::Sub => left - right,
                    ir::BinaryOp::Mul => left * right,
                    ir::BinaryOp::Ge => (left >= right) as i128,
                    ir::BinaryOp::Le => (left <= right) as i128,
                    ir::BinaryOp::LogicAnd => ((left != 0) && (right != 0)) as i128,
                    ir::BinaryOp::LogicOr => ((left != 0) || (right != 0)) as i128,
                    _ => return None,
                })
            }
            ir::ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                if eval_test_const_expr(condition)? != 0 {
                    eval_test_const_expr(then_expr)
                } else {
                    eval_test_const_expr(else_expr)
                }
            }
            _ => None,
        }
    }

    #[test]
    fn keeps_case_item_guards_on_nested_comb_branches() {
        let ir = analyze_source(
            r#"
                module Top(input logic s, t, a, b, c, output logic y);
                    always_comb begin
                        case (s)
                            1'b0: if (t) y = a; else y = b;
                            default: y = c;
                        endcase
                    end
                endmodule
            "#,
            Path::new("nested_case.sv"),
        )
        .expect("SV analysis should succeed");

        let assignments = ir.modules()[0].comb_processes()[0].assignments();
        assert_eq!(assignments.len(), 1);
        let ir::Expr::Mux { else_expr, .. } = assignments[0].rhs() else {
            panic!("expected a multiplexer chain");
        };
        // The default branch value must remain the final fallback so that
        // `s != 0` selects `c`, not the nested else value.
        assert_eq!(expr_bottom_else(else_expr), "c");
    }

    fn expr_bottom_else(expr: &ir::Expr) -> String {
        match expr {
            ir::Expr::Mux { else_expr, .. } => expr_bottom_else(else_expr),
            ir::Expr::Ident(name) => name.clone(),
            other => panic!("unexpected expression in mux chain: {other:?}"),
        }
    }

    #[test]
    fn preserves_reads_between_merged_conditional_writes() {
        let ir = analyze_source(
            r#"
                module Top(input logic c, d, output logic x, y);
                    always_comb begin
                        x = d;
                        y = x;
                        if (c) x = 1'b1;
                    end
                endmodule
            "#,
            Path::new("intervening_read.sv"),
        )
        .expect("intervening read should use the value at its statement position");
        let assignments = ir.modules()[0].comb_processes()[0].assignments();
        let y = assignments
            .iter()
            .find(|assignment| assignment.lhs() == "y")
            .expect("y assignment");
        assert!(
            expr_references_ident_name(y.rhs(), "d"),
            "expected y to use the preceding d assignment: {:?}",
            y.rhs()
        );
        assert!(!expr_references_ident_name(y.rhs(), "x"));
    }

    #[test]
    fn snapshots_unconditional_sources_before_relocated_conditional_writes() {
        let ir = analyze_source(
            r#"
                module Top(input logic a, b, c, d, e, output logic x, y);
                    always_comb begin
                        y = a;
                        if (c) x = y;
                        else x = b;
                        y = d;
                        if (e) x = b;
                    end
                endmodule
            "#,
            Path::new("relocated_cross_target_read.sv"),
        )
        .expect("a relocated write should retain values read at its source position");
        let assignments = ir.modules()[0].comb_processes()[0].assignments();
        let x = assignments
            .iter()
            .find(|assignment| assignment.lhs() == "x")
            .expect("x assignment");
        assert!(expr_references_ident_name(x.rhs(), "a"));
        assert!(
            !expr_references_ident_name(x.rhs(), "y"),
            "x must snapshot y before its later overwrite: {:?}",
            x.rhs()
        );
    }

    #[test]
    fn preserves_fallback_guards_for_each_comb_target() {
        let ir = analyze_source(
            r#"
                module Top(input logic c, a, b, output logic x, y);
                    always_comb begin
                        x = 1'b0;
                        y = 1'b0;
                        if (c) x = a;
                        else y = b;
                    end
                endmodule
            "#,
            Path::new("target_fallback.sv"),
        )
        .expect("SV analysis should succeed");
        let assignments = ir.modules()[0].comb_processes()[0].assignments();
        let y = assignments
            .iter()
            .find(|assignment| assignment.lhs() == "y")
            .expect("y assignment");
        assert!(
            matches!(y.rhs(), ir::Expr::Mux { .. }),
            "the else write must not become globally unconditional: {:?}",
            y.rhs()
        );
    }

    #[test]
    fn keeps_writes_after_exhaustive_comb_fallbacks() {
        let ir = analyze_source(
            r#"
                module Top(input logic c, d, a, b, e, output logic x);
                    always_comb begin
                        if (c) x = a;
                        else x = b;
                        if (d) x = e;
                    end
                endmodule
            "#,
            Path::new("write_after_fallback.sv"),
        )
        .expect("SV analysis should succeed");
        let rhs = ir.modules()[0].comb_processes()[0].assignments()[0].rhs();
        let ir::Expr::Mux { condition, .. } = rhs else {
            panic!("expected the trailing write to produce a mux: {rhs:?}");
        };
        assert!(
            expr_references_ident_name(condition, "d"),
            "the trailing d write must retain priority: {rhs:?}"
        );
    }

    #[test]
    fn later_exhaustive_comb_chain_overrides_the_previous_chain() {
        let ir = analyze_source(
            r#"
                module Top(input logic c, d, a, b, e, f, output logic x);
                    always_comb begin
                        if (c) x = a;
                        else x = b;
                        if (d) x = e;
                        else x = f;
                    end
                endmodule
            "#,
            Path::new("consecutive_exhaustive_chains.sv"),
        )
        .expect("the later exhaustive chain should fully define x");
        let rhs = ir.modules()[0].comb_processes()[0].assignments()[0].rhs();
        assert!(expr_references_ident_name(rhs, "d"));
        assert!(expr_references_ident_name(rhs, "e"));
        assert!(expr_references_ident_name(rhs, "f"));
        assert!(
            !expr_references_ident_name(rhs, "c")
                && !expr_references_ident_name(rhs, "a")
                && !expr_references_ident_name(rhs, "b"),
            "the fully overriding second chain must discard the first chain: {rhs:?}"
        );
    }

    #[test]
    fn recognizes_complementary_equality_guards_as_exhaustive() {
        analyze_source(
            r#"
                module Top(input logic outer, input bit s, input logic a, b, c, output logic y);
                    always_comb begin
                        if (outer) begin
                            if (s == 0) y = a;
                            if (s != 0) y = b;
                        end else begin
                            y = c;
                        end
                    end
                endmodule
            "#,
            Path::new("complementary_equality_guards.sv"),
        )
        .expect("complementary two-state equality guards should define y exhaustively");
    }

    #[test]
    fn recognizes_block_wrapped_complementary_guards_as_exhaustive() {
        analyze_source(
            r#"
                module Top(input logic outer, input bit s, input logic a, b, c, output logic y);
                    always_comb begin
                        if (outer) begin
                            if (s) begin y = a; end
                            if (!s) begin y = b; end
                        end else begin
                            y = c;
                        end
                    end
                endmodule
            "#,
            Path::new("block_wrapped_complementary_guards.sv"),
        )
        .expect("block-wrapped complementary guards should define y exhaustively");
    }

    #[test]
    fn preserves_complementary_guards_across_harmless_blocks() {
        analyze_source(
            r#"
                module Top(input logic outer, input bit s, input logic a, b, output logic y, z);
                    always_comb begin
                        if (outer) begin
                            if (s) y = a;
                            begin z = 1'b0; end
                            if (!s) y = b;
                        end else begin
                            y = a;
                            z = 1'b1;
                        end
                    end
                endmodule
            "#,
            Path::new("harmless_block_between_complementary_guards.sv"),
        )
        .expect("a block that cannot change the guard should preserve its proof");
    }

    #[test]
    fn invalidates_complementary_guards_for_overlapping_selected_writes() {
        let error = analyze_source(
            r#"
                module Top(
                    input logic outer, q, a, b, c,
                    input bit idx,
                    output logic y
                );
                    logic [1:0] s;
                    always_comb begin
                        s = {q, q};
                        if (outer) begin
                            if (s[0]) y = a;
                            s[idx] = b;
                            if (!s[0]) y = c;
                        end else begin
                            y = a;
                        end
                    end
                endmodule
            "#,
            Path::new("overlapping_write_between_complementary_guards.sv"),
        )
        .expect_err("a dynamic overlapping write must invalidate the guard proof");
        assert!(
            error
                .to_string()
                .contains("latch inference inside always_comb")
        );

        let error = analyze_source(
            r#"
                module Top(input logic outer, q, a, b, output logic y);
                    logic s;
                    function automatic bit f();
                        return s;
                    endfunction
                    always_comb begin
                        s = q;
                        if (outer) begin
                            if (f()) y = a;
                            s = 1'b1;
                            if (!f()) y = b;
                        end else begin
                            y = a;
                        end
                    end
                endmodule
            "#,
            Path::new("function_guard_dependency_write.sv"),
        )
        .expect_err("a function guard's free-variable write must invalidate its proof");
        assert!(
            error
                .to_string()
                .contains("latch inference inside always_comb")
        );
    }

    #[test]
    fn substitutes_reads_of_selected_comb_targets() {
        let ir = analyze_source(
            r#"
                module Top(input logic c, a, b, output logic [1:0] x, output logic y);
                    always_comb begin
                        x[0] = a;
                        y = x[0];
                        if (c) x[0] = b;
                    end
                endmodule
            "#,
            Path::new("selected_intervening_read.sv"),
        )
        .expect("SV analysis should succeed");
        let assignments = ir.modules()[0].comb_processes()[0].assignments();
        let y = assignments
            .iter()
            .find(|assignment| assignment.lhs() == "y")
            .expect("y assignment");
        assert!(expr_references_ident_name(y.rhs(), "a"));
        assert!(
            !expr_references_ident_name(y.rhs(), "x"),
            "y must observe the preceding selected write: {:?}",
            y.rhs()
        );
    }

    #[test]
    fn substitutes_subselect_reads_of_selected_comb_targets() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic c,
                    input logic [2:0] a, b,
                    output logic [3:0] x,
                    output logic y
                );
                    always_comb begin
                        x[3:1] = a;
                        y = x[2];
                        if (c) x[3:1] = b;
                    end
                endmodule
            "#,
            Path::new("selected_subselect_intervening_read.sv"),
        )
        .expect("SV analysis should succeed");
        let assignments = ir.modules()[0].comb_processes()[0].assignments();
        let y = assignments
            .iter()
            .find(|assignment| assignment.lhs() == "y")
            .expect("y assignment");
        assert!(expr_references_ident_name(y.rhs(), "a"));
        let ir::Expr::Select { msb, lsb, .. } = y.rhs() else {
            panic!(
                "expected the intervening bit read to select from a: {:?}",
                y.rhs()
            );
        };
        assert_eq!(msb, &ir::ConstExpr::Literal("1".to_string()));
        assert_eq!(lsb, &ir::ConstExpr::Literal("1".to_string()));
        assert!(
            !expr_references_ident_name(y.rhs(), "x"),
            "y must observe the matching bit of the preceding selected write: {:?}",
            y.rhs()
        );
    }

    #[test]
    fn substitutes_partially_overlapping_reads_of_selected_comb_targets() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic c,
                    input logic [2:0] a, b,
                    output logic [4:0] x,
                    output logic [2:0] y
                );
                    always_comb begin
                        x[3:1] = a;
                        y = x[4:2];
                        if (c) x[3:1] = b;
                    end
                endmodule
            "#,
            Path::new("selected_partial_overlap_intervening_read.sv"),
        )
        .expect("SV analysis should succeed");
        let assignments = ir.modules()[0].comb_processes()[0].assignments();
        let y = assignments
            .iter()
            .find(|assignment| assignment.lhs() == "y")
            .expect("y assignment");
        assert!(
            expr_references_ident_name(y.rhs(), "a"),
            "the overlapping bits must come from the preceding selected write: {:?}",
            y.rhs()
        );
        assert!(
            expr_references_ident_name(y.rhs(), "x"),
            "the non-overlapping bit must retain its original source: {:?}",
            y.rhs()
        );
    }

    #[test]
    fn coerces_always_comb_if_predicates_to_procedural_truth() {
        let ir = analyze_source(
            r#"
                module Top(input logic s, output logic y);
                    always_comb begin
                        if (s) y = 1'b1;
                        else y = 1'b0;
                    end
                endmodule
            "#,
            Path::new("always_comb_procedural_truth.sv"),
        )
        .expect("SV analysis should succeed");
        let rhs = ir.modules()[0].comb_processes()[0].assignments()[0].rhs();
        let ir::Expr::Mux { condition, .. } = rhs else {
            panic!("expected conditional assignment mux: {rhs:?}");
        };
        assert!(matches!(
            &**condition,
            ir::Expr::Unary {
                op: ir::UnaryOp::RedOr,
                expr,
            } if matches!(
                &**expr,
                ir::Expr::Unary {
                    op: ir::UnaryOp::ToTwoState,
                    ..
                }
            )
        ));
    }

    #[test]
    fn applies_cross_target_substitutions_before_merging_comb_groups() {
        let ir = analyze_source(
            r#"
                module Top(input logic c, d, output logic x, y);
                    always_comb begin
                        y = 1'b0;
                        x = 1'b0;
                        y = x;
                        if (c) x = 1'b1;
                        if (d) y = 1'b1;
                    end
                endmodule
            "#,
            Path::new("cross_target_substitution.sv"),
        )
        .expect("SV analysis should succeed");
        let y = ir.modules()[0].comb_processes()[0]
            .assignments()
            .iter()
            .find(|assignment| assignment.lhs() == "y")
            .expect("y assignment");
        assert!(
            !expr_references_ident_name(y.rhs(), "x"),
            "y must use x's value at the intervening statement: {:?}",
            y.rhs()
        );
    }

    #[test]
    fn uses_whole_vector_defaults_for_conditional_selected_writes() {
        let ir = analyze_source(
            r#"
                module Top(input logic c, output logic [1:0] x);
                    always_comb begin
                        x = '0;
                        if (c) x[0] = 1'b1;
                    end
                endmodule
            "#,
            Path::new("whole_then_selected.sv"),
        )
        .expect("whole-vector initialization should cover the selected fallback");
        assert_eq!(ir.modules()[0].comb_processes()[0].assignments().len(), 2);
    }

    #[test]
    fn uses_selected_writes_before_conditional_whole_vector_writes() {
        analyze_source(
            r#"
                module Top(input logic c, a, output logic [7:0] x);
                    always_comb begin
                        x = '0;
                        x[0] = a;
                        if (c) x = 8'hff;
                    end
                endmodule
            "#,
            Path::new("selected_then_conditional_whole.sv"),
        )
        .expect("the preceding selected write should initialize the whole-write fallback");
    }

    #[test]
    fn permits_reads_after_assignments_on_the_same_comb_path() {
        analyze_source(
            r#"
                module Top(input logic c, output logic x, y);
                    always_comb begin
                        if (c) begin
                            x = 1'b1;
                            y = x;
                        end else begin
                            x = 1'b0;
                            y = x;
                        end
                    end
                endmodule
            "#,
            Path::new("path_local_comb_read.sv"),
        )
        .expect("each guarded read is preceded by a write on the same path");
    }

    #[test]
    fn freezes_comb_branch_guards_before_overwriting_the_predicate() {
        let ir = analyze_source(
            r#"
                module Top(input logic en, output logic t, y);
                    always_comb begin
                        t = en;
                        y = 1'b0;
                        if (t) begin
                            t = 1'b0;
                            y = 1'b1;
                        end
                    end
                endmodule
            "#,
            Path::new("frozen_comb_guard.sv"),
        )
        .expect("the branch predicate should use t's value on entry");
        let y = ir.modules()[0].comb_processes()[0]
            .assignments()
            .iter()
            .find(|assignment| assignment.lhs() == "y")
            .expect("y assignment");
        assert!(expr_references_ident_name(y.rhs(), "en"));
        assert!(!expr_references_ident_name(y.rhs(), "t"));
    }

    #[test]
    fn rejects_entry_value_guards_relocated_past_their_first_write() {
        let error = analyze_source(
            r#"
                module Top(
                    input logic a, b, d, e,
                    output logic t, x
                );
                    always_comb begin
                        if (t) x = a;
                        else x = b;
                        t = d;
                        if (e) x = b;
                    end
                endmodule
            "#,
            Path::new("relocated_entry_guard.sv"),
        )
        .expect_err("the entry value of t cannot be moved past t's first write");
        assert!(
            error
                .to_string()
                .contains("read-before-write dependency inside always_comb"),
            "unexpected error: {error}"
        );

        let error = analyze_source(
            r#"
                module Top(
                    input logic a, b, c, d, e, f,
                    output logic y,
                    output logic [1:0] x
                );
                    always_comb begin
                        y = 1'b0;
                        if (x) y = a;
                        x[0] = c;
                        if (d) x[0] = e;
                        if (f) y = b;
                    end
                endmodule
            "#,
            Path::new("relocated_overlapping_entry_guard.sv"),
        )
        .expect_err("a whole-vector guard read cannot move past a selected write");
        assert!(
            error
                .to_string()
                .contains("read-before-write dependency inside always_comb"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn preserves_prior_partially_overlapping_selected_writes() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic c, d,
                    input logic [2:0] a, b,
                    output logic [3:0] x
                );
                    always_comb begin
                        x = '0;
                        if (c) x[3:1] = a;
                        if (d) x[2:0] = b;
                    end
                endmodule
            "#,
            Path::new("overlapping_selected_fallback.sv"),
        )
        .expect("overlapping selected writes should preserve their procedural order");
        let assignments = ir.modules()[0].comb_processes()[0].assignments();
        let rhs = assignments.last().expect("final selected assignment").rhs();
        assert!(
            expr_references_ident_name(rhs, "a"),
            "the later false path must retain the earlier selected value: {rhs:?}"
        );
    }

    #[test]
    fn requires_definite_assignment_before_filling_comb_fallbacks() {
        let error = analyze_source(
            r#"
                module Top(input logic c, d, a, b, output logic x);
                    always_comb begin
                        if (c) x = a;
                        else if (d) x = b;
                    end
                endmodule
            "#,
            Path::new("nested_incomplete_fallback.sv"),
        )
        .expect_err("the incomplete nested fallback must infer a latch")
        .to_string();
        assert!(error.contains("latch inference inside always_comb"));
    }

    #[test]
    fn rejects_genuine_self_reads_in_exhaustive_comb_branches() {
        let error = analyze_source(
            r#"
                module Top(input logic c, output logic [7:0] x);
                    always_comb begin
                        if (c) x = x + 1;
                        else x = 0;
                    end
                endmodule
            "#,
            Path::new("genuine_self_read.sv"),
        )
        .expect_err("a genuine self-read must not be filled as a fallback hole")
        .to_string();
        assert!(error.contains("latch inference inside always_comb"));
    }

    #[test]
    fn rejects_overlapping_selected_self_reads() {
        let error = analyze_source(
            r#"
                module Top(input logic c, output logic [1:0] x);
                    always_comb begin
                        if (c) x[0] = x[1:0];
                        else x[0] = 1'b0;
                    end
                endmodule
            "#,
            Path::new("overlapping_selected_self_read.sv"),
        )
        .expect_err("an overlapping selected self-read must be rejected")
        .to_string();
        assert!(error.contains("latch inference inside always_comb"));
    }

    #[test]
    fn sign_extends_negative_literals_in_widening_constant_casts() {
        let ir = analyze_source(
            r#"
                module Top #(parameter V = $bits(logic signed [7:0])'(4'shf)) ();
                endmodule
            "#,
            Path::new("sign_extend_cast.sv"),
        )
        .expect("SV analysis should succeed");
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(-1));
    }

    #[test]
    fn sizes_first_dimension_for_size_cast_targets() {
        let ir = analyze_source(
            r#"
                module Top #(
                    parameter W = $size(logic [1:0][3:0])'(3'd7),
                    parameter B = $bits(logic [1:0][3:0])'(4'd7)
                ) ();
                endmodule
            "#,
            Path::new("size_cast.sv"),
        )
        .expect("SV analysis should succeed");
        // A 2-bit $size target truncates 7 to 3; an 8-bit $bits target
        // keeps it.
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(3));
        assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(7));
    }

    #[test]
    fn infers_size_cast_targets_from_selected_expressions() {
        let ir = analyze_source(
            r#"
                module Top(input logic [7:0] a);
                    localparam Q = $bits(a[3:0])'(4'hf);
                    localparam S = $size(a[3:0])'(4'hf);
                endmodule
            "#,
            Path::new("selected_expression_size_cast.sv"),
        )
        .expect("selected expression types should determine size cast widths");
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(15));
        assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(15));
    }

    #[test]
    fn treats_scalar_size_cast_targets_as_one_bit() {
        let ir = analyze_source(
            r#"
                module Top #(parameter W = $size(logic)'(2'd3)) ();
                endmodule
            "#,
            Path::new("scalar_size_cast.sv"),
        )
        .expect("a scalar $size cast target should resolve to one bit");
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(1));
    }

    #[test]
    fn resolves_constant_cast_targets_from_module_environments() {
        let alias_ir = analyze_source(
            r#"
                module Top;
                    typedef logic [7:0] byte_t;
                    localparam P = byte_t'(4'd3);
                endmodule
            "#,
            Path::new("constant_alias_cast_env.sv"),
        )
        .expect("typedef cast target should resolve");
        assert_eq!(
            alias_ir.modules()[0].parameters()[0].resolved_value(),
            Some(3)
        );

        let width_ir = analyze_source(
            r#"
                module Top;
                    localparam W = 8;
                    localparam Q = W'(4'd3);
                endmodule
            "#,
            Path::new("constant_width_cast_env.sv"),
        )
        .expect("parameter-sized cast target should resolve");
        assert_eq!(
            width_ir.modules()[0].parameters()[1].resolved_value(),
            Some(3)
        );
    }

    #[test]
    fn evaluates_constant_cast_operand_expressions() {
        let ir = analyze_source(
            r#"
                module Top;
                    typedef logic [7:0] byte_t;
                    localparam A = 3;
                    localparam B = byte_t'(A);
                    localparam C = byte_t'(1 + 2);
                endmodule
            "#,
            Path::new("constant_cast_operands.sv"),
        )
        .expect("constant cast operands should be evaluated in the module environment");
        assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(3));
        assert_eq!(ir.modules()[0].parameters()[2].resolved_value(), Some(3));
    }

    #[test]
    fn resolves_enum_members_referencing_earlier_members() {
        let ir = analyze_source(
            r#"
                module Top(input logic [1:0] sel, output logic y);
                    typedef enum logic [1:0] { A = 2'd0, B = A + 2'd1 } E;
                    always_comb y = (sel == B);
                endmodule
            "#,
            Path::new("enum_member_ref.sv"),
        )
        .expect("SV analysis should succeed");

        let assignments = ir.modules()[0].comb_processes()[0].assignments();
        assert_eq!(assignments.len(), 1);
        // `B` must resolve to its constant value even though it references
        // the earlier member `A`.
        assert!(
            !expr_references_ident_name(assignments[0].rhs(), "B"),
            "unresolved enum member in {:?}",
            assignments[0].rhs()
        );
        assert!(
            expr_contains_literal(assignments[0].rhs(), "1"),
            "expected the folded member value in {:?}",
            assignments[0].rhs()
        );
    }

    #[test]
    fn context_sizes_unbased_enum_member_initializers() {
        let ir = analyze_source(
            r#"
                module Top;
                    typedef enum logic [3:0] { A = '1 } E;
                    logic [A:0] data;
                endmodule
            "#,
            Path::new("unbased_enum_initializer.sv"),
        )
        .expect("an unbased fill should be sized to the enum base type");
        let width = ir.modules()[0]
            .signals()
            .iter()
            .find(|signal| signal.name() == "data")
            .and_then(|signal| signal.r#type().resolved_width());
        assert_eq!(width, Some(16));
    }

    #[test]
    fn preserves_enum_base_types_during_constant_substitution() {
        let ir = analyze_source(
            r#"
                module Top(output logic [31:0] y);
                    typedef enum logic [1:0] { A = 2'd0 } E;
                    assign y = ~A;
                endmodule
            "#,
            Path::new("typed_enum_constant.sv"),
        )
        .expect("SV analysis should succeed");
        let rhs = ir.modules()[0].assignments()[0].rhs();
        let ir::Expr::Unary { expr, .. } = rhs else {
            panic!("expected enum complement: {rhs:?}");
        };
        assert_eq!(&**expr, &ir::Expr::Literal("2'd0".to_string()));
    }

    #[test]
    fn rejects_casez_nested_under_comb_conditionals() {
        let error = analyze_source(
            r#"
                module Top(input logic en, sel, output logic y);
                    always_comb begin
                        y = 1'b0;
                        if (en) casez (sel)
                            1'b?: y = 1'b1;
                        endcase
                    end
                endmodule
            "#,
            Path::new("nested_casez.sv"),
        )
        .expect_err("nested casez must be rejected")
        .to_string();
        assert!(
            error.contains("casez or casex inside always_comb"),
            "unexpected error: {error}"
        );
    }

    fn expr_references_ident_name(expr: &ir::Expr, name: &str) -> bool {
        match expr {
            ir::Expr::Ident(ident) => ident == name,
            ir::Expr::Select { expr, .. } => expr_references_ident_name(expr, name),
            ir::Expr::Concat(parts) | ir::Expr::RepeatConcat { parts, .. } => parts
                .iter()
                .any(|part| expr_references_ident_name(part, name)),
            ir::Expr::Resize { expr, .. } | ir::Expr::Unary { expr, .. } => {
                expr_references_ident_name(expr, name)
            }
            ir::Expr::Call { args, .. } => {
                args.iter().any(|arg| expr_references_ident_name(arg, name))
            }
            ir::Expr::Binary { left, right, .. } => {
                expr_references_ident_name(left, name) || expr_references_ident_name(right, name)
            }
            ir::Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                expr_references_ident_name(condition, name)
                    || expr_references_ident_name(then_expr, name)
                    || expr_references_ident_name(else_expr, name)
            }
            ir::Expr::Literal(_) => false,
        }
    }

    fn expr_contains_literal(expr: &ir::Expr, needle: &str) -> bool {
        match expr {
            ir::Expr::Literal(value) => value == needle || value.ends_with(&format!("d{needle}")),
            ir::Expr::Select { expr, .. } => expr_contains_literal(expr, needle),
            ir::Expr::Concat(parts) | ir::Expr::RepeatConcat { parts, .. } => {
                parts.iter().any(|part| expr_contains_literal(part, needle))
            }
            ir::Expr::Resize { expr, .. } | ir::Expr::Unary { expr, .. } => {
                expr_contains_literal(expr, needle)
            }
            ir::Expr::Call { args, .. } => {
                args.iter().any(|arg| expr_contains_literal(arg, needle))
            }
            ir::Expr::Binary { left, right, .. } => {
                expr_contains_literal(left, needle) || expr_contains_literal(right, needle)
            }
            ir::Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                expr_contains_literal(condition, needle)
                    || expr_contains_literal(then_expr, needle)
                    || expr_contains_literal(else_expr, needle)
            }
            ir::Expr::Ident(_) => false,
        }
    }

    #[test]
    fn uses_enum_members_as_module_constants() {
        let ir = analyze_source(
            r#"
                module Top #(
                    parameter logic [1:0] BASE = 2'd1
                ) (input logic a, output logic y);
                    typedef enum logic [1:0] { N = BASE + 2'd1 } E;
                    logic [N-1:0] data;
                    always_comb begin
                        if (N != 0) y = a;
                        else y = 1'b0;
                    end
                endmodule
            "#,
            Path::new("enum_const_env.sv"),
        )
        .expect("SV analysis should succeed");
        let width = ir.modules()[0]
            .signals()
            .iter()
            .find(|signal| signal.name() == "data")
            .map(|signal| signal.r#type().resolved_width())
            .unwrap();
        assert_eq!(width, Some(2));
    }

    #[test]
    fn resolves_parameters_that_reference_enum_members() {
        let ir = analyze_source(
            r#"
                module Top;
                    typedef enum logic [1:0] { N = 2 } E;
                    localparam W = N;
                    logic [W-1:0] data;
                endmodule
            "#,
            Path::new("enum_parameter_dependency.sv"),
        )
        .expect("parameters should be re-resolved after enum collection");
        let width = ir.modules()[0]
            .signals()
            .iter()
            .find(|signal| signal.name() == "data")
            .and_then(|signal| signal.r#type().resolved_width());
        assert_eq!(width, Some(2));
    }

    #[test]
    fn re_resolves_parameters_between_enum_declarations() {
        let ir = analyze_source(
            r#"
                module Top;
                    typedef enum logic [1:0] { A = 2 } E0;
                    localparam W = A + 1;
                    typedef enum logic [3:0] { B = W } E1;
                    logic [B-1:0] data;
                endmodule
            "#,
            Path::new("enum_parameter_enum_dependency.sv"),
        )
        .expect("parameters between enum declarations should be available to later enums");
        let width = ir.modules()[0]
            .signals()
            .iter()
            .find(|signal| signal.name() == "data")
            .and_then(|signal| signal.r#type().resolved_width());
        assert_eq!(width, Some(3));
    }

    #[test]
    fn rebuilds_typedefs_after_preceding_enum_members() {
        let ir = analyze_source(
            r#"
                module Top;
                    typedef enum int { W = 3 } E;
                    typedef logic [W'(2):0] B;
                    typedef enum B { A = 3'b101 } F;
                    logic [A-1:0] data;
                endmodule
            "#,
            Path::new("enum_dependent_typedef.sv"),
        )
        .expect("later enum bases should use typedefs rebuilt from preceding members");
        let width = ir.modules()[0]
            .signals()
            .iter()
            .find(|signal| signal.name() == "data")
            .and_then(|signal| signal.r#type().resolved_width());
        assert_eq!(width, Some(5));
    }

    #[test]
    fn treats_constant_equivalent_mux_case_selectors_as_two_state() {
        analyze_source(
            r#"
                module Top(input logic c, a, output logic y);
                    localparam logic P = 0;
                    localparam logic Q = 0;
                    always_comb begin
                        case (c ? P : Q)
                            1'b0: y = a;
                        endcase
                    end
                endmodule
            "#,
            Path::new("constant_equivalent_mux_case.sv"),
        )
        .expect("equal constant mux arms should make the case selector exhaustive");
    }

    #[test]
    fn skips_unreachable_sparse_case_items_with_default() {
        analyze_source(
            r#"
                module Top(input bit [1:0] s, input logic a, b, output logic y);
                    always_comb begin
                        case (s)
                            2'd0: y = a;
                            2'd0: ;
                            default: y = b;
                        endcase
                    end
                endmodule
            "#,
            Path::new("sparse_case_duplicate_with_default.sv"),
        )
        .expect("unreachable duplicate items should not prevent definite assignment");
    }

    #[test]
    fn skips_duplicate_case_items_for_four_state_selectors() {
        analyze_source(
            r#"
                module Top(input logic s, input logic a, b, output logic y);
                    always_comb begin
                        case (s)
                            1'b0: y = a;
                            1'b0: ;
                            default: y = b;
                        endcase
                    end
                endmodule
            "#,
            Path::new("four_state_duplicate_case_item.sv"),
        )
        .expect("an unreachable duplicate case item should not infer a latch");
    }

    #[test]
    fn compares_normalized_four_state_case_labels_for_reachability() {
        analyze_source(
            r#"
                module Top(input logic [1:0] s, input logic a, b, output logic y);
                    always_comb begin
                        case (s)
                            2'd0: y = a;
                            2'b00: ;
                            default: y = b;
                        endcase
                    end
                endmodule
            "#,
            Path::new("normalized_four_state_case_labels.sv"),
        )
        .expect("equivalent case-label values should make the later item unreachable");

        analyze_source(
            r#"
                module Top(input logic [8:0] s, input logic a, b, output logic y);
                    always_comb begin
                        case (s)
                            9'd0: y = a;
                            9'b000000000: ;
                            default: y = b;
                        endcase
                    end
                endmodule
            "#,
            Path::new("wide_normalized_four_state_case_labels.sv"),
        )
        .expect("equivalent wide labels should be normalized without domain enumeration");

        analyze_source(
            r#"
                module Top(input logic signed [8:0] s, input logic a, b, output logic y);
                    always_comb begin
                        case (s)
                            1'sb1: y = a;
                            9'b111111111: ;
                            default: y = b;
                        endcase
                    end
                endmodule
            "#,
            Path::new("selector_typed_four_state_case_labels.sv"),
        )
        .expect("labels should be normalized in the signed selector context");

        let error = analyze_source(
            r#"
                module Top(input logic [8:0] s, input logic a, b, output logic y);
                    always_comb begin
                        case (s)
                            10'h3ff: y = a;
                            9'h1ff: ;
                            default: y = b;
                        endcase
                    end
                endmodule
            "#,
            Path::new("wider_unreachable_case_label.sv"),
        )
        .expect_err("a wider unreachable label must not hide a reachable empty item");
        assert!(
            error
                .to_string()
                .contains("latch inference inside always_comb")
        );
    }

    #[test]
    fn folds_compound_four_state_constant_case_selectors_for_coverage() {
        analyze_source(
            r#"
                module Top(input logic c, a, b, output logic y);
                    always_comb begin
                        case (1'bx | 1'b0)
                            1'bx: if (c) y = a; else y = b;
                        endcase
                    end
                endmodule
            "#,
            Path::new("compound_four_state_constant_case_selector.sv"),
        )
        .expect("a compound constant X selector should preserve its mask for coverage");
    }

    #[test]
    fn expands_constant_function_predicates_for_definite_assignments() {
        analyze_source(
            r#"
                module Top(input logic outer, a, b, output logic y);
                    function automatic bit one();
                        return 1'b1;
                    endfunction
                    always_comb begin
                        if (outer) begin
                            if (one()) y = a;
                        end else begin
                            y = b;
                        end
                    end
                endmodule
            "#,
            Path::new("constant_function_definite_assignment.sv"),
        )
        .expect("a constant-true function predicate should make the inner write definite");
    }

    #[test]
    fn folds_concatenated_four_state_constant_case_selectors_for_coverage() {
        analyze_source(
            r#"
                module Top(input logic a, output logic y);
                    always_comb begin
                        case ({1'bx})
                            1'bx: y = a;
                        endcase
                    end
                endmodule
            "#,
            Path::new("concatenated_four_state_constant_case_selector.sv"),
        )
        .expect("a constant concatenation should retain its X mask for case coverage");
    }

    #[test]
    fn recognizes_mixed_boolean_and_zero_equality_complements() {
        analyze_source(
            r#"
                module Top(input logic outer, input bit s, input logic a, b, c, output logic y);
                    always_comb begin
                        if (outer) begin
                            if (s) y = a;
                            if (s == 0) y = b;
                        end else begin
                            y = c;
                        end
                    end
                endmodule
            "#,
            Path::new("mixed_boolean_equality_complements.sv"),
        )
        .expect("a two-state predicate and its zero equality should be complementary");
    }

    #[test]
    fn resolves_function_types_in_size_cast_targets() {
        let ir = analyze_source(
            r#"
                module Top(output logic [7:0] y);
                    function automatic logic [7:0] f();
                        return 8'h00;
                    endfunction
                    localparam P = $bits(f())'(16'hffff);
                    always_comb y = P;
                endmodule
            "#,
            Path::new("function_size_cast_target.sv"),
        )
        .expect("a size-function cast target should use the function return type");
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0xff));
    }

    #[test]
    fn restricts_enum_alias_types_to_the_declared_base() {
        let ir = analyze_source(
            r#"
                module Top(output E y);
                    typedef enum logic { A = int'(1) } E;
                    always_comb y = A;
                endmodule
            "#,
            Path::new("enum_member_cast_type_is_not_base.sv"),
        )
        .expect("types in enum member initializers must not replace the enum base");
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(1)
        );
        assert!(!ir.modules()[0].ports()[0].r#type().is_signed());
    }

    #[test]
    fn recognizes_complete_four_state_cases() {
        analyze_source(
            r#"
                module Top(input logic s, input logic a, output logic y);
                    always_comb begin
                        case (s)
                            1'b0: y = a;
                            1'b1: y = a;
                            1'bx: y = a;
                            1'bz: y = a;
                        endcase
                    end
                endmodule
            "#,
            Path::new("complete_four_state_case.sv"),
        )
        .expect("all four states should exhaust a one-bit logic selector");
    }

    #[test]
    fn recognizes_exhaustive_constant_four_state_cases() {
        analyze_source(
            r#"
                module Top(input logic c, a, b, output logic y);
                    always_comb begin
                        case (1'bx)
                            1'bx: if (c) y = a; else y = b;
                        endcase
                    end
                endmodule
            "#,
            Path::new("constant_four_state_case_selector.sv"),
        )
        .expect("a matching X-valued constant case item should be exhaustive");
    }

    #[test]
    fn expands_constant_function_case_selectors_for_coverage() {
        analyze_source(
            r#"
                module Top(input logic a, output logic y);
                    function automatic bit f();
                        return 1'b0;
                    endfunction
                    always_comb begin
                        case (f())
                            1'b0: y = a;
                        endcase
                    end
                endmodule
            "#,
            Path::new("constant_function_case_selector.sv"),
        )
        .expect("a constant function selector should make its matching item exhaustive");

        analyze_source(
            r#"
                module Top(input logic a, output logic y);
                    function automatic logic f();
                        return 1'bx;
                    endfunction
                    always_comb begin
                        case (f())
                            1'bx: y = a;
                        endcase
                    end
                endmodule
            "#,
            Path::new("constant_unknown_function_case_selector.sv"),
        )
        .expect("an X-valued constant function selector should preserve its mask");
    }

    #[test]
    fn expands_function_calls_in_procedural_lvalue_indices() {
        analyze_source(
            r#"
                module Top(input bit index, input logic data, output logic [1:0] x);
                    function automatic bit idx();
                        return index;
                    endfunction
                    always_comb begin
                        x = '0;
                        x[idx()] = data;
                    end
                endmodule
            "#,
            Path::new("function_lvalue_index.sv"),
        )
        .expect("a supported function call in an lvalue index should be expanded");
    }

    #[test]
    fn resolves_value_dependent_type_parameter_defaults() {
        let ir = analyze_source(
            r#"
                module Top #(
                    parameter W = 8,
                    parameter type T = logic [W'(7):0]
                ) (output T y);
                    always_comb y = 8'hff;
                endmodule
            "#,
            Path::new("value_dependent_type_parameter.sv"),
        )
        .expect("a type parameter default should use preceding value parameters");
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
        );
    }

    #[test]
    fn caps_dynamic_select_normalization_expansion() {
        let error = analyze_source(
            r#"
                module Top(
                    input logic [16:0] index,
                    input logic data, replace,
                    output logic [4096:0] value
                );
                    always_comb begin
                        value = '0;
                        value[index] = data;
                        if (replace) value = '1;
                    end
                endmodule
            "#,
            Path::new("capped_dynamic_select_expansion.sv"),
        )
        .expect_err("oversized dynamic-select expansion should be rejected compactly");
        assert!(
            error
                .to_string()
                .contains("dynamic selected write expansion exceeds limit")
        );
    }

    #[test]
    fn retains_zero_iteration_loop_writes_for_latch_detection() {
        let error = analyze_source(
            r#"
                module Top(input logic a, output logic y);
                    always_comb
                        for (int i = 0; i < 0; i++) y = a;
                endmodule
            "#,
            Path::new("zero_iteration_comb_loop.sv"),
        )
        .expect_err("a zero-iteration loop must not silently discard its target");
        assert!(
            error
                .to_string()
                .contains("latch inference inside always_comb")
        );

        let error = analyze_source(
            r#"
                module Top(input logic enable, a, output logic y);
                    always_comb begin
                        if (enable)
                            for (int i = 0; i < 0; i++) y = a;
                    end
                endmodule
            "#,
            Path::new("nested_zero_iteration_comb_loop.sv"),
        )
        .expect_err("a nested zero-iteration loop must retain its write target");
        assert!(
            error
                .to_string()
                .contains("latch inference inside always_comb")
        );

        analyze_source(
            r#"
                module Top(input logic a, output logic y);
                    always_comb begin
                        y = 1'b0;
                        for (int i = 0; i < 0; i++) y = a;
                    end
                endmodule
            "#,
            Path::new("initialized_zero_iteration_comb_loop.sv"),
        )
        .expect("a preceding assignment should initialize a zero-iteration loop target");
    }

    #[test]
    fn materializes_static_loop_indices_in_definite_write_targets() {
        analyze_source(
            r#"
                module Top(input logic c, a, b, output logic [1:0] x);
                    always_comb begin
                        if (c) begin
                            x[0] = a;
                            x[1] = a;
                        end else begin
                            for (int i = 0; i < 2; i++) x[i] = b;
                        end
                    end
                endmodule
            "#,
            Path::new("materialized_definite_loop_targets.sv"),
        )
        .expect("each static loop iteration should contribute its concrete target");
    }

    #[test]
    fn preserves_named_constant_casts_in_packed_ranges() {
        let ir = analyze_source(
            r#"
                module Top #(
                    parameter W = 8
                ) (
                    output logic [W'(15):0] y
                );
                endmodule
            "#,
            Path::new("named_cast_packed_range.sv"),
        )
        .expect("named constant casts should lower with the module environment");
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(16)
        );
    }

    #[test]
    fn resolves_named_casts_while_collecting_parameter_ranges() {
        let ir = analyze_source(
            r#"
                module Top #(
                    parameter W = 3,
                    parameter logic [W'(2):0] P = 3'b100
                ) ();
                endmodule
            "#,
            Path::new("named_cast_parameter_range.sv"),
        )
        .expect("parameter ranges should use preceding parameters in named casts");
        let parameter = &ir.modules()[0].parameters()[1];
        assert_eq!(parameter.declared_width(), Some(3));
        assert_eq!(parameter.resolved_value(), Some(4));
    }

    #[test]
    fn preserves_named_constant_casts_in_unpacked_ranges() {
        let ir = analyze_source(
            r#"
                module Top #(
                    parameter W = 2
                ) (
                    output logic y [W'(1):0]
                );
                endmodule
            "#,
            Path::new("named_cast_unpacked_range.sv"),
        )
        .expect("named constant casts in unpacked dimensions should use the module environment");
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(2)
        );
    }

    #[test]
    fn preserves_operand_signedness_for_named_numeric_size_casts() {
        let ir = analyze_source(
            r#"
                module Top;
                    localparam W = 8;
                    localparam P = W'(4'shf);
                    localparam Q = 16'(P);
                endmodule
            "#,
            Path::new("named_numeric_size_cast.sv"),
        )
        .expect("named numeric size casts should preserve operand signedness");
        let parameters = ir.modules()[0].parameters();
        assert_eq!(parameters[1].resolved_value(), Some(-1));
        assert_eq!(parameters[1].resolved_signed(), Some(true));
        assert_eq!(parameters[2].resolved_value(), Some(-1));
    }

    #[test]
    fn applies_typedef_signedness_to_constant_primary_cast_targets() {
        let ir = analyze_source(
            r#"
                module Top;
                    typedef logic signed [7:0] S;
                    localparam P = S'(8'hff);
                endmodule
            "#,
            Path::new("constant_primary_typedef_cast.sv"),
        )
        .expect("a typedef cast should use the typedef signedness");
        let parameter = &ir.modules()[0].parameters()[0];
        assert_eq!(parameter.resolved_value(), Some(-1));
        assert_eq!(parameter.resolved_signed(), Some(true));
    }

    #[test]
    fn preserves_casted_ranges_while_collecting_typedefs() {
        let ir = analyze_source(
            r#"
                module Top #(parameter W = 8);
                    typedef logic [W'(15):0] T;
                    T x;
                endmodule
            "#,
            Path::new("casted_typedef_range.sv"),
        )
        .expect("typedef ranges should use the populated module environment");
        assert_eq!(
            ir.modules()[0].signals()[0].r#type().resolved_width(),
            Some(16)
        );
    }

    #[test]
    fn preserves_named_casts_in_constant_select_indices() {
        let ir = analyze_source(
            r#"
                module Top;
                    localparam W = 1;
                    localparam logic [1:0] A = 2'b10;
                    localparam B = A[W'(0)];
                endmodule
            "#,
            Path::new("casted_constant_select.sv"),
        )
        .expect("constant select indices should use the populated module environment");
        assert_eq!(ir.modules()[0].parameters()[2].resolved_value(), Some(0));
    }

    #[test]
    fn converts_unknowns_when_constant_casting_to_two_state_types() {
        let ir = analyze_source(
            r#"
                module Top;
                    typedef bit [1:0] two_t;
                    localparam logic [1:0] P = two_t'(2'bx1);
                endmodule
            "#,
            Path::new("two_state_constant_cast.sv"),
        )
        .expect("two-state constant casts should convert unknown bits to zero");
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(1));
    }

    #[test]
    fn resolves_typedef_declared_parameter_types() {
        let ir = analyze_source(
            r#"
                module Top;
                    typedef enum logic signed [1:0] { Z = 0 } E;
                    localparam E P = '0;
                    localparam B = $bits(P);
                endmodule
            "#,
            Path::new("typedef_declared_parameter.sv"),
        )
        .expect("typedef-declared parameters should retain their declared type");
        let parameters = ir.modules()[0].parameters();
        assert_eq!(parameters[0].declared_width(), Some(2));
        assert_eq!(parameters[0].declared_signed(), Some(true));
        assert_eq!(parameters[1].resolved_value(), Some(2));
    }

    #[test]
    fn preserves_enum_types_in_instance_parameter_overrides() {
        let ir = analyze_source(
            r#"
                module Child #(parameter P = 0) ();
                endmodule

                module Top;
                    typedef enum logic signed [1:0] { A = 2'b10 } E;
                    Child #(.P(A)) child();
                endmodule
            "#,
            Path::new("typed_enum_parameter_override.sv"),
        )
        .expect("enum parameter overrides should retain their declared type");
        let top = ir
            .modules()
            .iter()
            .find(|module| module.name() == "Top")
            .expect("Top module should exist");
        assert_eq!(
            top.instances()[0].parameter_overrides()[0].value(),
            Some(&ir::ConstExpr::Literal("2'sd2".to_string()))
        );
    }

    #[test]
    fn rejects_conditional_predicate_conjunction_terms() {
        let error = analyze_source(
            r#"
                module Top(input logic a, b, output logic y);
                    always_comb begin
                        if (a &&& b) y = 1'b1;
                        else y = 1'b0;
                    end
                endmodule
            "#,
            Path::new("predicate_conjunction.sv"),
        )
        .expect_err("unsupported predicate conjunctions must not be partially lowered")
        .to_string();
        assert!(
            error.contains("predicate lowering"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn analyzes_basic_sv_module_name() {
        let ir = analyze_source(
            r#"
                module Top(input logic [7:0] a, output logic y);
                    assign y = a;
                endmodule
            "#,
            Path::new("Top.sv"),
        )
        .expect("SV analysis should succeed");

        assert_eq!(ir.modules()[0].name(), "Top");
        assert_eq!(ir.modules()[0].ports().len(), 2);
        assert_eq!(ir.modules()[0].ports()[0].name(), "a");
        assert_eq!(
            ir.modules()[0].ports()[0].direction(),
            ir::PortDirection::Input
        );
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().kind(),
            ir::TypeKind::Logic
        );
        assert_eq!(ir.modules()[0].ports()[0].r#type().packed_ranges().len(), 1);
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().packed_ranges()[0].left(),
            &ir::ConstExpr::Literal("7".to_string())
        );
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().packed_ranges()[0].right(),
            &ir::ConstExpr::Literal("0".to_string())
        );
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
        );
        assert_eq!(ir.modules()[0].ports()[1].name(), "y");
        assert_eq!(
            ir.modules()[0].ports()[1].direction(),
            ir::PortDirection::Output
        );
        assert_eq!(
            ir.modules()[0].ports()[1].r#type().kind(),
            ir::TypeKind::Logic
        );
        assert_eq!(
            ir.modules()[0].ports()[1].r#type().resolved_width(),
            Some(1)
        );
    }

    #[test]
    fn preserves_signedness_for_compound_unpacked_array_lvalues() {
        let ir = analyze_source(
            r#"
                module Top(input logic signed [7:0] input_value);
                    logic signed [7:0] values[2];
                    always_comb begin
                        values[0] = input_value;
                        values[0] >>>= 1;
                    end
                endmodule
            "#,
            Path::new("compound_array.sv"),
        )
        .expect("SV analysis should succeed");
        let compound = ir.modules()[0]
            .comb_processes()
            .iter()
            .flat_map(|process| process.assignments())
            .find_map(|assignment| match assignment.rhs() {
                ir::Expr::Binary {
                    op: ir::BinaryOp::Sar,
                    left,
                    ..
                } => Some(left),
                _ => None,
            })
            .expect("compound assignment should lower to an arithmetic shift");
        assert!(matches!(
            compound.as_ref(),
            ir::Expr::Select { signed: true, .. }
        ));
    }

    #[test]
    fn preserves_operand_signedness_for_size_casts() {
        let ir = analyze_source(
            r#"
                module Top(output logic y);
                    assign y = (8'(0) < -1);
                endmodule
            "#,
            Path::new("size_cast_signedness.sv"),
        )
        .expect("size casts should preserve operand signedness");
        let expression = ir.modules()[0].comb_processes()[0].assignments()[0].rhs();
        let ir::Expr::Binary {
            op: ir::BinaryOp::Lt,
            left,
            ..
        } = expression
        else {
            panic!("expected a signed less-than comparison, got {expression:?}");
        };
        assert!(matches!(
            left.as_ref(),
            ir::Expr::Resize {
                width: 8,
                signed: true,
                ..
            }
        ));
    }

    #[test]
    fn tracks_default_nettype_state_for_each_module() {
        let source = r#"
            `default_nettype none
            module First(); endmodule
            `default_nettype wire
            module Second(); endmodule
            `default_nettype none
            `resetall
            module Third(); endmodule
            `default_nettype none
            module \Top.core (); endmodule
        "#;
        assert_eq!(
            source_module_implicit_net_permissions(source, Path::new("nettype.sv")).unwrap(),
            vec![
                ("First".to_string(), false),
                ("Second".to_string(), true),
                ("Third".to_string(), true),
                ("\\Top.core".to_string(), false),
            ]
        );
    }

    #[test]
    fn rejects_default_nettype_state_changes_inside_modules() {
        for source in [
            r#"
                `default_nettype wire
                module Top();
                    `default_nettype none
                endmodule
            "#,
            r#"
                `default_nettype none
                module Top();
                    `default_nettype wire
                endmodule
            "#,
            r#"
                `default_nettype none
                module Top();
                    `resetall
                endmodule
            "#,
        ] {
            let error = source_module_implicit_net_permissions(source, Path::new("nettype.sv"))
                .expect_err("in-module default nettype change should be rejected");
            assert!(
                error
                    .to_string()
                    .contains("`default_nettype change inside module `Top`"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn permits_redundant_default_nettype_directives_inside_modules() {
        let source = r#"
            `default_nettype none
            module Top();
                `default_nettype none
            endmodule
        "#;
        assert_eq!(
            source_module_implicit_net_permissions(source, Path::new("nettype.sv")).unwrap(),
            vec![("Top".to_string(), false)]
        );
    }

    #[test]
    fn rejects_unsupported_default_net_types() {
        for net_type in ["tri0", "wand", "wor"] {
            let source = format!("`default_nettype {net_type}\nmodule Top(); endmodule");
            let error = source_module_implicit_net_permissions(&source, Path::new("nettype.sv"))
                .expect_err("unsupported default net type should be rejected");
            assert!(
                error
                    .to_string()
                    .contains(&format!("`default_nettype {net_type}`")),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn records_always_ff_case_branches() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic clk,
                    input logic [1:0] mode,
                    input logic [7:0] d0,
                    input logic [7:0] d1,
                    input logic [7:0] d2,
                    output logic [7:0] q
                );
                    always_ff @(posedge clk) begin
                        case (mode)
                            2'b00: q <= d0;
                            2'b01, 2'b10: q <= d1;
                            default: q <= d2;
                        endcase
                    end
                endmodule
            "#,
            Path::new("ff_case.sv"),
        )
        .expect("SV analysis should succeed");

        let process = &ir.modules()[0].ff_processes()[0];
        assert_eq!(process.events().len(), 1);
        assert_eq!(process.assignments().len(), 3);
        assert!(
            process
                .assignments()
                .iter()
                .all(|assignment| assignment.condition().is_some())
        );
    }

    #[test]
    fn accepts_unknown_labels_in_always_ff_case() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic clk,
                    input logic [1:0] mode,
                    output logic q
                );
                    always_ff @(posedge clk) begin
                        case (mode)
                            2'b1x: q <= 1'b1;
                            default: q <= 1'b0;
                        endcase
                    end
                endmodule
            "#,
            Path::new("ff_case_unknown_label.sv"),
        )
        .expect("X/Z case labels should use exact four-state case equality");

        assert_eq!(ir.modules()[0].ff_processes()[0].assignments().len(), 2);
    }

    #[test]
    fn accepts_dynamic_labels_in_always_ff_case() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic clk,
                    input logic [1:0] selector,
                    input logic [1:0] dynamic_label,
                    output logic q
                );
                    always_ff @(posedge clk) begin
                        case (selector)
                            dynamic_label: q <= 1'b1;
                            default: q <= 1'b0;
                        endcase
                    end
                endmodule
            "#,
            Path::new("ff_case_dynamic_label.sv"),
        )
        .expect("dynamic labels should use exact four-state case equality");

        assert_eq!(ir.modules()[0].ff_processes()[0].assignments().len(), 2);
    }

    #[test]
    fn inlines_simple_function_call() {
        let ir = analyze_source(
            r#"
                module Top(input logic a, input logic b, output logic y);
                    function automatic logic choose(input logic s, input logic t);
                        if (s) begin
                            return t;
                        end else begin
                            return 1'b0;
                        end
                    endfunction

                    always_comb y = choose(a, b);
                endmodule
            "#,
            Path::new("Top.sv"),
        )
        .expect("SV analysis should succeed");

        assert!(matches!(
            ir.modules()[0].assignments()[0].rhs(),
            ir::Expr::Resize { expr, width: 1, .. }
                if matches!(&**expr, ir::Expr::Mux { .. })
        ));
    }

    #[test]
    fn inlines_veryl_generated_std_counter_functions() {
        let ir = analyze_source(
            include_str!("../testdata/verilator/StdCounter.sv"),
            Path::new("StdCounter.sv"),
        )
        .expect("SV analysis should succeed");
        let counter = ir
            .modules()
            .iter()
            .find(|module| module.name() == "counter")
            .expect("counter module should exist");
        assert_eq!(
            counter
                .parameters()
                .iter()
                .find(|parameter| parameter.name() == "MAX_COUNT")
                .and_then(|parameter| parameter.resolved_value()),
            Some(3)
        );
        assert_eq!(
            counter
                .ports()
                .iter()
                .find(|port| port.name() == "o_count")
                .and_then(|port| port.r#type().resolved_width()),
            Some(2)
        );

        assert!(
            counter
                .assignments()
                .iter()
                .all(|assignment| !expr_contains_call(assignment.rhs()))
        );
        let count_next = counter
            .assignments()
            .iter()
            .find(|assignment| assignment.lhs() == "count_next")
            .expect("count_next assignment should exist");
        assert!(
            matches!(count_next.rhs(), ir::Expr::Resize { width: 2, .. }),
            "{:#?}",
            count_next.rhs()
        );
        assert!(
            counter
                .signals()
                .iter()
                .any(|signal| signal.name() == "count")
        );
        assert!(
            counter
                .signals()
                .iter()
                .any(|signal| signal.name() == "count_next")
        );
    }

    #[test]
    fn analyzes_veryl_generated_lfsr_tap_assignments() {
        let ir = analyze_source(
            include_str!("../testdata/verilator/Lfsr.sv"),
            Path::new("Lfsr.sv"),
        )
        .expect("SV analysis should succeed");
        let lfsr = ir
            .modules()
            .iter()
            .find(|module| module.name() == "lfsr_galois")
            .expect("lfsr_galois module should exist");
        assert_eq!(
            lfsr.ports()
                .iter()
                .find(|port| port.name() == "o_val")
                .and_then(|port| port.r#type().resolved_width()),
            Some(64)
        );
        assert_eq!(
            lfsr.signals()
                .iter()
                .find(|signal| signal.name() == "val_next")
                .and_then(|signal| signal.r#type().resolved_width()),
            Some(64)
        );

        assert!(lfsr.assignments().iter().any(|assignment| matches!(
            assignment.lhs_value(),
            ir::LValue::Select { name, msb, lsb, .. }
                if name == "val_next"
                    && typecheck::eval_const_expr(
                        msb,
                        &[("SIZE".to_string(), 32)].into_iter().collect(),
                    ) == Some(31)
                    && typecheck::eval_const_expr(
                        lsb,
                        &[("SIZE".to_string(), 32)].into_iter().collect(),
                    ) == Some(31)
        )));
    }

    #[test]
    fn specializes_veryl_generated_lfsr_top_bit_assignment() {
        let ir = analyze_source_with_module_parameter_overrides(
            include_str!("../testdata/verilator/Lfsr.sv"),
            Path::new("Lfsr.sv"),
            "lfsr_galois",
            &[("SIZE".to_string(), 32)].into_iter().collect(),
        )
        .expect("SV analysis should succeed");
        let lfsr = ir
            .modules()
            .iter()
            .find(|module| module.name() == "lfsr_galois")
            .expect("lfsr_galois module should exist");
        let constants: HashMap<_, _> = [("SIZE".to_string(), 32)].into_iter().collect();

        let assignments = lfsr
            .comb_processes()
            .iter()
            .flat_map(|process| process.assignments().iter())
            .filter(|assignment| {
                matches!(
                    assignment.lhs_value(),
                    ir::LValue::Select { name, msb, lsb, .. }
                        if name == "val_next"
                            && typecheck::eval_const_expr(
                                msb,
                                &constants
                            ) == Some(31)
                            && typecheck::eval_const_expr(
                                lsb,
                                &constants
                            ) == Some(31)
                )
            })
            .collect::<Vec<_>>();

        let bit_zero_assignments = lfsr
            .comb_processes()
            .iter()
            .flat_map(|process| process.assignments().iter())
            .filter(|assignment| {
                matches!(
                    assignment.lhs_value(),
                    ir::LValue::Select { name, msb, lsb, .. }
                        if name == "val_next"
                            && typecheck::eval_const_expr(msb, &constants) == Some(0)
                            && typecheck::eval_const_expr(lsb, &constants) == Some(0)
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(assignments.len(), 1);
        assert!(matches!(
            assignments[0].rhs(),
            ir::Expr::Select { expr, msb, lsb, .. }
                if matches!(&**expr, ir::Expr::Ident(name) if name == "o_val")
                    && typecheck::eval_const_expr(msb, &HashMap::default())
                        == Some(0)
                    && typecheck::eval_const_expr(lsb, &HashMap::default())
                        == Some(0)
        ));
        assert!(
            bit_zero_assignments.iter().any(|assignment| {
                matches!(
                    assignment.rhs(),
                    ir::Expr::Mux {
                        condition,
                        then_expr,
                        else_expr,
                    } if matches!(&**condition, ir::Expr::Ident(name) if name == "i_set")
                        && matches!(&**then_expr, ir::Expr::Select { expr, .. }
                            if matches!(&**expr, ir::Expr::Ident(name) if name == "i_setval"))
                        && matches!(&**else_expr, ir::Expr::Binary { .. } | ir::Expr::Select { .. })
                )
            }),
            "val_next[0] should retain its ternary assignment"
        );
    }

    fn expr_contains_call(expr: &ir::Expr) -> bool {
        match expr {
            ir::Expr::Ident(_) | ir::Expr::Literal(_) => false,
            ir::Expr::Select { expr, .. } => expr_contains_call(expr),
            ir::Expr::Resize { expr, .. } => expr_contains_call(expr),
            ir::Expr::Concat(parts) | ir::Expr::RepeatConcat { parts, .. } => {
                parts.iter().any(expr_contains_call)
            }
            ir::Expr::Unary { expr, .. } => expr_contains_call(expr),
            ir::Expr::Binary { left, right, .. } => {
                expr_contains_call(left) || expr_contains_call(right)
            }
            ir::Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                expr_contains_call(condition)
                    || expr_contains_call(then_expr)
                    || expr_contains_call(else_expr)
            }
            ir::Expr::Call { .. } => true,
        }
    }

    #[test]
    fn rejects_duplicate_module_names() {
        let err = analyze_source(
            r#"
                module Top; endmodule
                module Top; endmodule
            "#,
            Path::new("duplicate.sv"),
        )
        .expect_err("duplicate modules should be rejected");

        assert!(matches!(err, AnalyzerError::DuplicateModule { name } if name == "Top"));
    }

    #[test]
    fn rejects_duplicate_port_names() {
        let err = analyze_source(
            r#"
                module Top(input logic a, output logic a);
                endmodule
            "#,
            Path::new("duplicate_port.sv"),
        )
        .expect_err("duplicate ports should be rejected");

        assert!(matches!(
            err,
            AnalyzerError::DuplicatePort { module, name } if module == "Top" && name == "a"
        ));
    }

    #[test]
    fn rejects_repeated_ansi_port_names() {
        let err = analyze_source(
            r#"
                module Top(output logic y, output logic y);
                    assign y = 1'b1;
                endmodule
            "#,
            Path::new("repeated_ansi_port.sv"),
        )
        .expect_err("repeated ANSI ports should be rejected");

        assert!(matches!(
            err,
            AnalyzerError::DuplicatePort { module, name } if module == "Top" && name == "y"
        ));
    }

    #[test]
    fn folds_constant_range_expressions() {
        let ir = analyze_source(
            r#"
                module Top(input logic [(4 * 2) - 1:0] data);
                endmodule
            "#,
            Path::new("constant_expr.sv"),
        )
        .expect("SV analysis should succeed");

        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
        );
    }

    #[test]
    fn folds_parameter_range_expressions() {
        let ir = analyze_source(
            r#"
                module Top #(
                    parameter WIDTH = (4 * 2)
                ) (
                    input logic [WIDTH - 1:0] data
                );
                endmodule
            "#,
            Path::new("parameter_expr.sv"),
        )
        .expect("SV analysis should succeed");

        assert_eq!(ir.modules()[0].parameters()[0].name(), "WIDTH");
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(8));
        assert_eq!(ir.modules()[0].ports()[0].r#type().packed_ranges().len(), 1);
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
        );
    }

    #[test]
    fn folds_based_number_literals() {
        let ir = analyze_source(
            r#"
                module Top(input logic [4'hf:8'd8] data);
                endmodule
            "#,
            Path::new("based_number.sv"),
        )
        .expect("SV analysis should succeed");

        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
        );
    }

    #[test]
    fn folds_localparam_range_expressions() {
        let ir = analyze_source(
            r#"
                module Top(input logic [WIDTH - 1:0] data);
                    localparam WIDTH = 8;
                endmodule
            "#,
            Path::new("localparam_expr.sv"),
        )
        .expect("SV analysis should succeed");

        assert_eq!(ir.modules()[0].parameters()[0].name(), "WIDTH");
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
        );
    }

    #[test]
    fn analyzes_fixed_unpacked_array_dimensions() {
        let ir = analyze_source(
            r#"
                module Top #(parameter N = 2) (
                    input logic [7:0] data,
                    output logic [7:0] out
                );
                    logic [7:0] values [N];
                    assign values[0] = data;
                    assign values[1] = data;
                    assign out = values[1];
                endmodule
            "#,
            Path::new("fixed_array.sv"),
        )
        .expect("fixed unpacked arrays should be analyzed");

        let top = &ir.modules()[0];
        let values = top
            .signals()
            .iter()
            .find(|signal| signal.name() == "values")
            .expect("array signal should be present");
        assert_eq!(values.r#type().unpacked_ranges().len(), 1);
        assert_eq!(values.r#type().resolved_width(), Some(16));
        assert_eq!(top.ports()[1].r#type().resolved_width(), Some(8));
    }

    #[test]
    fn rejects_nonpositive_implicit_unpacked_array_dimensions() {
        for size in ["0", "-1", "SIZE"] {
            let source = format!(
                r#"
                    module Top #(parameter SIZE = 0) ();
                        logic [7:0] values[{size}];
                    endmodule
                "#
            );
            let error = analyze_source(&source, Path::new("invalid_array_size.sv"))
                .expect_err("nonpositive implicit array dimensions should be rejected");
            assert!(
                matches!(error, AnalyzerError::Unsupported(message) if message == "nonpositive unpacked array dimension")
            );
        }
    }

    #[test]
    fn analyzes_nested_static_loops_with_outer_index_environment() {
        let ir = analyze_source(
            r#"
                module Top(input logic clk, input logic d);
                    logic q[2][2];
                    always_ff @(posedge clk) begin
                        for (int i = 0; i < 2; i++) begin
                            for (int j = 0; j < i; j++) begin
                                q[i][j] <= d;
                            end
                        end
                    end
                endmodule
            "#,
            Path::new("nested_static_loops.sv"),
        )
        .expect("nested static loops should use the outer loop environment");
        assert_eq!(ir.modules()[0].ff_processes()[0].assignments().len(), 1);
    }

    #[test]
    fn carries_outer_loop_types_into_nested_loop_preflight() {
        let ir = analyze_source(
            r#"
                module Top(input logic clk, input logic d);
                    logic q;
                    always_ff @(posedge clk) begin
                        for (int i = -1; i < 0; i++) begin
                            for (int j = 0; i < 32'd1; j++) begin
                                q <= d;
                            end
                        end
                    end
                endmodule
            "#,
            Path::new("nested_loop_preflight_types.sv"),
        )
        .expect("nested loop preflight should use outer loop types");
        assert!(ir.modules()[0].ff_processes().is_empty());
    }

    #[test]
    fn applies_expression_types_to_compound_loop_steps() {
        let ir = analyze_source(
            r#"
                module Top(input logic clk, output logic q);
                    always_ff @(posedge clk) begin
                        for (int i = -2; i < 0; i /= 32'd2) begin
                            q <= 1'b1;
                        end
                        for (int i = -3; i < 0; i %= 32'd2) begin
                            q <= 1'b0;
                        end
                    end
                endmodule
            "#,
            Path::new("typed_compound_loop_steps.sv"),
        )
        .expect("compound loop steps should use expression types");
        assert_eq!(ir.modules()[0].ff_processes()[0].assignments().len(), 2);
    }

    #[test]
    fn flattens_partial_unpacked_array_selections() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic [7:0] values[2][3],
                    output logic [7:0] row[3]
                );
                    assign row = values[0];
                endmodule
            "#,
            Path::new("partial_unpacked_selection.sv"),
        )
        .expect("partial unpacked selections should be analyzed");
        let expression = ir.modules()[0].comb_processes()[0].assignments()[0].rhs();
        let ir::Expr::Select { msb, lsb, .. } = expression else {
            panic!("expected a flattened partial selection, got {expression:?}");
        };
        assert_eq!(eval_test_const_expr(msb), Some(23));
        assert_eq!(eval_test_const_expr(lsb), Some(0));
    }

    #[test]
    fn flattens_partial_unpacked_array_lvalue_selections() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic [7:0] row[3],
                    output logic [7:0] values[2][3]
                );
                    assign values[0] = row;
                endmodule
            "#,
            Path::new("partial_unpacked_lvalue_selection.sv"),
        )
        .expect("partial unpacked lvalues should be analyzed");
        let assignment = &ir.modules()[0].comb_processes()[0].assignments()[0];
        let ir::LValue::Select { name, msb, lsb, .. } = assignment.lhs_value() else {
            panic!(
                "expected a flattened partial lvalue selection, got {:?}",
                assignment.lhs_value()
            );
        };
        assert_eq!(name, "values");
        assert_eq!(eval_test_const_expr(msb), Some(23));
        assert_eq!(eval_test_const_expr(lsb), Some(0));
    }

    #[test]
    fn flattens_unpacked_array_range_selections() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic [7:0] source[4],
                    output logic [7:0] target[4]
                );
                    assign target[1:0] = source[1:0];
                endmodule
            "#,
            Path::new("unpacked_array_range_selection.sv"),
        )
        .expect("unpacked array ranges should be analyzed");
        let assignment = &ir.modules()[0].comb_processes()[0].assignments()[0];
        let ir::LValue::Select {
            msb,
            lsb,
            array_slice_width,
            array_slice_reversed,
            ..
        } = assignment.lhs_value()
        else {
            panic!("expected a flattened unpacked range lvalue");
        };
        assert_eq!(eval_test_const_expr(msb), Some(15));
        assert_eq!(eval_test_const_expr(lsb), Some(0));
        assert_eq!(
            array_slice_width.as_ref().map(eval_test_const_expr),
            Some(Some(8))
        );
        assert!(*array_slice_reversed);
        let ir::Expr::Concat(parts) = assignment.rhs() else {
            panic!("expected an element-ordered unpacked range expression");
        };
        assert_eq!(parts.len(), 2);
        let ir::Expr::Select { msb, lsb, .. } = &parts[0] else {
            panic!("expected the first unpacked range element selection");
        };
        assert_eq!(eval_test_const_expr(msb), Some(7));
        assert_eq!(eval_test_const_expr(lsb), Some(0));
        let ir::Expr::Select { msb, lsb, .. } = &parts[1] else {
            panic!("expected the second unpacked range element selection");
        };
        assert_eq!(eval_test_const_expr(msb), Some(15));
        assert_eq!(eval_test_const_expr(lsb), Some(8));
    }

    #[test]
    fn preserves_implicit_packed_bit_selects_on_scalar_arrays() {
        let ir = analyze_source(
            r#"
                module Top(input logic values[2], output logic y);
                    assign y = values[0][0];
                endmodule
            "#,
            Path::new("scalar_array_bit_selection.sv"),
        )
        .expect("scalar array bit-selects should be analyzed");
        let expression = ir.modules()[0].comb_processes()[0].assignments()[0].rhs();
        let ir::Expr::Select { msb, lsb, .. } = expression else {
            panic!("expected a scalar array bit-select, got {expression:?}");
        };
        assert_eq!(eval_test_const_expr(msb), Some(0));
        assert_eq!(eval_test_const_expr(lsb), Some(0));
    }

    #[test]
    fn rejects_duplicate_parameters() {
        let err = analyze_source(
            r#"
                module Top #(parameter WIDTH = 8, parameter WIDTH = 4) ();
                endmodule
            "#,
            Path::new("duplicate_parameter.sv"),
        )
        .expect_err("duplicate parameters should be rejected");

        assert!(matches!(
            err,
            AnalyzerError::DuplicateParameter { module, name } if module == "Top" && name == "WIDTH"
        ));
    }

    #[test]
    fn rejects_duplicate_module_scope_parameters() {
        let err = analyze_source(
            r#"
                module Top(output logic y);
                    parameter P = 0;
                    parameter P = 1;
                    assign y = P;
                endmodule
            "#,
            Path::new("duplicate_module_parameter.sv"),
        )
        .expect_err("duplicate module-scope parameters should be rejected");

        assert!(matches!(
            err,
            AnalyzerError::DuplicateParameter { module, name } if module == "Top" && name == "P"
        ));
    }

    #[test]
    fn rejects_duplicate_instance_names() {
        let err = analyze_source(
            r#"
                module Child(output logic y); assign y = 1'b1; endmodule
                module Top(output logic a, output logic b);
                    Child u(.y(a));
                    Child u(.y(b));
                endmodule
            "#,
            Path::new("duplicate_instance.sv"),
        )
        .expect_err("duplicate instances should be rejected");

        assert!(matches!(
            err,
            AnalyzerError::DuplicateInstance { module, name } if module == "Top" && name == "u"
        ));
    }

    #[test]
    fn preserves_signed_port_types() {
        let ir = analyze_source(
            r#"
                module Top(
                    input logic signed [7:0] a,
                    output signed [3:0] b,
                    input logic unsigned [1:0] c
                );
                endmodule
            "#,
            Path::new("signed_ports.sv"),
        )
        .expect("SV analysis should succeed");

        let ports = ir.modules()[0].ports();
        assert!(ports[0].r#type().is_signed());
        assert_eq!(ports[0].r#type().kind(), ir::TypeKind::Logic);
        assert_eq!(ports[0].r#type().resolved_width(), Some(8));
        assert!(ports[1].r#type().is_signed());
        assert_eq!(ports[1].r#type().kind(), ir::TypeKind::Implicit);
        assert_eq!(ports[1].r#type().resolved_width(), Some(4));
        assert!(!ports[2].r#type().is_signed());
        assert_eq!(ports[2].r#type().resolved_width(), Some(2));
    }

    #[test]
    fn records_module_instantiations() {
        let ir = analyze_source(
            r#"
                module Child(input logic a, output logic y);
                endmodule

                module Top(input logic a, output logic y);
                    Child #(.WIDTH(8)) u_child (.a(a), .y(y));
                endmodule
            "#,
            Path::new("instantiation.sv"),
        )
        .expect("SV analysis should succeed");

        let top = ir
            .modules()
            .iter()
            .find(|module| module.name() == "Top")
            .expect("Top module should exist");
        assert_eq!(top.instances().len(), 1);
        assert_eq!(top.instances()[0].module_name(), "Child");
        assert_eq!(top.instances()[0].name(), "u_child");
        assert_eq!(top.instances()[0].parameter_names(), &["WIDTH"]);
        assert_eq!(top.instances()[0].parameter_overrides().len(), 1);
        assert_eq!(top.instances()[0].parameter_overrides()[0].name(), "WIDTH");
        assert_eq!(
            top.instances()[0].parameter_overrides()[0].value(),
            Some(&ir::ConstExpr::Literal("8".to_string()))
        );
        assert_eq!(top.instances()[0].port_names(), &["a", "y"]);
    }

    #[test]
    fn records_continuous_assignments() {
        let ir = analyze_source(
            r#"
                module Top(input logic a, input logic b, output logic y, output logic z);
                    assign y = a & b;
                    assign z = a | b;
                endmodule
            "#,
            Path::new("continuous_assign.sv"),
        )
        .expect("SV analysis should succeed");

        let top = &ir.modules()[0];
        assert_eq!(top.assignments().len(), 2);
        assert_eq!(top.assignments()[0].lhs(), "y");
        assert_eq!(top.assignments()[1].lhs(), "z");
        assert_eq!(top.comb_processes().len(), 2);
        assert_eq!(
            top.comb_processes()[0].kind(),
            ir::CombProcessKind::ContinuousAssign
        );
        assert_eq!(top.comb_processes()[0].assignments()[0].lhs(), "y");
    }

    #[test]
    fn records_always_comb_processes() {
        let ir = analyze_source(
            r#"
                module Top(input logic a, input logic b, output logic y, output logic z);
                    always_comb begin
                        y = a & b;
                        z = a | b;
                    end
                endmodule
            "#,
            Path::new("always_comb.sv"),
        )
        .expect("SV analysis should succeed");

        let top = &ir.modules()[0];
        assert_eq!(top.assignments().len(), 2);
        assert_eq!(top.comb_processes().len(), 1);
        assert_eq!(
            top.comb_processes()[0].kind(),
            ir::CombProcessKind::AlwaysComb
        );
        assert_eq!(top.comb_processes()[0].assignments()[0].lhs(), "y");
        assert_eq!(top.comb_processes()[0].assignments()[1].lhs(), "z");
    }

    #[test]
    fn expands_operator_assignments() {
        let ir = analyze_source(
            r#"
                module Top(input logic [3:0] a, input logic [3:0] b, output logic [3:0] y);
                    always_comb begin
                        y = a;
                        y ^= b;
                    end
                endmodule
            "#,
            Path::new("operator_assignment.sv"),
        )
        .expect("SV analysis should succeed");

        let top = &ir.modules()[0];
        let assignment = &top.comb_processes()[0].assignments()[1];
        assert_eq!(assignment.lhs(), "y");
        assert!(matches!(
            assignment.rhs(),
            ir::Expr::Binary {
                op: ir::BinaryOp::BitXor,
                ..
            }
        ));
    }

    #[test]
    fn folds_supported_system_functions() {
        let ir = analyze_source(
            r#"
                module Top #(
                    parameter W = $clog2(9),
                    parameter O = $onehot(8),
                    parameter Z = $onehot0(0)
                ) (
                    input logic [W-1:0] data
                );
                endmodule
            "#,
            Path::new("system_functions.sv"),
        )
        .expect("SV analysis should succeed");

        let top = &ir.modules()[0];
        assert_eq!(top.parameters()[0].resolved_value(), Some(4));
        assert_eq!(top.parameters()[1].resolved_value(), Some(1));
        assert_eq!(top.parameters()[2].resolved_value(), Some(1));
        assert_eq!(top.ports()[0].r#type().resolved_width(), Some(4));
    }

    #[test]
    fn analyzes_veryl_emitted_benchmark_sv() {
        let cases = [
            (
                "Countones.sv",
                include_str!("../testdata/verilator/Countones.sv"),
            ),
            (
                "StdCounter.sv",
                include_str!("../testdata/verilator/StdCounter.sv"),
            ),
            (
                "GrayCounter.sv",
                include_str!("../testdata/verilator/GrayCounter.sv"),
            ),
            (
                "GrayCodec.sv",
                include_str!("../testdata/verilator/GrayCodec.sv"),
            ),
            (
                "EdgeDetector.sv",
                include_str!("../testdata/verilator/EdgeDetector.sv"),
            ),
            ("Onehot.sv", include_str!("../testdata/verilator/Onehot.sv")),
            ("Lfsr.sv", include_str!("../testdata/verilator/Lfsr.sv")),
        ];

        for (name, code) in cases {
            let ir = analyze_source(code, Path::new(name))
                .unwrap_or_else(|err| panic!("failed to analyze Veryl-emitted {name}: {err}"));
            assert!(
                !ir.modules().is_empty(),
                "Veryl-emitted {name} should contain modules"
            );
        }
    }

    #[test]
    fn rejects_unlowered_constructs_in_veryl_emitted_sources() {
        for (name, source) in [
            ("Top.sv", include_str!("../testdata/verilator/Top.sv")),
            ("Fifo.sv", include_str!("../testdata/verilator/Fifo.sv")),
        ] {
            let error = analyze_source(source, Path::new(name))
                .expect_err("unlowered constructs must not be silently ignored");
            assert!(matches!(error, AnalyzerError::Unsupported(_)));
        }

        let error = analyze_source(
            include_str!("../testdata/verilator/LinearSec.sv"),
            Path::new("LinearSec.sv"),
        )
        .expect_err("loop-local declarations must not be silently ignored");
        assert!(matches!(
            error,
            AnalyzerError::Unsupported(detail)
                if detail == "local data declaration inside loop-generate"
        ));
    }

    #[test]
    fn records_veryl_emitted_module_instantiations() {
        let ir = analyze_source(
            include_str!("../testdata/verilator/StdCounter.sv"),
            Path::new("StdCounter.sv"),
        )
        .expect("SV analysis should succeed");
        let top = ir
            .modules()
            .iter()
            .find(|module| module.name() == "Top")
            .expect("Top module should exist");

        assert_eq!(top.instances().len(), 1);
        assert_eq!(top.instances()[0].module_name(), "counter");
        assert_eq!(top.instances()[0].name(), "u");
        assert_eq!(top.instances()[0].parameter_names(), &["WIDTH"]);
        assert_eq!(
            top.instances()[0].port_names(),
            &[
                "i_clk",
                "i_rst",
                "i_clear",
                "i_set",
                "i_set_value",
                "i_up",
                "i_down",
                "o_count",
                "o_count_next",
                "o_wrap_around",
            ]
        );
    }
}
