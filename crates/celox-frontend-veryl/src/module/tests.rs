use num_bigint::{BigInt, BigUint};
use veryl_analyzer::{
    Analyzer, Context, attribute_table,
    ir::{Component, Declaration, Expression, Factor, Ir, Op, Statement, VarId},
    symbol_table,
};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

use super::SLTNodeArena;
use super::{
    collect_glue_sources, collect_parent_output_address_sources,
    uses_named_external_port_associations,
};
use crate::{GlueAddr, HashMap};
use celox_design::{BinaryOp, BitAccess, VarAtomBase};
use celox_slt::{SLTForFoldGroupState, SLTNode};

#[test]
fn selected_positional_external_ports_are_not_treated_as_formal_names() {
    assert!(!uses_named_external_port_associations(&["a[7:0]", "y"]));
}

#[test]
fn positional_ternary_external_ports_are_not_treated_as_formal_names() {
    assert!(!uses_named_external_port_associations(&[
        "if sel ? a : b",
        "y",
    ]));
}

fn parse_top_module(code: &str) -> veryl_analyzer::ir::Module {
    symbol_table::clear();
    attribute_table::clear();

    let metadata = Metadata::create_default("prj").unwrap();
    let parser = Parser::parse(code, &"").unwrap();
    let analyzer = Analyzer::new(&metadata);
    let mut context = Context::default();
    let mut ir = Ir::default();
    assert!(analyzer.analyze_pass1("prj", &parser.veryl).is_empty());
    assert!(Analyzer::analyze_post_pass1().is_empty());
    assert!(
        analyzer
            .analyze_pass2(&parser.veryl, &mut context, Some(&mut ir))
            .is_empty()
    );
    assert!(Analyzer::analyze_post_pass2(&ir).is_empty());

    let top = veryl_parser::resource_table::insert_str("Top");
    ir.components
        .into_iter()
        .find_map(|component| match component {
            Component::Module(module) if module.name == top => Some(module),
            _ => None,
        })
        .expect("Top module not found")
}

#[test]
fn memory_extraction_does_not_execute_testbench_only_loops() {
    let module = parse_top_module(
        "#[test(Top)] module Top { var x: u32; initial {
                for i in 0..32 { if i <: 16 { x = i; } }
                $assert(x == 15);
            } }",
    );
    let mut context = Context::default();
    context.variables = module.variables.clone();
    let before: Vec<_> = context
        .variables
        .iter()
        .map(|(id, var)| (*id, var.get_value(&[]).cloned()))
        .collect();
    for declaration in &module.declarations {
        if let Declaration::Initial(initial) = declaration {
            for stmt in &initial.statements {
                super::visit_initial_statement(stmt, &mut context, &mut |_, _, _| {
                    panic!("no memory initialization in this testbench")
                })
                .unwrap();
            }
        }
    }
    // Walking a runtime loop used to overwrite its induction variable
    // during memory extraction, even though it contained no readmemh.
    for (id, value) in before {
        assert_eq!(context.variables[&id].get_value(&[]), value.as_ref());
    }
}

