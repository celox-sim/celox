use veryl_analyzer::ir::{Component, Declaration, Ir, VarKind, VarPath};
use veryl_analyzer::{Analyzer, Context, attribute_table, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;
use veryl_parser::resource_table;

use super::*;
use crate::logic_tree::parse_comb_with_loop_recovery;
use crate::loop_provenance::{LoopProvenance, LoopSourceTable};

#[test]
fn binary_case_proof_requires_distinct_complete_labels() {
    for (labels, expected) in [
        (vec![0u8, 1, 2, 3], true),
        (vec![0, 1, 1, 3], false),
        (vec![0, 1, 2], false),
    ] {
        let mut arena = SLTNodeArena::new();
        let target = VarId::from_raw(1);
        let selector = arena
            .alloc(SLTNode::Input {
                variable: VarId::from_raw(2),
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 1),
            })
            .unwrap();
        let mut expr = arena
            .alloc(SLTNode::Input {
                variable: target,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        for label in labels.into_iter().rev() {
            let constant = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(label),
                    BigUint::zero(),
                    2,
                    false,
                ))
                .unwrap();
            let cond = arena
                .alloc(SLTNode::Binary(selector, BinaryOp::EqWildcard, constant))
                .unwrap();
            let cond = super::super::procedural_condition(&mut arena, cond).unwrap();
            let value = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(label + 10),
                    BigUint::zero(),
                    8,
                    false,
                ))
                .unwrap();
            expr = arena
                .alloc(SLTNode::Mux {
                    cond,
                    then_expr: value,
                    else_expr: expr,
                })
                .unwrap();
        }
        let path = LogicPath {
            target: celox_slt::LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
            expr,
            transparent_copy: false,
            sources: HashSet::default(),
            previous_sources: Default::default(),
            address_sources: Default::default(),
            local_inputs: Vec::new(),
            order_before: Default::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
        };
        assert!(!proves_combinational(std::slice::from_ref(&path), &arena));
        assert_eq!(
            proves_binary_combinational(std::slice::from_ref(&path), &arena),
            expected
        );
        // Two-state lowering drops the unmatched branch only for
        // complete labels; otherwise the retained value stays.
        let mut paths = [path];
        assert_eq!(complete_binary_cases(&mut paths, &mut arena), expected);
        assert_eq!(paths[0].expr == expr, !expected);
        assert_eq!(
            proves_combinational(&paths, &arena),
            expected,
            "{}",
            arena.display(paths[0].expr)
        );
    }
}

#[test]
fn latch_feedback_proves_deep_fully_written_wide_graph() {
    let mut arena = SLTNodeArena::new();
    let old = arena
        .alloc(SLTNode::Input {
            variable: VarId::from_raw(0),
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 511),
        })
        .unwrap();
    let cond = arena
        .alloc(SLTNode::Input {
            variable: VarId::from_raw(1),
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let all = (BigUint::one() << 512) - BigUint::one();
    let mut expr = old;
    let mut incomplete = old;
    for bit in 0..512 {
        let value = BigUint::one() << bit;
        let clear = arena
            .alloc(SLTNode::Constant(
                &all ^ &value,
                BigUint::zero(),
                512,
                false,
            ))
            .unwrap();
        let set = arena
            .alloc(SLTNode::Constant(value, BigUint::zero(), 512, false))
            .unwrap();
        let cleared = arena
            .alloc(SLTNode::Binary(expr, BinaryOp::And, clear))
            .unwrap();
        let written = arena
            .alloc(SLTNode::Binary(cleared, BinaryOp::Or, set))
            .unwrap();
        expr = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr: written,
                else_expr: cleared,
            })
            .unwrap();
        if bit == 510 {
            incomplete = expr;
        }
    }
    let path = |expr| LogicPath {
        target: celox_slt::LogicPathTarget::Var(VarAtomBase::new(VarId::from_raw(0), 0, 511)),
        transparent_copy: false,
        sources: Default::default(),
        previous_sources: Default::default(),
        address_sources: Default::default(),
        local_inputs: Vec::new(),
        order_before: Default::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
        expr,
    };
    assert!(proves_combinational(&[path(expr)], &arena));
    assert!(!proves_combinational(&[path(incomplete)], &arena));
}

#[test]
fn latch_feedback_proof_bounds_opaque_operand_work() {
    let mut arena = SLTNodeArena::new();
    let wide = arena
        .alloc(SLTNode::Input {
            variable: VarId::from_raw(0),
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 999_999),
        })
        .unwrap();
    let reduction = arena.alloc(SLTNode::Unary(UnaryOp::Or, wide)).unwrap();
    let mut budget = FeedbackProofBudget {
        nodes: 64,
        bits: 64,
        edges: 64,
    };
    assert!(
        feedback_masks(
            &[reduction],
            &arena,
            &HashMap::default(),
            &mut budget,
            false
        )
        .is_none()
    );
}

fn dependency_masks_for_test(
    roots: &[NodeId],
    arena: &SLTNodeArena<VarId>,
    target: BitAccess,
) -> Vec<BigUint> {
    let targets = [(VarId::from_raw(0), vec![target])].into_iter().collect();
    let mut budget = FeedbackProofBudget {
        nodes: 1024,
        bits: 65_536,
        edges: 4096,
    };
    feedback_masks(roots, arena, &targets, &mut budget, false).unwrap()
}

#[test]
fn latch_feedback_keeps_address_and_mux_condition_dependencies() {
    let mut arena = SLTNodeArena::new();
    let old = arena
        .alloc(SLTNode::Input {
            variable: VarId::from_raw(0),
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let indexed = arena
        .alloc(SLTNode::Input {
            variable: VarId::from_raw(1),
            signed: false,
            index: vec![celox_slt::SLTIndex {
                node: old,
                stride: 1,
                kind: celox_slt::SLTIndexKind::Packed,
            }],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let zero = arena
        .alloc(SLTNode::Constant(0u8.into(), 0u8.into(), 8, false))
        .unwrap();
    let one = arena
        .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 8, false))
        .unwrap();
    let mux = arena
        .alloc(SLTNode::Mux {
            cond: old,
            then_expr: zero,
            else_expr: one,
        })
        .unwrap();
    let masks = dependency_masks_for_test(&[indexed, mux], &arena, BitAccess::new(0, 0));
    assert_eq!(masks, vec![BigUint::from(255u8), BigUint::from(255u8)]);
}

#[test]
fn latch_feedback_keeps_indexed_reads_beyond_the_base_window() {
    let mut arena = SLTNodeArena::new();
    let index = arena
        .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 1, false))
        .unwrap();
    let selected = arena
        .alloc(SLTNode::Input {
            variable: VarId::from_raw(0),
            signed: false,
            index: vec![celox_slt::SLTIndex {
                node: index,
                stride: 8,
                kind: celox_slt::SLTIndexKind::Packed,
            }],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    assert_eq!(
        dependency_masks_for_test(&[selected], &arena, BitAccess::new(8, 15)),
        vec![BigUint::from(255u8)]
    );
    let path = LogicPath {
        target: celox_slt::LogicPathTarget::Var(VarAtomBase::new(VarId::from_raw(0), 8, 15)),
        transparent_copy: false,
        sources: Default::default(),
        previous_sources: Default::default(),
        address_sources: Default::default(),
        local_inputs: Vec::new(),
        order_before: Default::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
        expr: selected,
    };
    assert!(!proves_combinational(&[path], &arena));
}

#[test]
fn latch_feedback_logical_shifts_keep_the_left_operand_width() {
    let mut arena = SLTNodeArena::new();
    let old = arena
        .alloc(SLTNode::Input {
            variable: VarId::from_raw(0),
            signed: true,
            index: Vec::new(),
            access: BitAccess::new(0, 3),
        })
        .unwrap();
    let amount = arena
        .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 8, false))
        .unwrap();
    let left = arena
        .alloc(SLTNode::Binary(old, BinaryOp::Shl, amount))
        .unwrap();
    let right = arena
        .alloc(SLTNode::Binary(old, BinaryOp::Shr, amount))
        .unwrap();
    assert_eq!(get_width(left, &arena), 4);
    assert_eq!(get_width(right, &arena), 4);
    assert_eq!(
        dependency_masks_for_test(&[left, right], &arena, BitAccess::new(0, 3)),
        vec![BigUint::from(14u8), BigUint::from(7u8)]
    );
}

