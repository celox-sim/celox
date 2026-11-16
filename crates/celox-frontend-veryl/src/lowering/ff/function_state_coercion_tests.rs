use super::FfParser;
use crate::BuildConfig;
use veryl_analyzer::{
    Analyzer, Context, attribute_table,
    ir::{Component, Expression, Factor, Ir, Op, Statement, ValueVariant},
    symbol_table,
};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

#[test]
fn unpacked_array_element_assignment_uses_element_type() {
    symbol_table::clear();
    attribute_table::clear();

    let code = r#"
            module Top (
                clk: input clock,
                q: output logic
            ) {
                function observed () -> logic {
                    var values: logic<8>[2];
                    values[1] = 8'd0;
                    values[0] = 16'h0100;
                    $display("upper=%0d", values[1]);
                    return 1'b0;
                }

                always_ff (clk) {
                    q = observed();
                }
            }
        "#;
    let metadata = Metadata::create_default("prj").unwrap();
    let parsed = Parser::parse(code, &"").unwrap();
    let analyzer = Analyzer::new(&metadata);
    let mut context = Context::default();
    let mut ir = Ir::default();
    assert!(analyzer.analyze_pass1("prj", &parsed.veryl).is_empty());
    assert!(Analyzer::analyze_post_pass1().is_empty());
    assert!(
        analyzer
            .analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir))
            .is_empty()
    );
    assert!(Analyzer::analyze_post_pass2(&ir).is_empty());

    let module = ir
        .components
        .into_iter()
        .find_map(|component| match component {
            Component::Module(module) => Some(module),
            _ => None,
        })
        .unwrap();
    let function_body = module
        .functions
        .values()
        .find_map(|function| function.get_function(&[]))
        .unwrap();
    let assignment = function_body
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::Assign(assign)
                if assign.expr.comptime().r#type.total_width() == Some(16) =>
            {
                Some(assign)
            }
            _ => None,
        })
        .unwrap();

    let parser = FfParser::new(&module, BuildConfig::default());
    let coerced = parser
        .coerce_function_state_assignment(assignment.expr.clone(), &assignment.dst[0])
        .unwrap();
    let Expression::Binary(_, Op::As, target, comptime) = coerced else {
        panic!("unpacked array element assignment must insert an explicit cast");
    };
    assert_eq!(comptime.r#type.total_width(), Some(8));
    let Expression::Term(target) = target.as_ref() else {
        panic!("assignment cast must have a term target");
    };
    let Factor::Value(target) = target.as_ref() else {
        panic!("assignment cast must have a type target");
    };
    let ValueVariant::Type(target) = &target.value else {
        panic!("assignment cast target must be a type");
    };
    assert_eq!(target.total_width(), Some(8));
    assert!(target.array.is_empty());
}
