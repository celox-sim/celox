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
            Some(0xffff_ffff)
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
            ir::LValue::Select { name, msb, lsb }
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
                    ir::LValue::Select { name, msb, lsb }
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
                    ir::LValue::Select { name, msb, lsb }
                        if name == "val_next"
                            && typecheck::eval_const_expr(msb, &constants) == Some(0)
                            && typecheck::eval_const_expr(lsb, &constants) == Some(0)
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(assignments.len(), 1);
        assert!(matches!(
            assignments[0].rhs(),
            ir::Expr::Select { expr, msb, lsb }
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
            assert!(matches!(
                error,
                AnalyzerError::Unsupported(detail) if detail == "unpacked array dimension"
            ));
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