#[test]
fn latch_feedback_keeps_arithmetic_carries_and_signed_extensions() {
    let mut arena = SLTNodeArena::new();
    let old = arena
        .alloc(SLTNode::Input {
            variable: VarId::from_raw(0),
            signed: true,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let one = arena
        .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 8, true))
        .unwrap();
    let high = arena
        .alloc(SLTNode::Constant(128u8.into(), 0u8.into(), 8, true))
        .unwrap();
    let add = arena
        .alloc(SLTNode::Binary(old, BinaryOp::Add, one))
        .unwrap();
    let and = arena
        .alloc(SLTNode::Binary(old, BinaryOp::And, high))
        .unwrap();
    let add_bit = arena
        .alloc(SLTNode::Slice {
            expr: add,
            access: BitAccess::new(7, 7),
        })
        .unwrap();
    let and_bit = arena
        .alloc(SLTNode::Slice {
            expr: and,
            access: BitAccess::new(7, 7),
        })
        .unwrap();
    assert_eq!(
        dependency_masks_for_test(&[add_bit, and_bit], &arena, BitAccess::new(0, 0)),
        vec![BigUint::one(), BigUint::one()]
    );
}

#[test]
fn latch_feedback_clears_only_known_controlling_bits() {
    let mut arena = SLTNodeArena::new();
    let old = arena
        .alloc(SLTNode::Input {
            variable: VarId::from_raw(0),
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let unknown = arena
        .alloc(SLTNode::Constant(0u8.into(), 1u8.into(), 1, false))
        .unwrap();
    let one = arena
        .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 1, false))
        .unwrap();
    let unknown_and = arena
        .alloc(SLTNode::Binary(old, BinaryOp::And, unknown))
        .unwrap();
    let known_or = arena
        .alloc(SLTNode::Binary(old, BinaryOp::Or, one))
        .unwrap();
    assert_eq!(
        dependency_masks_for_test(&[unknown_and, known_or], &arena, BitAccess::new(0, 0)),
        vec![BigUint::one(), BigUint::zero()]
    );
}

#[test]
fn latch_feedback_bounds_repeated_concat_edges() {
    let mut arena = SLTNodeArena::new();
    let bit = arena
        .alloc(SLTNode::Constant(0u8.into(), 0u8.into(), 1, false))
        .unwrap();
    let repeated = arena.alloc(SLTNode::Concat(vec![(bit, 1); 512])).unwrap();
    let mut budget = FeedbackProofBudget {
        nodes: 64,
        bits: 1024,
        edges: 64,
    };
    assert!(feedback_proof_nodes(&[repeated], &arena, &mut budget).is_none());
}

#[test]
fn constant_specialization_distinguishes_signed_division_and_remainder() {
    let lhs = SpecializedConstant {
        value: BigUint::from(0xf9u8),
        mask: BigUint::from(0u8),
        width: 8,
        signed: true,
    };
    let rhs = SpecializedConstant {
        value: BigUint::from(4u8),
        mask: BigUint::from(0u8),
        width: 8,
        signed: true,
    };

    let div_u = specialize_binary_constant(BinaryOp::DivU, &lhs, &rhs, 8, false).unwrap();
    let rem_u = specialize_binary_constant(BinaryOp::RemU, &lhs, &rhs, 8, false).unwrap();
    let div_s = specialize_binary_constant(BinaryOp::DivS, &lhs, &rhs, 8, true).unwrap();
    let rem_s = specialize_binary_constant(BinaryOp::RemS, &lhs, &rhs, 8, true).unwrap();

    assert_eq!(div_u.value, BigUint::from(0x3eu8));
    assert_eq!(rem_u.value, BigUint::from(1u8));
    assert_eq!(div_s.value, BigUint::from(0xffu8));
    assert_eq!(rem_s.value, BigUint::from(0xfdu8));
    assert!(!div_u.signed && !rem_u.signed);
    assert!(div_s.signed && rem_s.signed);
}

#[test]
fn two_state_constant_specialization_clears_unknown_bits() {
    let inner = SpecializedConstant {
        value: BigUint::from(0b1110_1101u8),
        mask: BigUint::from(0b1010_0100u8),
        width: 8,
        signed: true,
    };

    let result = specialize_unary_constant(UnaryOp::ToTwoState, &inner, 8, true).unwrap();
    assert_eq!(result.value, BigUint::from(0b0100_1001u8));
    assert_eq!(result.mask, BigUint::from(0u8));
    assert!(result.signed);
    assert!(specialize_unary_constant(UnaryOp::Ident, &inner, 8, true).is_none());
}

#[test]
fn boolean_constant_specialization_uses_dominant_known_one() {
    for (value, expected_not, expected_or) in [
        (0b1000_0100u8, (0u8, 0u8), (1u8, 0u8)),
        (0b0000_0100u8, (1u8, 1u8), (1u8, 1u8)),
        (0u8, (1u8, 1u8), (1u8, 1u8)),
    ] {
        let inner = SpecializedConstant {
            value: BigUint::from(value),
            mask: BigUint::from(0b0000_0100u8),
            width: 8,
            signed: false,
        };

        let logic_not = specialize_unary_constant(UnaryOp::LogicNot, &inner, 1, false).unwrap();
        assert_eq!(logic_not.value, BigUint::from(expected_not.0));
        assert_eq!(logic_not.mask, BigUint::from(expected_not.1));
        assert!(!logic_not.signed);

        let reduction_or = specialize_unary_constant(UnaryOp::Or, &inner, 1, false).unwrap();
        assert_eq!(reduction_or.value, BigUint::from(expected_or.0));
        assert_eq!(reduction_or.mask, BigUint::from(expected_or.1));
        assert!(!reduction_or.signed);
    }
}

fn analyze(code: &str) -> (Module, LoopProvenance) {
    symbol_table::clear();
    attribute_table::clear();

    let metadata = Metadata::create_default("prj").expect("default metadata must be valid");
    let analyzer = Analyzer::new(&metadata);
    let parsed = Parser::parse(code, &"").expect("test source must parse");
    let loop_sources = LoopSourceTable::collect([&parsed.veryl]);
    let pass1_errors = analyzer.analyze_pass1("prj", &parsed.veryl);
    assert!(pass1_errors.is_empty(), "pass1 errors: {pass1_errors:?}");
    assert!(
        Analyzer::analyze_post_pass1().is_empty(),
        "post-pass1 must succeed"
    );

    let mut context = Context::default();
    let mut ir = Ir::default();
    let pass2_errors = analyzer.analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir));
    assert!(pass2_errors.is_empty(), "pass2 errors: {pass2_errors:?}");
    assert!(
        Analyzer::analyze_post_pass2(&ir).is_empty(),
        "post-pass2 must succeed"
    );
    let provenance = loop_sources.match_unrolled(&ir);
    let top = resource_table::insert_str("Top");
    let module = ir
        .components
        .iter()
        .find_map(|component| match component {
            Component::Module(module) if module.name == top => Some(module.clone()),
            _ => None,
        })
        .expect("Top module must exist");
    (module, provenance)
}

fn folded_sequence_fixture() -> (Module, UnrolledLoopCandidate) {
    let (module, provenance) = analyze(
        r#"
                module Top (
                    value: output logic,
                ) {
                    var state: logic;
                    always_comb {
                        state = 1'b0;
                        for i in 0..16 {
                            if i == 15 {
                                state = 1'b1;
                            }
                        }
                        value = state;
                    }
                }
            "#,
    );
    let candidate = provenance
        .candidates_for_module(&module)
        .into_iter()
        .next()
        .expect("the fixture loop must retain provenance")
        .unrolled;
    assert_eq!(candidate.iterations.len(), 16);
    (module, candidate)
}

