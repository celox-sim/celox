use super::*;
use crate::{BuildConfig, HashSet};
use celox_design::{STABLE_REGION, WORKING_REGION};
use celox_sir::SIROffset;
use veryl_analyzer::{
    Analyzer, AnalyzerError, Context, attribute_table,
    ir::{Component, Declaration, Ir},
    symbol_table,
};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

#[test]
fn module_state_read_after_write_is_tracked_as_an_old_state_source() {
    symbol_table::clear();
    attribute_table::clear();
    let code = r#"
module Top (
    clk    : input clock,
    present: input logic,
    d      : input logic<8>,
) {
    var in_flight: logic;
    var captured: logic<8>;
    always_ff (clk) {
        in_flight = present;
        if in_flight {
            captured = d;
        }
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
    let in_flight = module
        .variables
        .iter()
        .find_map(|(&id, variable)| (variable.path.to_string() == "in_flight").then_some(id))
        .unwrap();

    let mut parser = FfParser::new(&module, BuildConfig::default());
    let mut builder = SIRBuilder::new();
    let result = parser.parse_ff_group(&declarations, &mut builder).unwrap();

    assert!(result.sources.iter().any(|source| {
        source.id.var_id == in_flight
            && source.id.region == celox_design::STABLE_REGION
            && source.access == BitAccess::new(0, 0)
    }));
}

#[test]
fn direct_ff_stores_are_selected_per_bit_range() {
    symbol_table::clear();
    attribute_table::clear();
    let code = r#"
module Top (
    clk: input clock,
    lo : input logic<8>,
    hi : input logic<8>,
) {
    var state: logic<16>;
    always_ff (clk) {
        state[7:0] = lo;
        state[15:8] = hi;
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
    let state = module
        .variables
        .iter()
        .find_map(|(&id, variable)| (variable.path.to_string() == "state").then_some(id))
        .unwrap();
    let direct_ranges = [(state, vec![BitAccess::new(8, 15)])].into_iter().collect();

    let mut parser = FfParser::new(&module, BuildConfig::default())
        .with_direct_write_ranges(direct_ranges, HashSet::default());
    let mut builder = SIRBuilder::new();
    parser.parse_ff_group(&declarations, &mut builder).unwrap();
    let execution_unit = builder.flush_eu().unwrap();
    let stores = execution_unit
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(address, SIROffset::Static(offset), width, ..)
                if address.var_id == state =>
            {
                Some((address.region, *offset, *width))
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(stores.contains(&(WORKING_REGION, 0, 8)));
    assert!(stores.contains(&(STABLE_REGION, 8, 8)));
}

#[test]
fn conditional_expression_effects_are_not_definitely_defined_after_merge() {
    symbol_table::clear();
    attribute_table::clear();
    let code = r#"
module Top (
    clk: input clock,
    guard: input logic,
    sel: input logic,
    and_result: output logic,
    ternary_result: output logic,
    after_and: output logic,
    after_then: output logic,
    after_else: output logic,
) {
    var and_side: logic;
    var then_side: logic;
    var else_side: logic;
    function touch (value: output logic) -> logic {
        value = 1'b1;
        return 1'b1;
    }
    always_ff (clk) {
        and_result = guard && touch(and_side);
        ternary_result = if sel ? touch(then_side) : touch(else_side);
        after_and = and_side;
        after_then = then_side;
        after_else = else_side;
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
    let errors = analyzer.analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir));
    assert!(
        !errors.is_empty()
            && errors.iter().all(|error| matches!(
                error,
                AnalyzerError::FunctionOutputInAlwaysFf { function, .. }
                    if function == "touch"
            )),
        "expected only always_ff output diagnostics for touch, got: {errors:?}"
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
    let effect_ids = module
        .variables
        .iter()
        .filter_map(|(&id, variable)| {
            matches!(
                variable.path.to_string().as_str(),
                "and_side" | "then_side" | "else_side"
            )
            .then_some(id)
        })
        .collect::<Vec<_>>();
    assert_eq!(effect_ids.len(), 3);

    let mut parser = FfParser::new(&module, BuildConfig::default());
    let mut builder = SIRBuilder::new();
    parser.parse_ff_group(&declarations, &mut builder).unwrap();
    builder.flush_eu().unwrap().verify();

    for id in effect_ids {
        assert!(
            parser
                .defined_ranges
                .get(&id)
                .is_none_or(|bits| bits.is_empty()),
            "conditional output argument must not be definitely defined: {id:?}"
        );
        assert!(!parser.dynamic_defined_vars.contains(&id));
    }
}