#[test]
fn value_type_check_handles_deep_statement_chains() {
    // Exercise only the checker: parsing and dropping deeply recursive IR
    // have their own traversal constraints.
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            use veryl_analyzer::ir::{Expression, IfStatement, Statement};
            use veryl_analyzer::value::Value;
            let mut module =
                parse_top_module("module Top(a: input logic, q: output logic) { assign q = a; }");
            let Declaration::Comb(comb) = &mut module.declarations[0] else {
                panic!("expected combinational declaration");
            };
            let mut body = std::mem::take(&mut comb.statements);
            for depth in 0..4096 {
                let (true_side, false_side) = if depth % 2 == 0 {
                    (body, Vec::new())
                } else {
                    (Vec::new(), body)
                };
                body = vec![Statement::If(IfStatement {
                    cond: Expression::create_value(Value::new(1, 1, false), Default::default()),
                    true_side,
                    false_side,
                    token: Default::default(),
                })];
            }
            comb.statements = body;
            let result = crate::value_type_check::check_module(&module);
            let Declaration::Comb(comb) = &mut module.declarations[0] else {
                unreachable!()
            };
            let mut pending = std::mem::take(&mut comb.statements);
            while let Some(statement) = pending.pop() {
                if let Statement::If(branch) = statement {
                    pending.extend(branch.true_side);
                    pending.extend(branch.false_side);
                }
            }
            result.unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn output_address_sources_include_runtime_power_exponent() {
    let mut module = parse_top_module(
        r#"
module Top (
    base: input logic<2>,
    idx: input logic,
    q: output logic<4>,
    data: output logic<2>,
) {
    function exponent (x: input logic, y: output logic) -> logic {
        y = x;
        return 1'b1;
    }
    always_comb {
        data = 2'b0;
        q = exponent(idx, data[idx]);
        q = base ** 2;
    }
}
"#,
    );
    let comb = module
        .declarations
        .iter_mut()
        .find_map(|declaration| match declaration {
            Declaration::Comb(comb) => Some(comb),
            _ => None,
        })
        .expect("No always_comb found in Top");
    let Statement::Assign(seed) = &comb.statements[1] else {
        panic!("expected seed assignment");
    };
    let seed = seed.expr.clone();
    let Expression::Term(seed_factor) = &seed else {
        panic!("expected function call expression");
    };
    let Factor::FunctionCall(call) = seed_factor.as_ref() else {
        panic!("expected function call expression");
    };
    let data_id = call
        .outputs
        .values()
        .flatten()
        .next()
        .expect("missing output actual")
        .id;
    let Statement::Assign(pow_assign) = &mut comb.statements[2] else {
        panic!("expected power assignment");
    };
    let Expression::Binary(_, Op::Pow, exponent, _) = &mut pow_assign.expr else {
        panic!("expected power expression");
    };
    **exponent = seed;
    let expression = pow_assign.expr.clone();

    let mut arena = SLTNodeArena::new();
    let mut sources = HashMap::default();
    collect_parent_output_address_sources(
        &module,
        &Default::default(),
        &expression,
        &mut arena,
        &mut sources,
    )
    .unwrap();
    assert!(sources.contains_key(&data_id));
}

#[test]
fn for_fold_group_glue_sources_keep_initial_but_hide_scoped_updates() {
    let loop_id = VarId::default();
    let mut state_id = loop_id;
    state_id.inc();
    let mut external_id = state_id;
    external_id.inc();
    let loop_addr = GlueAddr::Parent(loop_id);
    let state_addr = GlueAddr::Parent(state_id);
    let external_addr = GlueAddr::Parent(external_id);

    let mut arena = SLTNodeArena::new();
    let input = |arena: &mut SLTNodeArena<GlueAddr>, variable| {
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap()
    };
    let guard = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8),
            BigUint::from(0u8),
            1,
            false,
        ))
        .unwrap();
    let initial = input(&mut arena, state_addr);
    let state_input = input(&mut arena, state_addr);
    let loop_input = input(&mut arena, loop_addr);
    let external_input = input(&mut arena, external_addr);
    let uncovered_state_input = arena
        .alloc(SLTNode::Input {
            variable: state_addr,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(8, 15),
        })
        .unwrap();
    let scoped_sum = arena
        .alloc(SLTNode::Binary(state_input, BinaryOp::Add, loop_input))
        .unwrap();
    let update = arena
        .alloc(SLTNode::Binary(scoped_sum, BinaryOp::Add, external_input))
        .unwrap();
    let update = arena
        .alloc(SLTNode::Binary(
            update,
            BinaryOp::Add,
            uncovered_state_input,
        ))
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: loop_addr,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 2,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(state_addr, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();

    let sources = collect_glue_sources(group, &arena);

    assert!(sources.contains(&VarAtomBase::new(state_addr, 0, 7)));
    assert!(sources.contains(&VarAtomBase::new(state_addr, 8, 15)));
    assert!(sources.contains(&VarAtomBase::new(external_addr, 0, 7)));
    assert!(!sources.iter().any(|atom| atom.id == loop_addr));
}