fn folded_constant_expressions(
    values: impl IntoIterator<Item = usize>,
    width: usize,
    signed: bool,
) -> Vec<Expression> {
    values
        .into_iter()
        .map(|value| {
            Expression::Term(Box::new(Factor::create_value(
                Value::new(value as u64, width, signed),
                TokenRange::default(),
            )))
        })
        .collect()
}

fn parameterize_test_sequence(
    module: &Module,
    candidate: &UnrolledLoopCandidate,
    values: &[Expression],
) -> Option<ExactFoldedValueForm> {
    let variants = values.iter().collect::<Vec<_>>();
    parameterize_exact_folded_value(module, &values[0], &variants, candidate).map(|(_, form)| form)
}

#[test]
fn parameterizes_exact_quotient_sequence_and_rejects_a_near_miss() {
    let (module, candidate) = folded_sequence_fixture();
    let quotient = folded_constant_expressions((0..16).map(|value| value / 8), 32, true);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &quotient),
        Some(ExactFoldedValueForm::Quotient {
            divisor: BigInt::from(8u8),
        })
    );

    let mut near_miss = (0..16).map(|value| value / 8).collect::<Vec<_>>();
    near_miss[7] = 1;
    let near_miss = folded_constant_expressions(near_miss, 32, true);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &near_miss),
        None
    );

    let non_power_of_two = folded_constant_expressions((0..16).map(|value| value / 3), 32, true);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &non_power_of_two),
        None,
        "recovery must not introduce a runtime division"
    );
}

#[test]
fn parameterizes_exact_modulo_affine_sequences_and_rejects_a_near_miss() {
    let (module, candidate) = folded_sequence_fixture();
    let byte_offsets = folded_constant_expressions((0..16).map(|value| (value % 8) * 8), 32, true);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &byte_offsets),
        Some(ExactFoldedValueForm::ModuloAffine {
            slope: BigInt::from(8u8),
            intercept: BigInt::zero(),
            modulus: BigInt::from(64u8),
        })
    );

    let previous_entry =
        folded_constant_expressions((0..16).map(|value| (value + 15) % 16), 32, true);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &previous_entry),
        Some(ExactFoldedValueForm::ModuloAffine {
            slope: BigInt::one(),
            intercept: BigInt::from(15u8),
            modulus: BigInt::from(16u8),
        })
    );

    let mut near_miss = (0..16).map(|value| (value % 8) * 8).collect::<Vec<_>>();
    near_miss[5] += 1;
    let near_miss = folded_constant_expressions(near_miss, 32, true);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &near_miss),
        None
    );

    let non_power_of_two =
        folded_constant_expressions((0..16).map(|value| (value * 3 + 1) % 10), 32, true);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &non_power_of_two),
        None,
        "recovery must not introduce a runtime remainder"
    );
}

#[test]
fn parameterizes_exact_eq_ne_indicators_and_rejects_near_misses() {
    let (module, candidate) = folded_sequence_fixture();
    let eq = folded_constant_expressions((0..16).map(|value| usize::from(value == 0)), 1, false);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &eq),
        Some(ExactFoldedValueForm::Eq(BigInt::zero()))
    );
    let ne = folded_constant_expressions((0..16).map(|value| usize::from(value != 0)), 1, false);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &ne),
        Some(ExactFoldedValueForm::Ne(BigInt::zero()))
    );

    let mut eq_near_miss = (0..16)
        .map(|value| usize::from(value == 0))
        .collect::<Vec<_>>();
    eq_near_miss[1] = 1;
    let eq_near_miss = folded_constant_expressions(eq_near_miss, 1, false);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &eq_near_miss),
        None
    );

    let mut ne_near_miss = (0..16)
        .map(|value| usize::from(value != 0))
        .collect::<Vec<_>>();
    ne_near_miss[1] = 0;
    let ne_near_miss = folded_constant_expressions(ne_near_miss, 1, false);
    assert_eq!(
        parameterize_test_sequence(&module, &candidate, &ne_near_miss),
        None
    );
}

fn parse_with_candidates(
    module: &Module,
    candidates: &[LoopRecoveryCandidate],
) -> (Vec<super::super::LogicPath<VarId>>, SLTNodeArena<VarId>) {
    let declaration = module
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Comb(comb) => Some(comb),
            _ => None,
        })
        .expect("always_comb must exist");
    let mut arena = SLTNodeArena::new();
    let (paths, _, _, _, _) =
        parse_comb_with_loop_recovery(module, declaration, &mut arena, candidates, 0)
            .expect("comb lowering must succeed");
    (paths, arena)
}

#[test]
fn independent_packed_lanes_keep_static_offsets() {
    let (module, provenance) = analyze(
        r#"
            module Top(a: input logic<128>, en: input logic<16>, q: output logic<128>) {
                always_comb {
                    q = '0;
                    for i in 0..16 {
                        if en[i] { q[i * 8 +: 8] = a[i * 8 +: 8]; }
                    }
                }
            }
        "#,
    );
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);
    let (_, arena) = parse_with_candidates(&module, &candidates);
    assert!(
        !arena
            .iter()
            .any(|node| matches!(node, SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. }))
    );
}

fn variable(module: &Module, name: &str) -> VarId {
    let name = resource_table::insert_str(name);
    module
        .variables
        .values()
        .find(|variable| variable.path == VarPath(vec![name]))
        .map(|variable| variable.id)
        .expect("named variable must exist")
}

fn group_count(arena: &SLTNodeArena<VarId>) -> usize {
    arena
        .iter()
        .filter(|node| matches!(node, SLTNode::ForFoldGroup { .. }))
        .count()
}

const MULTI_STATE_LOOP: &str = r#"
        module Top (
            en   : input  logic,
            bits : input  logic<4>,
            value: output logic<3>,
        ) {
            var valid: logic;
            var age  : logic<3>;
            var data : logic<3>;
            always_comb {
                valid = 1'b0;
                age   = 3'd0;
                data  = 3'd0;
                if en {
                    for i in 0..4 {
                        let ai: logic<3> = (i as 3);
                        if bits[i] && (!valid || ai >: age) {
                            valid = 1'b1;
                            age   = ai;
                            data  = ai;
                        }
                    }
                }
                value = data;
            }
        }
    "#;

const CROSS_STATE_LOOP: &str = r#"
        module Top (
            value: output logic<8>,
        ) {
            var left : logic<4>;
            var right: logic<4>;
            always_comb {
                left  = 4'd1;
                right = 4'd2;
                for i in 0..4 {
                    let old_left : logic<4> = left;
                    let old_right: logic<4> = right;
                    left  = old_right + (i as 4);
                    right = old_left ^ (i as 4);
                }
                value = {left, right};
            }
        }
    "#;

const PARTIAL_FOUND_LOOP: &str = r#"
        module Top (
            source: input  logic<4>,
            mask  : input  logic<4>,
            old   : input  logic<4>,
            bound : input  logic<3>,
            vm    : input  logic,
            mode  : input  logic<2>,
            value : output logic<5>,
        ) {
            var found   : logic;
            var selected: logic<4>;
            always_comb {
                found    = 1'b0;
                selected = old;
                for i in 0..4 {
                    if ((i as 3) <: bound) && (vm || mask[i]) {
                        let bf : logic = !found && !source[i];
                        let of : logic = !found && source[i];
                        let sif: logic = !found;
                        selected[i] = case mode {
                            2'b01  : bf,
                            2'b10  : of,
                            default: sif,
                        };
                        found = found || source[i];
                    }
                }
                value = {found, selected};
            }
        }
    "#;

const MULTIPLE_PARTIAL_WRITES_LOOP: &str = r#"
        module Top (
            low  : input  logic<4>,
            high : input  logic<4>,
            value: output logic<8>,
        ) {
            var state: logic<2> [4];
            always_comb {
                state[0] = 2'b00;
                state[1] = 2'b00;
                state[2] = 2'b00;
                state[3] = 2'b00;
                for i in 0..4 {
                    state[i][0] = low[i];
                    state[i][1] = high[i];
                }
                value = {state[3], state[2], state[1], state[0]};
            }
        }
    "#;

