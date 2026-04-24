use celox::SimulatorBuilder;
use veryl_analyzer::{Analyzer, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

#[test]
fn test_trace_analyzer_ir() {
    let code = r#"
        module Top {
            let a: logic<32> = 1;
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_analyzer_ir()
        .build_with_trace();
    let trace = result.trace;

    let analyzer_ir = trace
        .format_analyzer_ir()
        .expect("analyzer_ir should be captured");
    assert!(analyzer_ir.contains("module Top"));
    assert!(analyzer_ir.contains("let var0(a): logic<32>"));
}

#[test]
fn test_trace_analyzer_ir_nested_static_break_escapes_dynamic_loop() {
    let code = r#"
        module Top (
            count: input logic<3>,
            d: input logic<4>,
            q: output logic<8>,
        ) {
            function f (
                n: input logic<3>,
                x: input logic<4>,
            ) -> logic<8> {
                var tmp: logic<8>;
                tmp = 8'd0;
                for i in 0..n {
                    for j in 0..4 {
                        if x[j] {
                            break;
                        }
                    }
                    tmp = tmp + 8'd1;
                }
                return tmp;
            }

            always_comb {
                q = f(count, d);
            }
        }
    "#;
    symbol_table::clear();
    attribute_table::clear();

    let metadata = Metadata::create_default("prj").unwrap();
    let analyzer = Analyzer::new(&metadata);
    let parsed = Parser::parse(code, &"").unwrap();
    analyzer.analyze_pass1("prj", &parsed.veryl);
    Analyzer::analyze_post_pass1();

    let mut context = Context::default();
    let mut analyzer_ir = Ir::default();
    analyzer.analyze_pass2("prj", &parsed.veryl, &mut context, Some(&mut analyzer_ir));
    Analyzer::analyze_post_pass2();

    let analyzer_ir = analyzer_ir.to_string();
    assert!(analyzer_ir.contains("for i in 0..var5 {"));
    assert!(
        !analyzer_ir.contains("for j in 0..4 {"),
        "inner static loop should have remained visible if break ownership were preserved:\n{analyzer_ir}"
    );
    assert!(
        analyzer_ir.contains("break;"),
        "expected analyzer IR to leak a bare break into the outer dynamic loop body:\n{analyzer_ir}"
    );
}
