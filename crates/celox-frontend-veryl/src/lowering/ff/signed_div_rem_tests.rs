use super::FfParser;
use crate::BuildConfig;
use celox_design::BinaryOp;
use celox_sir::{SIRBuilder, SIRInstruction};
use veryl_analyzer::{
    Analyzer, Context, attribute_table,
    ir::{Component, Declaration, Ir},
    symbol_table,
};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

#[test]
fn ff_parser_selects_explicit_div_rem_signedness_from_source_expressions() {
    symbol_table::clear();
    attribute_table::clear();

    let code = r#"
            module Top (
                clk: input clock,
                ua: input logic<8>,
                ub: input logic<8>,
                sa: input signed logic<8>,
                sb: input signed logic<8>,
                sc: input signed logic<8>,
                udiv: output logic<8>,
                urem: output logic<8>,
                sdiv: output signed logic<8>,
                srem: output signed logic<8>,
                nested: output signed logic<8>,
                mixed: output logic<8>
            ) {
                always_ff (clk) {
                    udiv = ua / ub;
                    urem = ua % ub;
                    sdiv = sa / sb;
                    srem = sa % sb;
                    nested = (sa / sb) / sc;
                    mixed = sa / ub;
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
    let declarations = module
        .declarations
        .iter()
        .filter_map(|declaration| match declaration {
            Declaration::Ff(declaration) => Some(declaration.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut parser = FfParser::new(&module, BuildConfig::default());
    let mut builder = SIRBuilder::new();
    parser.parse_ff_group(&declarations, &mut builder).unwrap();
    let execution_unit = builder.flush_eu().unwrap();

    let mut div_u = 0;
    let mut div_s = 0;
    let mut rem_u = 0;
    let mut rem_s = 0;
    for op in execution_unit.blocks.values().flat_map(|block| {
        block
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Binary(_, _, op, _) => Some(*op),
                _ => None,
            })
    }) {
        match op {
            BinaryOp::DivU => div_u += 1,
            BinaryOp::DivS => div_s += 1,
            BinaryOp::RemU => rem_u += 1,
            BinaryOp::RemS => rem_s += 1,
            _ => {}
        }
    }

    assert_eq!((div_u, div_s, rem_u, rem_s), (2, 3, 1, 1));
}