const DESCENDING_SELECT_LOOP: &str = r#"
        module Top (
            bits : input  logic<4>,
            value: output logic<4>,
        ) {
            var state: logic<4>;
            always_comb {
                state = 4'd0;
                for i in 0..4 {
                    state[3 - i] = bits[i];
                }
                value = state;
            }
        }
    "#;

const INDUCTION_INDEPENDENT_RECURRENCE: &str = r#"
        module Top (
            seed : input  logic<8>,
            value: output logic<8>,
        ) {
            var state: logic<8>;
            always_comb {
                state = seed;
                for _i in 0..8 {
                    state = {state[6:0], state[7] ^ state[5]};
                }
                value = state;
            }
        }
    "#;

const PMP_STYLE_PRIORITY_LOOP: &str = r#"
        module Top (
            i_pmpcfg : input  logic<64> [2] ,
            i_pmpaddr: input  logic<64> [16],
            probe    : input  logic<54>     ,
            value    : output logic<3>      ,
        ) {
            var matched: logic;
            var allow_w: logic;
            var any_cfg: logic;
            always_comb {
                matched = 1'b0;
                allow_w = 1'b0;
                any_cfg = 1'b0;
                for e in 0..16 {
                    let cfg   : logic<8>  = i_pmpcfg[e / 8][(e % 8) * 8+:8];
                    let a     : logic<2>  = cfg[4:3];
                    let this_a: logic<54> = i_pmpaddr[e][53:0];
                    let lo    : logic<54> = if e == 0 ? 54'd0 : i_pmpaddr[(e + 15) % 16][53:0];
                    let hit   : logic     = (probe >= lo) && (probe <: this_a);
                    if a != 2'd0 {
                        any_cfg = 1'b1;
                    }
                    if !matched && hit {
                        matched = 1'b1;
                        allow_w = cfg[0];
                    }
                }
                value = {matched, allow_w, any_cfg};
            }
        }
    "#;

#[test]
fn recovers_guarded_multi_state_loop_as_one_shared_group() {
    let (module, provenance) = analyze(MULTI_STATE_LOOP);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);
    let (paths, arena) = parse_with_candidates(&module, &candidates);

    assert_eq!(group_count(&arena), 1);
    let (group_id, loop_var, states, entry_guard, trip_count) = arena
        .iter()
        .enumerate()
        .find_map(|(index, node)| match node {
            SLTNode::ForFoldGroup {
                loop_var,
                states,
                entry_guard,
                trip_count,
                ..
            } => Some((NodeId(index), *loop_var, states, *entry_guard, *trip_count)),
            _ => None,
        })
        .expect("group must exist");
    assert_eq!(trip_count, 4);
    assert_eq!(states.len(), 3);
    assert!(!matches!(arena.get(entry_guard), SLTNode::Constant(..)));
    assert!(states.iter().any(|state| {
        let mut visited = HashSet::default();
        update_has_dynamic_loop_index(state.update, loop_var, &arena, &mut visited)
    }));

    for name in ["valid", "age", "data"] {
        let id = variable(&module, name);
        let path = paths
            .iter()
            .find(|path| path.target.var().is_some_and(|target| target.id == id))
            .expect("state path must exist");
        assert!(matches!(
            arena.get(path.expr),
            SLTNode::Slice { expr, .. } if *expr == group_id
        ));
    }
}

#[test]
fn recovers_descending_dynamic_destination_select() {
    let (module, provenance) = analyze(DESCENDING_SELECT_LOOP);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);

    let (_, arena) = parse_with_candidates(&module, &candidates);
    let (loop_var, states) = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup {
                loop_var, states, ..
            } => Some((*loop_var, states)),
            _ => None,
        })
        .expect("descending destination select must retain the recovered loop");
    assert_eq!(states.len(), 1);

    let mut visited = HashSet::default();
    let mut inputs = HashSet::default();
    assert!(collect_template_inputs(
        states[0].update,
        &arena,
        &mut visited,
        &mut inputs,
    ));
    assert!(inputs.contains(&loop_var));
}

#[test]
fn recovers_fixed_recurrence_without_induction_reads() {
    let (module, provenance) = analyze(INDUCTION_INDEPENDENT_RECURRENCE);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);

    let (_, arena) = parse_with_candidates(&module, &candidates);
    let (states, trip_count) = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup {
                states, trip_count, ..
            } => Some((states, *trip_count)),
            _ => None,
        })
        .expect("an induction-independent recurrence must retain its fixed loop");
    assert_eq!(trip_count, 8);
    assert_eq!(states.len(), 1);
}

#[test]
fn recovers_pmp_style_quotient_modulo_and_priority_recurrence() {
    let (module, provenance) = analyze(PMP_STYLE_PRIORITY_LOOP);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);

    let (_, arena) = parse_with_candidates(&module, &candidates);
    assert!(
        !arena.iter().any(|node| matches!(
            node,
            SLTNode::Binary(
                _,
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS,
                _
            )
        )),
        "power-of-two quotient/modulo recovery must lower to shifts and masks"
    );
    let (loop_var, states, trip_count) = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup {
                loop_var,
                states,
                trip_count,
                ..
            } => Some((*loop_var, states, *trip_count)),
            _ => None,
        })
        .expect("the full PMP-style recurrence must be recovered");

    assert_eq!(trip_count, 16);
    assert_eq!(states.len(), 3);
    assert!(states.iter().any(|state| {
        let mut visited = HashSet::default();
        update_has_dynamic_loop_index(state.update, loop_var, &arena, &mut visited)
    }));
}

#[test]
fn recovers_unrolled_loop_nested_in_case_arm() {
    let code = r#"
            module Top (
                sel  : input  logic<2>,
                bits : input  logic<4>,
                value: output logic<4>,
            ) {
                var state: logic<4>;
                always_comb {
                    state = 4'd0;
                    case sel {
                        2'd0: {
                            for i in 0..4 {
                                state[i] = bits[i];
                            }
                        }
                        default: {}
                    }
                    value = state;
                }
            }
        "#;
    let (module, provenance) = analyze(code);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);

    let (_, arena) = parse_with_candidates(&module, &candidates);
    let (entry_guard, trip_count, states) = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup {
                entry_guard,
                trip_count,
                states,
                ..
            } => Some((*entry_guard, *trip_count, states)),
            _ => None,
        })
        .expect("case-arm loop must be recovered");

    assert_eq!(trip_count, 4);
    assert_eq!(states.len(), 1);
    assert!(
        !matches!(arena.get(entry_guard), SLTNode::Constant(..)),
        "the recovered loop must retain the case-arm predicate"
    );
    assert_eq!(states[0].target.id, variable(&module, "state"));
    assert_eq!(states[0].target.access, BitAccess::new(0, 3));
}

#[test]
fn guarded_count_idiom_stays_expanded_for_word_lowering() {
    let code = r#"
            module Top (
                en   : input  logic,
                bits : input  logic<8>,
                value: output logic<4>,
            ) {
                var count: logic<4>;
                always_comb {
                    count = 4'd0;
                    if en {
                        for i in 0..8 {
                            count = count + {3'b0, bits[i]};
                        }
                    }
                    value = count;
                }
            }
        "#;
    let (module, provenance) = analyze(code);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);

    let (_, arena) = parse_with_candidates(&module, &candidates);
    assert_eq!(
        group_count(&arena),
        0,
        "a guarded count must remain available to the exact word-level matcher",
    );
}

#[test]
fn composes_case_and_if_guards_for_nested_recovered_loop() {
    let code = r#"
            module Top (
                sel  : input  logic<2>,
                en   : input  logic,
                bits : input  logic<4>,
                value: output logic<4>,
            ) {
                var state: logic<4>;
                always_comb {
                    state = 4'd0;
                    case sel {
                        2'd0: {
                            if en {
                                for i in 0..4 {
                                    state[i] = bits[i];
                                }
                            } else {
                                state = 4'hf;
                            }
                        }
                        default: {}
                    }
                    value = state;
                }
            }
        "#;
    let (module, provenance) = analyze(code);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);

    let (_, arena) = parse_with_candidates(&module, &candidates);
    let entry_guard = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup { entry_guard, .. } => Some(*entry_guard),
            _ => None,
        })
        .expect("nested loop must be recovered");
    let mut visited = HashSet::default();
    let mut inputs = HashSet::default();
    assert!(collect_template_inputs(
        entry_guard,
        &arena,
        &mut visited,
        &mut inputs,
    ));
    assert!(inputs.contains(&variable(&module, "sel")));
    assert!(inputs.contains(&variable(&module, "en")));
}

