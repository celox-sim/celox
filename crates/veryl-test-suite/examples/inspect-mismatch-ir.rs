//! Print the pinned analyzer's constant values for two SV/IR discrepancies.
use std::path::Path;
use veryl_analyzer::{
    Analyzer, Context, attribute_table,
    ir::{Component, Ir, VarKind},
    symbol_table,
};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

fn main() {
    let path = Path::new("constant_context.veryl");
    let source = include_str!("../verification/repros/mismatches/constant_context.veryl");
    symbol_table::clear();
    attribute_table::clear();
    let metadata = Metadata::create_default("prj").unwrap();
    let analyzer = Analyzer::new(&metadata);
    let parsed = Parser::parse(source, &path).unwrap();
    let mut errors = analyzer.analyze_pass1("prj", &parsed.veryl);
    errors.extend(Analyzer::analyze_post_pass1());
    let mut context = Context::default();
    let mut ir = Ir::default();
    errors.extend(analyzer.analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir)));
    errors.extend(Analyzer::analyze_post_pass2(&ir));
    assert!(!errors.iter().any(|error| error.is_error()), "{errors:?}");
    let mut constants = Vec::new();
    for component in &ir.components {
        if let Component::Module(module) = component {
            for variable in module.variables.values() {
                if matches!(variable.kind, VarKind::Const) {
                    constants.push(format!(
                        "{} = {:b}",
                        variable.path,
                        variable.get_value(&[]).unwrap()
                    ));
                }
            }
        }
    }
    constants.sort();
    for constant in constants {
        println!("{constant}");
    }
    if let Some(output) = std::env::args_os().nth(1) {
        let emitted = veryl_test_suite::emit::emit_veryl_sources(&[(source, path)]);
        let mut sv = emitted.as_sv_sources()[0].0.to_owned();
        sv.push_str(r#"
module Top;
    wire [31:0] bits_result;
    wire argument_result;
    wire [7:0] shift_result;
    ConstantContext dut(bits_result, argument_result, shift_result);
    initial begin
        #1;
        $display("review:constant_bits=%h constant_argument=%b constant_shift=%b", bits_result, argument_result, shift_result);
        $finish;
    end
endmodule
"#);
        std::fs::write(output, sv).unwrap();
    }
}