#[test]
fn recovers_loop_with_function_call_expression() {
    let code = r#"
            module Top (
                data : input  logic<32>,
                seed : input  logic<32>,
                value: output logic<32>,
            ) {
                function lane (
                    x   : input logic<8>,
                    bias: input logic<8>,
                ) -> logic<8> {
                    return x + bias;
                }

                var invariant: logic<32>;
                var state: logic<32>;
                always_comb {
                    invariant = data ^ seed;
                    state = 32'd0;
                    for i in 0..4 {
                        state[i*8+:8] = lane(invariant[i*8+:8], (i as 8));
                    }
                    value = state;
                }
            }
        "#;
    let (module, provenance) = analyze(code);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);

    let (_, arena) = parse_with_candidates(&module, &candidates);
    let states = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup { states, .. } => Some(states),
            _ => None,
        })
        .expect("function-call lane loop must be recovered");
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].target.id, variable(&module, "state"));
    assert_eq!(states[0].target.access, BitAccess::new(0, 31));
}

#[test]
fn whole_fold_proof_composes_cross_state_updates() {
    let (module, provenance) = analyze(CROSS_STATE_LOOP);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);
    let declaration = module
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Comb(comb) => Some(comb),
            _ => None,
        })
        .unwrap();
    let start = declaration
        .statements
        .iter()
        .position(|statement| statement_in_range(statement, candidates[0].source.body_token))
        .unwrap();
    let run_len = candidate_run_len(
        &declaration.statements[start..],
        candidates[0].source.body_token,
    );
    let proof_variables =
        proof_variable_ids(&declaration.statements[start..start + run_len], &[]).unwrap();
    assert!(proof_variables.contains(&variable(&module, "left")));
    assert!(proof_variables.contains(&variable(&module, "right")));
    assert!(!proof_variables.contains(&variable(&module, "value")));
    assert!(proof_variables.len() < module.variables.len());
    assert_eq!(
        marker_store(&module, &proof_variables).unwrap().len(),
        proof_variables.len()
    );

    let (_, arena) = parse_with_candidates(&module, &candidates);

    let states = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup { states, .. } => Some(states),
            _ => None,
        })
        .expect("cross-state loop must pass whole-fold composition proof");
    assert_eq!(states.len(), 2);
    assert_eq!(
        states
            .iter()
            .map(|state| state.target.id)
            .collect::<HashSet<_>>(),
        [variable(&module, "left"), variable(&module, "right")]
            .into_iter()
            .collect()
    );
}

#[test]
fn rejects_non_affine_provenance_values() {
    let (module, provenance) = analyze(MULTI_STATE_LOOP);
    let mut candidates = provenance.candidates_for_module(&module);
    candidates[0].unrolled.iterations[2].value = 9;
    let (_, arena) = parse_with_candidates(&module, &candidates);
    assert_eq!(group_count(&arena), 0);
}

#[test]
fn failed_guarded_probe_restores_effectful_guard_store() {
    let (module, provenance) = analyze(
        r#"
                module Top (
                    en         : input  logic,
                    bits       : input  logic<4>,
                    guard_calls: output logic<8>,
                ) {
                    function guard (
                        enabled : input  logic,
                        previous: input  logic<8>,
                        next    : output logic<8>,
                    ) -> logic {
                        next = previous + 8'd1;
                        return enabled;
                    }

                    var state: logic<4>;
                    var calls: logic<8>;
                    always_comb {
                        state = 4'd0;
                        calls = 8'd0;
                        if guard(en, calls, calls) {
                            for i in 0..4 {
                                state[i] = bits[i];
                            }
                        }
                        guard_calls = calls;
                    }
                }
            "#,
    );
    let mut candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);
    // Keep the source-range match intact while making recovery decline
    // the candidate after the speculative guard evaluation.
    candidates[0].unrolled.iterations[2].value = 9;

    let (paths, arena) = parse_with_candidates(&module, &candidates);
    assert_eq!(group_count(&arena), 0);
    let guard_calls = variable(&module, "guard_calls");
    let path = paths
        .iter()
        .find(|path| {
            path.target
                .var()
                .is_some_and(|target| target.id == guard_calls)
        })
        .expect("guard_calls path must exist");
    let SLTNode::Binary(lhs, BinaryOp::Add, rhs) = arena.get(path.expr) else {
        panic!("guard_calls expression: {:?}", arena.get(path.expr));
    };
    assert!(matches!(
        arena.get(*lhs),
        SLTNode::Constant(value, mask, 8, false)
            if value.is_zero() && mask.is_zero()
    ));
    assert!(matches!(
        arena.get(*rhs),
        SLTNode::Constant(value, mask, 8, false)
            if value == &BigUint::from(1u8) && mask.is_zero()
    ));
}

#[test]
fn rejects_ambiguous_duplicate_candidate() {
    let (module, provenance) = analyze(MULTI_STATE_LOOP);
    let mut candidates = provenance.candidates_for_module(&module);
    candidates.push(candidates[0].clone());
    let (_, arena) = parse_with_candidates(&module, &candidates);
    assert_eq!(group_count(&arena), 0);
}

#[test]
fn rejects_state_initial_value_that_depends_on_the_same_state() {
    let (module, provenance) = analyze(MULTI_STATE_LOOP);
    let candidates = provenance.candidates_for_module(&module);
    let declaration = module
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Comb(comb) => Some(comb),
            _ => None,
        })
        .unwrap();
    let guarded_body = declaration
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::If(statement) => Some(statement.true_side.as_slice()),
            _ => None,
        })
        .unwrap();

    let mut arena = SLTNodeArena::new();
    let all_variables = module.variables.keys().copied().collect::<HashSet<_>>();
    let mut initial_store = marker_store(&module, &all_variables).unwrap();
    for name in ["valid", "age", "data"] {
        let id = variable(&module, name);
        let width = resolve_total_width(&module, &module.variables[&id]).unwrap();
        let node = if name == "valid" {
            arena
                .alloc(SLTNode::Input {
                    variable: id,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })
                .unwrap()
        } else {
            arena
                .alloc(SLTNode::Constant(
                    BigUint::from(0u8),
                    BigUint::from(0u8),
                    width,
                    false,
                ))
                .unwrap()
        };
        let mut sources = HashSet::default();
        if name == "valid" {
            sources.insert(VarAtomBase::new(id, 0, width - 1));
        }
        initial_store.insert(id, RangeStore::new(Some((node, sources)), width));
    }

    let recovered = recover_group(
        &module,
        &initial_store,
        guarded_body,
        &candidates[0],
        None,
        &mut arena,
    )
    .unwrap();
    assert!(recovered.is_none());
}

#[test]
fn rejects_temporally_old_state_embedded_as_a_loop_invariant() {
    let (module, provenance) = analyze(MULTI_STATE_LOOP);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);
    let declaration = module
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Comb(comb) => Some(comb),
            _ => None,
        })
        .unwrap();
    let guarded_body = declaration
        .statements
        .iter()
        .find_map(|statement| match statement {
            Statement::If(statement) => Some(statement.true_side.as_slice()),
            _ => None,
        })
        .unwrap();

    let mut arena = SLTNodeArena::new();
    let all_variables = module.variables.keys().copied().collect::<HashSet<_>>();
    let mut initial_store = marker_store(&module, &all_variables).unwrap();
    for name in ["valid", "age", "data"] {
        let id = variable(&module, name);
        let width = resolve_total_width(&module, &module.variables[&id]).unwrap();
        let zero = arena
            .alloc(SLTNode::Constant(
                BigUint::zero(),
                BigUint::zero(),
                width,
                false,
            ))
            .unwrap();
        initial_store.insert(id, RangeStore::new(Some((zero, HashSet::default())), width));
    }

    let valid = variable(&module, "valid");
    let old_valid = arena
        .alloc(SLTNode::Input {
            variable: valid,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let captured_bits = arena
        .alloc(SLTNode::Concat(vec![(old_valid, 1); 4]))
        .unwrap();
    let mut captured_sources = HashSet::default();
    captured_sources.insert(VarAtomBase::new(valid, 0, 0));
    initial_store.insert(
        variable(&module, "bits"),
        RangeStore::new(Some((captured_bits, captured_sources)), 4),
    );

    let recovered = recover_group(
        &module,
        &initial_store,
        guarded_body,
        &candidates[0],
        None,
        &mut arena,
    )
    .unwrap();
    assert!(
        recovered.is_none(),
        "an old state version needs a distinct capture and must not be rebound to loop state",
    );
}

#[test]
fn recovers_loop_dependent_destination_as_full_width_state() {
    let code = r#"
            module Top (
                bits : input  logic<4>,
                value: output logic<4>,
            ) {
                var state: logic<4>;
                always_comb {
                    state = 4'd0;
                    for i in 0..4 {
                        state[i] = bits[i];
                    }
                    value = state;
                }
            }
        "#;
    let (module, provenance) = analyze(code);
    let candidates = provenance.candidates_for_module(&module);
    let (_, arena) = parse_with_candidates(&module, &candidates);
    let states = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup { states, .. } => Some(states),
            _ => None,
        })
        .expect("dynamic partial write must be recovered");
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].target.id, variable(&module, "state"));
    assert_eq!(states[0].target.access, BitAccess::new(0, 3));
}

#[test]
fn recovers_partial_dynamic_write_with_cross_state_found_recurrence() {
    let (module, provenance) = analyze(PARTIAL_FOUND_LOOP);
    let candidates = provenance.candidates_for_module(&module);
    let (_, arena) = parse_with_candidates(&module, &candidates);
    let (group, states, loop_width, loop_signed, start, step, trip_count) = arena
        .iter()
        .enumerate()
        .find_map(|(index, node)| match node {
            SLTNode::ForFoldGroup {
                loop_width,
                loop_signed,
                start,
                step,
                trip_count,
                states,
                ..
            } => Some((
                NodeId(index),
                states,
                *loop_width,
                *loop_signed,
                start,
                step,
                *trip_count,
            )),
            _ => None,
        })
        .expect("cross-state partial writes must be recovered");

    assert_eq!(loop_width, 32);
    assert!(loop_signed, "Veryl's recovered for-loop IV is signed");
    assert_eq!(start, &BigInt::from(0u8));
    assert_eq!(step, &BigInt::from(1u8));
    assert_eq!(trip_count, 4);
    assert_eq!(states.len(), 2);
    let targets = states
        .iter()
        .map(|state| (state.target.id, state.target.access))
        .collect::<HashMap<_, _>>();
    assert_eq!(
        targets.get(&variable(&module, "found")),
        Some(&BitAccess::new(0, 0))
    );
    assert_eq!(
        targets.get(&variable(&module, "selected")),
        Some(&BitAccess::new(0, 3))
    );
    assert!(
        celox_slt::matches_slt_or_scan_group(group, &arena),
        "the exact recovered Veryl scan must enter the word-scan lowering"
    );
}

#[test]
fn deduplicates_multiple_partial_writes_to_the_same_base_state() {
    let (module, provenance) = analyze(MULTIPLE_PARTIAL_WRITES_LOOP);
    let candidates = provenance.candidates_for_module(&module);
    let (_, arena) = parse_with_candidates(&module, &candidates);
    let states = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup { states, .. } => Some(states),
            _ => None,
        })
        .expect("multiple writes to one base must be recovered");

    assert_eq!(states.len(), 1);
    assert_eq!(states[0].target.id, variable(&module, "state"));
    assert_eq!(states[0].target.access, BitAccess::new(0, 7));
}

#[test]
fn rejects_wrong_affine_provenance_for_partial_dynamic_writes() {
    let (module, provenance) = analyze(PARTIAL_FOUND_LOOP);
    let mut candidates = provenance.candidates_for_module(&module);
    for (iteration, wrong_value) in candidates[0]
        .unrolled
        .iterations
        .iter_mut()
        .zip([3, 2, 1, 0])
    {
        iteration.value = wrong_value;
    }

    let (_, arena) = parse_with_candidates(&module, &candidates);
    assert_eq!(group_count(&arena), 0);
}

#[test]
fn recovers_heliodor_shaped_32_iteration_array_fold() {
    let code = r#"
            module Top (
                en        : input  logic,
                load_age  : input  logic<5>,
                load_addr : input  logic<64>,
                store_addr: input  logic<64> [32],
                store_data: input  logic<64> [32],
                store_size: input  logic<2>  [32],
                store_fwd : input  logic     [32],
                value     : output logic<8>,
            ) {
                var found: logic;
                var age  : logic<5>;
                var selected: logic<8>;

                always_comb {
                    found = 1'b0;
                    age   = 5'd0;
                    selected = 8'd0;
                    if en {
                        for i in 0..32 {
                            let age_i: logic<5>  = (i as 5);
                            let sa   : logic<64> = store_addr[i];
                            let la   : logic<64> = load_addr;
                            let cov  : logic = store_fwd[i]
                                && (age_i <: load_age)
                                && (la >= sa)
                                && (la <: sa + ((5'd1 << store_size[i]) as 64));
                            let off  : logic<64> = la - sa;
                            let byt  : logic<64> = store_data[i] >> {off[2:0], 3'b000};
                            if cov && (!found || age_i >: age) {
                                found = 1'b1;
                                age   = age_i;
                                selected = byt[7:0];
                            }
                        }
                    }
                    value = selected;
                }
            }
        "#;
    let (module, provenance) = analyze(code);
    let candidates = provenance.candidates_for_module(&module);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].unrolled.iterations.len(), 32);
    for iteration in &candidates[0].unrolled.iterations {
        let local_lets = module
            .variables
            .values()
            .filter(|variable| {
                variable.kind == VarKind::Let
                    && variable.path.0.len() > iteration.hierarchy.len()
                    && variable.path.0.starts_with(&iteration.hierarchy)
            })
            .count();
        assert_eq!(local_lets, 6);
    }

    let (_, arena) = parse_with_candidates(&module, &candidates);
    assert_eq!(group_count(&arena), 1);
    let (loop_var, states, trip_count) = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup {
                loop_var,
                states,
                trip_count,
                ..
            } => Some((*loop_var, states, *trip_count)),
            _ => None,
        })
        .expect("recovered group must exist");
    assert_eq!(trip_count, 32);
    assert_eq!(states.len(), 3);

    let mut narrow_widths = BTreeSet::new();
    let mut visited = HashSet::default();
    for state in states {
        collect_narrow_dynamic_input_widths(
            state.update,
            loop_var,
            &arena,
            &mut visited,
            &mut narrow_widths,
        );
    }
    assert!(narrow_widths.contains(&1), "missing scalar dynamic load");
    assert!(narrow_widths.contains(&2), "missing size dynamic load");
    assert!(narrow_widths.contains(&64), "missing 64-bit dynamic load");
}

#[test]
fn keeps_a_loop_invariant_static_destination_as_a_narrow_state() {
    let code = r#"
            module Top (
                bits : input  logic<4>,
                value: output logic<16>,
            ) {
                var state: logic<8> [2];
                always_comb {
                    state[0] = 8'd0;
                    state[1] = 8'd0;
                    for i in 0..4 {
                        if bits[i] {
                            state[1] = state[1] ^ ((i + 1) as 8);
                        }
                    }
                    value = {state[1], state[0]};
                }
            }
        "#;
    let (module, provenance) = analyze(code);
    let candidates = provenance.candidates_for_module(&module);
    let (_, arena) = parse_with_candidates(&module, &candidates);
    let state = variable(&module, "state");
    let target = arena
        .iter()
        .find_map(|node| match node {
            SLTNode::ForFoldGroup { states, .. } => states
                .iter()
                .find(|entry| entry.target.id == state)
                .map(|entry| entry.target),
            _ => None,
        })
        .expect("the fixed-destination recurrence must be recovered");
    assert_eq!(
        target.access,
        BitAccess::new(8, 15),
        "a fixed array element must not become a whole-array carried state"
    );
}

#[test]
fn proof_bit_canonicalizer_handles_deep_arithmetic_on_a_small_stack() {
    let (module, _) = analyze(MULTI_STATE_LOOP);
    let bits = variable(&module, "bits");
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut arena = SLTNodeArena::new();
            let input = arena
                .alloc(SLTNode::Input {
                    variable: bits,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, 3),
                })
                .unwrap();
            let mut node = input;
            for _ in 0..10_000 {
                node = arena
                    .alloc(SLTNode::Binary(node, BinaryOp::Add, input))
                    .unwrap();
            }
            let mut canonicalizer = ProofBitCanonicalizer::default();
            assert!(!proof_node_signed(
                node,
                &arena,
                &mut canonicalizer.signed_cache
            ));
            let result = canonicalize_proof_bit(node, 0, &mut arena, &mut canonicalizer);
            assert!(result.is_some());
            assert_eq!(
                result,
                canonicalize_proof_bit(node, 0, &mut arena, &mut canonicalizer)
            );
            assert_ne!(
                result,
                canonicalize_proof_bit(input, 0, &mut arena, &mut canonicalizer)
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn proof_bit_canonicalizer_reuses_bits_and_concat_layouts() {
    let (module, _) = analyze(MULTI_STATE_LOOP);
    let bits = variable(&module, "bits");
    let mut arena = SLTNodeArena::new();
    let input = arena
        .alloc(SLTNode::Input {
            variable: bits,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 3),
        })
        .unwrap();
    let low = arena
        .alloc(SLTNode::Slice {
            expr: input,
            access: BitAccess::new(0, 1),
        })
        .unwrap();
    let high = arena
        .alloc(SLTNode::Slice {
            expr: input,
            access: BitAccess::new(2, 3),
        })
        .unwrap();
    let concat = arena
        .alloc(SLTNode::Concat(vec![(high, 2), (low, 2)]))
        .unwrap();
    let mut canonicalizer = ProofBitCanonicalizer::default();

    assert_eq!(
        proof_outputs_match(&[concat], &[input], &mut arena, &mut canonicalizer),
        Some(true)
    );
    assert_eq!(canonicalizer.concat_layout_builds, 1);
    assert_eq!(
        canonicalizer.concat_layouts[&concat],
        vec![(low, 0), (low, 1), (high, 0), (high, 1)]
    );
    let first_misses = canonicalizer.bit_cache_misses;
    let first_hits = canonicalizer.bit_cache_hits;
    let first_nodes = arena.len();

    // The second comparison must be served entirely by the shared bit
    // cache: four lhs and four rhs root lookups, with no new proof nodes.
    assert_eq!(
        proof_outputs_match(&[concat], &[input], &mut arena, &mut canonicalizer),
        Some(true)
    );
    assert_eq!(canonicalizer.bit_cache_misses, first_misses);
    assert_eq!(canonicalizer.bit_cache_hits - first_hits, 8);
    assert_eq!(canonicalizer.concat_layout_builds, 1);
    assert_eq!(arena.len(), first_nodes);

    // Force bit recomputation while retaining the Concat layout. The
    // layout must still be reused rather than rebuilt or rescanned.
    canonicalizer.bit_cache.clear();
    assert_eq!(
        proof_outputs_match(&[concat], &[input], &mut arena, &mut canonicalizer),
        Some(true)
    );
    assert_eq!(canonicalizer.concat_layout_builds, 1);
    assert_eq!(arena.len(), first_nodes);
}

#[test]
fn wide_deep_proof_does_not_retain_every_bit_column() {
    let (module, _) = analyze(MULTI_STATE_LOOP);
    let bits = variable(&module, "bits");
    let width = 512;
    let depth = 256;
    let mut arena = SLTNodeArena::new();
    let input = arena
        .alloc(SLTNode::Input {
            variable: bits,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, width - 1),
        })
        .unwrap();
    let mut equivalent = input;
    for _ in 0..depth {
        equivalent = arena
            .alloc(SLTNode::Slice {
                expr: equivalent,
                access: BitAccess::new(0, width - 1),
            })
            .unwrap();
    }
    let mut canonicalizer = ProofBitCanonicalizer::default();
    assert_eq!(
        proof_outputs_match(&[equivalent], &[input], &mut arena, &mut canonicalizer),
        Some(true)
    );
    let retained_bytes =
        canonicalizer.bit_cache.capacity() * std::mem::size_of::<((NodeId, usize), NodeId)>();
    let arena_bytes = arena.len() * std::mem::size_of::<SLTNode<VarId>>();
    // Hash table growth rounds capacity up; allow that storage overhead,
    // while rejecting the depth-by-width retention of the old evaluator.
    assert!(
        retained_bytes <= 4 * arena_bytes,
        "cache={retained_bytes} arena={arena_bytes}"
    );
    let flipped = arena.alloc(SLTNode::Unary(UnaryOp::BitNot, input)).unwrap();
    assert_eq!(
        proof_outputs_match(&[equivalent], &[flipped], &mut arena, &mut canonicalizer),
        Some(false)
    );
}

#[test]
fn proof_comparison_canonicalization_preserves_signed_extension() {
    let (module, _) = analyze(MULTI_STATE_LOOP);
    let bits = variable(&module, "bits");
    let mut arena = SLTNodeArena::new();
    let signed_narrow = arena
        .alloc(SLTNode::Input {
            variable: bits,
            signed: true,
            index: Vec::new(),
            access: BitAccess::new(0, 3),
        })
        .unwrap();
    let unsigned_select = arena
        .alloc(SLTNode::Slice {
            expr: signed_narrow,
            access: BitAccess::new(0, 3),
        })
        .unwrap();
    let signed_wide = arena
        .alloc(SLTNode::Input {
            variable: bits,
            signed: true,
            index: Vec::new(),
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let signed_eq = arena
        .alloc(SLTNode::Binary(signed_narrow, BinaryOp::Eq, signed_wide))
        .unwrap();
    let unsigned_eq = arena
        .alloc(SLTNode::Binary(unsigned_select, BinaryOp::Eq, signed_wide))
        .unwrap();

    assert_eq!(
        proof_outputs_match(
            &[signed_eq],
            &[unsigned_eq],
            &mut arena,
            &mut ProofBitCanonicalizer::default(),
        ),
        Some(false),
    );
}

#[test]
fn constant_comparison_specialization_matches_lowering_extension_rules() {
    let signed_narrow = SpecializedConstant {
        value: BigUint::from(0xfu8),
        mask: BigUint::zero(),
        width: 4,
        signed: true,
    };
    let unsigned_narrow = SpecializedConstant {
        signed: false,
        ..signed_narrow.clone()
    };
    let signed_wide = SpecializedConstant {
        value: BigUint::from(0xffu8),
        mask: BigUint::zero(),
        width: 8,
        signed: true,
    };

    let signed_eq =
        specialize_binary_constant(BinaryOp::Eq, &signed_narrow, &signed_wide, 1, false).unwrap();
    let signed_eq_case =
        specialize_binary_constant(BinaryOp::EqCase, &signed_narrow, &signed_wide, 1, false)
            .unwrap();
    let unsigned_eq =
        specialize_binary_constant(BinaryOp::Eq, &unsigned_narrow, &signed_wide, 1, false).unwrap();

    assert_eq!(signed_eq.value, BigUint::from(1u8));
    assert_eq!(signed_eq_case.value, BigUint::from(1u8));
    assert_eq!(unsigned_eq.value, BigUint::from(0u8));
}

#[test]
fn known_mux_specialization_preserves_result_width() {
    let mut arena = SLTNodeArena::new();
    let cond = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8),
            BigUint::from(0u8),
            1,
            false,
        ))
        .unwrap();
    let narrow = arena
        .alloc(SLTNode::Constant(
            BigUint::from(5u8),
            BigUint::from(0u8),
            3,
            true,
        ))
        .unwrap();
    let wide = arena
        .alloc(SLTNode::Constant(
            BigUint::from(0xa5u8),
            BigUint::from(0u8),
            8,
            false,
        ))
        .unwrap();
    let mux = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr: narrow,
            else_expr: wide,
        })
        .unwrap();

    let specialized =
        specialize_slt_node(mux, None, None, &mut arena, &mut HashMap::default()).unwrap();
    assert!(matches!(
        arena.get(specialized),
        SLTNode::Constant(value, mask, 8, false)
            if value == &BigUint::from(5u8) && mask.is_zero()
    ));
}

#[test]
fn specializes_deep_dag_without_using_the_call_stack() {
    let mut arena = SLTNodeArena::new();
    let mut root = arena
        .alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0u8),
            1,
            false,
        ))
        .unwrap();
    for _ in 0..20_000 {
        root = arena.alloc(SLTNode::Unary(UnaryOp::BitNot, root)).unwrap();
    }

    let specialized =
        specialize_slt_node(root, None, None, &mut arena, &mut HashMap::default()).unwrap();
    assert!(matches!(
        arena.get(specialized),
        SLTNode::Constant(value, mask, 1, false) if value.is_zero() && mask.is_zero()
    ));
}

#[test]
fn specializes_dynamic_slt_for_every_iteration() {
    let (module, provenance) = analyze(MULTI_STATE_LOOP);
    let candidate = provenance
        .candidates_for_module(&module)
        .into_iter()
        .next()
        .unwrap();
    let loop_var = candidate.unrolled.iterations[0].loop_var;
    let bits = variable(&module, "bits");
    let mut arena = SLTNodeArena::new();
    let loop_input = arena
        .alloc(SLTNode::Input {
            variable: loop_var,
            signed: true,
            index: Vec::new(),
            access: BitAccess::new(0, 31),
        })
        .unwrap();
    let raw_bits = arena
        .alloc(SLTNode::Input {
            variable: bits,
            signed: true,
            index: vec![super::super::SLTIndex {
                node: loop_input,
                stride: 1,
                kind: super::super::SLTIndexKind::Packed,
            }],
            access: BitAccess::new(0, 3),
        })
        .unwrap();
    let dynamic_bit = arena
        .alloc(SLTNode::Slice {
            expr: raw_bits,
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let narrow_loop_value = arena
        .alloc(SLTNode::Slice {
            expr: loop_input,
            access: BitAccess::new(0, 2),
        })
        .unwrap();

    for iteration in &candidate.unrolled.iterations {
        let mut cache = HashMap::default();
        let specialized_bit = specialize_slt_node(
            dynamic_bit,
            Some((loop_var, iteration.value)),
            None,
            &mut arena,
            &mut cache,
        )
        .unwrap();
        let expected_bit = arena
            .alloc(SLTNode::Input {
                variable: bits,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(iteration.value, iteration.value),
            })
            .unwrap();
        assert_eq!(specialized_bit, expected_bit);

        let mut cache = HashMap::default();
        let specialized_value = specialize_slt_node(
            narrow_loop_value,
            Some((loop_var, iteration.value)),
            None,
            &mut arena,
            &mut cache,
        )
        .unwrap();
        let expected_value = arena
            .alloc(SLTNode::Constant(
                BigUint::from(iteration.value & 0b111),
                BigUint::from(0u8),
                3,
                false,
            ))
            .unwrap();
        assert_eq!(specialized_value, expected_value);
    }

    let signed_constant = arena
        .alloc(SLTNode::Constant(
            BigUint::from(0xf5u8),
            BigUint::from(0u8),
            8,
            true,
        ))
        .unwrap();
    let signed_slice = arena
        .alloc(SLTNode::Slice {
            expr: signed_constant,
            access: BitAccess::new(0, 3),
        })
        .unwrap();
    let specialized_slice = specialize_slt_node(
        signed_slice,
        None,
        None,
        &mut arena,
        &mut HashMap::default(),
    )
    .unwrap();
    assert!(matches!(
        arena.get(specialized_slice),
        SLTNode::Constant(value, mask, 4, false)
            if value == &BigUint::from(5u8) && mask.is_zero()
    ));
}

fn update_has_dynamic_loop_index(
    root: NodeId,
    loop_var: VarId,
    arena: &SLTNodeArena<VarId>,
    visited: &mut HashSet<NodeId>,
) -> bool {
    if !visited.insert(root) {
        return false;
    }
    match arena.get(root) {
        SLTNode::Input { index, .. } => index.iter().any(|index| {
            let mut index_inputs = HashSet::default();
            let mut index_visited = HashSet::default();
            collect_template_inputs(index.node, arena, &mut index_visited, &mut index_inputs)
                && index_inputs.contains(&loop_var)
        }),
        SLTNode::Binary(lhs, _, rhs) => {
            update_has_dynamic_loop_index(*lhs, loop_var, arena, visited)
                || update_has_dynamic_loop_index(*rhs, loop_var, arena, visited)
        }
        SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => {
            update_has_dynamic_loop_index(*inner, loop_var, arena, visited)
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            update_has_dynamic_loop_index(*cond, loop_var, arena, visited)
                || update_has_dynamic_loop_index(*then_expr, loop_var, arena, visited)
                || update_has_dynamic_loop_index(*else_expr, loop_var, arena, visited)
        }
        SLTNode::Concat(parts) => parts
            .iter()
            .any(|(part, _)| update_has_dynamic_loop_index(*part, loop_var, arena, visited)),
        SLTNode::Slice { expr, .. } => {
            update_has_dynamic_loop_index(*expr, loop_var, arena, visited)
        }
        SLTNode::Constant(..) | SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => false,
    }
}

fn collect_narrow_dynamic_input_widths(
    root: NodeId,
    loop_var: VarId,
    arena: &SLTNodeArena<VarId>,
    visited: &mut HashSet<NodeId>,
    widths: &mut BTreeSet<usize>,
) {
    if !visited.insert(root) {
        return;
    }
    match arena.get(root) {
        SLTNode::Input { index, .. } => {
            for entry in index {
                collect_narrow_dynamic_input_widths(entry.node, loop_var, arena, visited, widths);
            }
        }
        SLTNode::Slice { expr, access } => {
            if let SLTNode::Input {
                index,
                access: raw_access,
                ..
            } = arena.get(*expr)
                && !index.is_empty()
                && index.iter().any(|entry| {
                    let mut inputs = HashSet::default();
                    let mut index_visited = HashSet::default();
                    collect_template_inputs(entry.node, arena, &mut index_visited, &mut inputs)
                        && inputs.contains(&loop_var)
                })
            {
                let width = access.msb - access.lsb + 1;
                let raw_width = raw_access.msb - raw_access.lsb + 1;
                assert!(width < raw_width, "dynamic array read was not narrowed");
                widths.insert(width);
            }
            collect_narrow_dynamic_input_widths(*expr, loop_var, arena, visited, widths);
        }
        SLTNode::Binary(lhs, _, rhs) => {
            collect_narrow_dynamic_input_widths(*lhs, loop_var, arena, visited, widths);
            collect_narrow_dynamic_input_widths(*rhs, loop_var, arena, visited, widths);
        }
        SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => {
            collect_narrow_dynamic_input_widths(*inner, loop_var, arena, visited, widths)
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_narrow_dynamic_input_widths(*cond, loop_var, arena, visited, widths);
            collect_narrow_dynamic_input_widths(*then_expr, loop_var, arena, visited, widths);
            collect_narrow_dynamic_input_widths(*else_expr, loop_var, arena, visited, widths);
        }
        SLTNode::Concat(parts) => {
            for (part, _) in parts {
                collect_narrow_dynamic_input_widths(*part, loop_var, arena, visited, widths);
            }
        }
        SLTNode::Constant(..) | SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {}
    }
}
