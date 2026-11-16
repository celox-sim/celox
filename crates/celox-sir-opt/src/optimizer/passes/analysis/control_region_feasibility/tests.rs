use super::*;
use crate::ir::{AbsoluteAddr, BasicBlock, InstanceId, RegisterType, SIROffset, STABLE_REGION};
use celox_design::StateObjectId as VarId;

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn logic(width: usize) -> RegisterType {
    RegisterType::Logic { width }
}

fn address(variable: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(
        STABLE_REGION,
        AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(variable),
        },
    )
}

/// Shared load versions decide every selection of loads as an analysis
/// of just those loads does, over random units with branches, a loop,
/// overlapping exact accesses, packed, dynamic and mistyped accesses.
#[test]
fn shared_load_versions_match_per_recipe_analysis() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    // 0 -> {1, 2} -> 3 -> {3, 4}
    let successors = [vec![1, 2], vec![3], vec![3], vec![3, 4], vec![]];
    let mut checked = 0;
    let mut stable = 0;
    for _ in 0..300 {
        let mut register_map = HashMap::default();
        let mut registers = 0;
        let mut register = |ty: Option<RegisterType>, map: &mut HashMap<_, _>| {
            registers += 1;
            let id = RegisterId(registers);
            if let Some(ty) = ty {
                map.insert(id, ty);
            }
            id
        };
        let condition = register(Some(bit(1)), &mut register_map);
        let index = register(Some(bit(2)), &mut register_map);
        let mut loads = Vec::new();
        let blocks = successors
            .iter()
            .enumerate()
            .map(|(block, successors)| {
                let mut instructions = Vec::new();
                for _ in 0..next(5) {
                    let width = if next(10) == 0 { 0 } else { 4 * (1 + next(2)) };
                    let bit_offset = 4 * next(3);
                    // Mostly well typed; sometimes the wrong width or no
                    // register type at all.
                    let ty = match next(8) {
                        0 => None,
                        1 => Some(bit(width + 1)),
                        2 => Some(logic(width)),
                        _ => Some(bit(width)),
                    };
                    let addr = address(next(2) as u32);
                    let value = register(ty, &mut register_map);
                    match next(6) {
                        0 | 1 => instructions.push(SIRInstruction::Store(
                            addr,
                            SIROffset::Static(bit_offset),
                            width,
                            value,
                            Vec::new(),
                            Vec::new(),
                        )),
                        2 => {
                            instructions.push(SIRInstruction::Load(
                                value,
                                addr,
                                SIROffset::PackedElements {
                                    bit_offset,
                                    element_width: 4,
                                },
                                width,
                            ));
                            loads.push(value);
                        }
                        3 if next(3) == 0 => {
                            instructions.push(SIRInstruction::Load(
                                value,
                                addr,
                                SIROffset::Dynamic(index),
                                width,
                            ));
                            loads.push(value);
                        }
                        _ => {
                            instructions.push(SIRInstruction::Load(
                                value,
                                addr,
                                SIROffset::Static(bit_offset),
                                width,
                            ));
                            loads.push(value);
                        }
                    }
                }
                let terminator = match successors.as_slice() {
                    [] => SIRTerminator::Return,
                    [target] => SIRTerminator::Jump(BlockId(*target), vec![]),
                    [then, otherwise] => SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(*then), vec![]),
                        false_block: (BlockId(*otherwise), vec![]),
                    },
                    _ => unreachable!(),
                };
                (
                    BlockId(block),
                    BasicBlock {
                        id: BlockId(block),
                        params: vec![],
                        instructions,
                        terminator,
                    },
                )
            })
            .collect();
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        let cfg = SirCfg::analyze_structure(&eu).unwrap();
        let definitions = collect_definition_sites(&eu);
        let mut versions = LoadVersions::new(&eu, &cfg);
        for _ in 0..12 {
            let selection = loads
                .iter()
                .copied()
                .filter(|_| next(3) == 0)
                .collect::<BTreeSet<_>>();
            for target in 0..successors.len() {
                let target = BlockId(target);
                let expected =
                    loads_are_stable_at_by_analysis(&eu, &cfg, &definitions, target, &selection);
                assert_eq!(
                    loads_are_stable_at(&eu, &cfg, &definitions, &mut versions, target, &selection),
                    expected,
                    "{selection:?} at {target:?} in {eu:?}"
                );
                checked += 1;
                stable += usize::from(expected && !selection.is_empty());
            }
        }
    }
    assert!(checked > 10_000, "{checked}");
    assert!(stable > 100, "{stable}");
}

#[test]
fn exact_case_guard_requires_the_adjacent_one_bit_normalization_chain() {
    let selector = RegisterId(0);
    let selected = RegisterId(1);
    let comparison = RegisterId(2);
    let reduction = RegisterId(3);
    let normalized = RegisterId(4);
    let unrelated = RegisterId(5);
    let block = BasicBlock {
        id: BlockId(0),
        params: vec![selector],
        instructions: vec![
            SIRInstruction::Imm(selected, SIRValue::new(3u8)),
            SIRInstruction::Binary(comparison, selector, BinaryOp::EqWildcard, selected),
            SIRInstruction::Unary(reduction, UnaryOp::Or, comparison),
            SIRInstruction::Unary(normalized, UnaryOp::ToTwoState, reduction),
        ],
        terminator: SIRTerminator::Return,
    };
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(BlockId(0), block)].into_iter().collect(),
        register_map: [
            (selector, logic(7)),
            (selected, logic(7)),
            (comparison, logic(1)),
            (reduction, logic(1)),
            (normalized, bit(1)),
            (unrelated, logic(1)),
        ]
        .into_iter()
        .collect(),
    };
    let constants = collect_exact_constants(&eu);

    assert_eq!(
        exact_case_guard(&eu, BlockId(0), selector, &BigUint::from(3u8), &constants,),
        Some(normalized)
    );

    eu.blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
        .insert(2, SIRInstruction::Imm(unrelated, SIRValue::new(0u8)));
    assert_eq!(
        exact_case_guard(&eu, BlockId(0), selector, &BigUint::from(3u8), &constants,),
        None
    );

    eu.blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions
        .remove(2);
    eu.register_map.insert(comparison, logic(7));
    assert_eq!(
        exact_case_guard(&eu, BlockId(0), selector, &BigUint::from(3u8), &constants,),
        None
    );
}

#[test]
fn exact_selector_specialization_skips_every_untaken_payload() {
    let selector = RegisterId(0);
    let zero = RegisterId(1);
    let one = RegisterId(2);
    let guard_zero = RegisterId(3);
    let guard_one = RegisterId(4);
    let payload_zero = RegisterId(5);
    let payload_one = RegisterId(6);
    let selected_zero = RegisterId(7);
    let result = RegisterId(8);
    let block = BasicBlock {
        id: BlockId(0),
        params: vec![selector],
        instructions: vec![
            SIRInstruction::Imm(zero, SIRValue::new(0u8)),
            SIRInstruction::Imm(one, SIRValue::new(1u8)),
            SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
            SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
            SIRInstruction::Binary(payload_zero, selector, BinaryOp::Mul, selector),
            SIRInstruction::Binary(payload_one, payload_zero, BinaryOp::Add, one),
            SIRInstruction::Mux(selected_zero, guard_zero, payload_zero, zero),
            SIRInstruction::Mux(result, guard_one, payload_one, selected_zero),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), vec![result]),
    };
    let exit = BasicBlock {
        id: BlockId(1),
        params: vec![RegisterId(9)],
        instructions: vec![SIRInstruction::Imm(RegisterId(10), SIRValue::new(3u8))],
        terminator: SIRTerminator::Return,
    };
    let eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(BlockId(0), block), (BlockId(1), exit)]
            .into_iter()
            .collect(),
        register_map: [
            (selector, bit(2)),
            (zero, bit(2)),
            (one, bit(2)),
            (guard_zero, bit(1)),
            (guard_one, bit(1)),
            (payload_zero, bit(2)),
            (payload_one, bit(2)),
            (selected_zero, bit(2)),
            (result, bit(2)),
            (RegisterId(9), bit(2)),
            (RegisterId(10), bit(2)),
        ]
        .into_iter()
        .collect(),
    };
    let report = analyze(&eu, &[(BlockId(0), 10), (BlockId(1), 10)]);
    assert_eq!(report.profitable_regions, 1);
    let fact = &report.facts[0];
    assert_eq!(fact.explicit_cases, 2);
    assert!(fact.minimum_skipped_instructions >= 2);
    assert!(fact.worst_case_cost < fact.baseline_cost);
    assert!(
        report.profile_weighted_selected_cost > (fact.baseline_cost as u128).saturating_mul(10)
    );
}

#[test]
fn selector_closure_crosses_block_parameters_and_names_exact_effect_sinks() {
    let selector = RegisterId(0);
    let zero = RegisterId(1);
    let one = RegisterId(2);
    let guard_zero = RegisterId(3);
    let guard_one = RegisterId(4);
    let first_value = RegisterId(5);
    let selected = RegisterId(6);
    let parameter = RegisterId(7);
    let result = RegisterId(8);
    let origin = BasicBlock {
        id: BlockId(0),
        params: vec![selector, first_value],
        instructions: vec![
            SIRInstruction::Imm(zero, SIRValue::new(0u8)),
            SIRInstruction::Imm(one, SIRValue::new(1u8)),
            SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
            SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
            SIRInstruction::Mux(selected, guard_zero, first_value, zero),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), vec![selected]),
    };
    let sink = BasicBlock {
        id: BlockId(1),
        params: vec![parameter],
        instructions: vec![
            SIRInstruction::Mux(result, guard_one, one, parameter),
            SIRInstruction::Store(
                address(0),
                SIROffset::Static(0),
                2,
                result,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Return,
    };
    let eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(BlockId(0), origin), (BlockId(1), sink)]
            .into_iter()
            .collect(),
        register_map: [
            (selector, bit(2)),
            (zero, bit(2)),
            (one, bit(2)),
            (guard_zero, bit(1)),
            (guard_one, bit(1)),
            (first_value, bit(2)),
            (selected, bit(2)),
            (parameter, bit(2)),
            (result, bit(2)),
        ]
        .into_iter()
        .collect(),
    };

    let report = analyze(&eu, &[(BlockId(0), 10)]);
    let fact = &report.facts[0];
    assert_eq!(fact.cross_block_affected_blocks, 2);
    assert_eq!(fact.cross_block_effect_sinks, 1);
    assert_eq!(fact.cross_block_effect_sites, vec![(BlockId(1), 1)]);
    assert_eq!(fact.sink_recipes.len(), 1);
    let recipe = &fact.sink_recipes[0];
    assert_eq!(recipe.sink, (BlockId(1), 1));
    assert_eq!(recipe.source, result);
    assert_eq!(recipe.recipe_instructions.len(), 4);
    assert_eq!(recipe.recipe_blocks, vec![BlockId(0), BlockId(1)]);
    assert_eq!(recipe.constant_frontier.len(), 2);
    assert_eq!(recipe.external_frontier.len(), 2);
    assert_eq!(recipe.control_merges, vec![parameter]);
    assert!(recipe.loop_cutoffs.is_empty());
    assert_eq!(recipe.case_summary.alternatives, 3);
    assert_eq!(recipe.case_summary.reachable_alternatives, 3);
    assert_eq!(recipe.case_summary.maximum_instructions, 0);
    assert_eq!(recipe.case_summary.maximum_dominating_ssa_frontier, 1);
    assert_eq!(recipe.case_summary.maximum_external_frontier, 0);
}

#[test]
fn sink_recipe_stops_at_executable_load_and_shared_ssa_frontiers() {
    let selector = RegisterId(0);
    let zero = RegisterId(1);
    let one = RegisterId(2);
    let guard_zero = RegisterId(3);
    let guard_one = RegisterId(4);
    let external = RegisterId(5);
    let shared = RegisterId(6);
    let loaded = RegisterId(7);
    let selected = RegisterId(8);
    let parameter = RegisterId(9);
    let result = RegisterId(10);
    let preheader = BasicBlock {
        id: BlockId(2),
        params: vec![selector, external],
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(0), Vec::new()),
    };
    let origin = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(zero, SIRValue::new(0u8)),
            SIRInstruction::Imm(one, SIRValue::new(1u8)),
            SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
            SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
            SIRInstruction::Binary(shared, external, BinaryOp::Add, one),
            SIRInstruction::Load(loaded, address(1), SIROffset::Static(0), 2),
            SIRInstruction::Mux(selected, guard_zero, loaded, shared),
            SIRInstruction::Store(
                address(2),
                SIROffset::Static(0),
                2,
                shared,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), vec![selected]),
    };
    let sink = BasicBlock {
        id: BlockId(1),
        params: vec![parameter],
        instructions: vec![
            SIRInstruction::Mux(result, guard_one, one, parameter),
            SIRInstruction::Store(
                address(0),
                SIROffset::Static(0),
                2,
                result,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
    };
    let continuation = BasicBlock {
        id: BlockId(3),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Return,
    };
    let eu = ExecutionUnit {
        entry_block_id: BlockId(2),
        blocks: [
            (BlockId(0), origin),
            (BlockId(1), sink),
            (BlockId(2), preheader),
            (BlockId(3), continuation),
        ]
        .into_iter()
        .collect(),
        register_map: [
            (selector, bit(2)),
            (zero, bit(2)),
            (one, bit(2)),
            (guard_zero, bit(1)),
            (guard_one, bit(1)),
            (external, bit(2)),
            (shared, bit(2)),
            (loaded, bit(2)),
            (selected, bit(2)),
            (parameter, bit(2)),
            (result, bit(2)),
        ]
        .into_iter()
        .collect(),
    };

    let report = analyze(&eu, &[(BlockId(0), 10)]);
    let recipe = &report.facts[0].sink_recipes[0];
    assert_eq!(recipe.load_frontier, vec![loaded]);
    assert_eq!(recipe.shared_ssa_frontier, vec![shared]);
    assert_eq!(recipe.entering_edges, vec![(BlockId(2), BlockId(0))]);
    assert_eq!(recipe.continuations, vec![BlockId(3)]);
    assert!(recipe.effect.contains("src_reg = 10"));
    assert_eq!(recipe.case_summary.alternatives, 3);
    assert_eq!(recipe.case_summary.maximum_instructions, 1);
    assert_eq!(recipe.case_summary.maximum_load_frontier, 1);
    assert_eq!(recipe.case_summary.maximum_dominating_ssa_frontier, 1);
    assert_eq!(recipe.case_summary.maximum_external_frontier, 0);
    assert_eq!(recipe.case_summary.stable_load_frontier, vec![loaded]);
    assert!(recipe.case_summary.unstable_load_frontier.is_empty());
    assert_eq!(recipe.case_summary.maximum_unstable_loads_per_case, 0);
}

#[test]
fn case_recipe_preserves_unknown_outer_control_as_a_merge_leaf() {
    let selector = RegisterId(0);
    let outer = RegisterId(1);
    let true_value = RegisterId(2);
    let false_value = RegisterId(3);
    let zero = RegisterId(4);
    let one = RegisterId(5);
    let guard_zero = RegisterId(6);
    let guard_one = RegisterId(7);
    let parameter = RegisterId(8);
    let result = RegisterId(9);
    let origin = BasicBlock {
        id: BlockId(0),
        params: vec![selector, outer, true_value, false_value],
        instructions: vec![
            SIRInstruction::Imm(zero, SIRValue::new(0u8)),
            SIRInstruction::Imm(one, SIRValue::new(1u8)),
            SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
            SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
        ],
        terminator: SIRTerminator::Branch {
            cond: outer,
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    };
    let true_path = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(3), vec![true_value]),
    };
    let false_path = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(3), vec![false_value]),
    };
    let sink = BasicBlock {
        id: BlockId(3),
        params: vec![parameter],
        instructions: vec![
            SIRInstruction::Mux(result, guard_zero, parameter, zero),
            SIRInstruction::Store(
                address(0),
                SIROffset::Static(0),
                2,
                result,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Return,
    };
    let eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            (BlockId(0), origin),
            (BlockId(1), true_path),
            (BlockId(2), false_path),
            (BlockId(3), sink),
        ]
        .into_iter()
        .collect(),
        register_map: [
            (selector, bit(2)),
            (outer, bit(1)),
            (true_value, bit(2)),
            (false_value, bit(2)),
            (zero, bit(2)),
            (one, bit(2)),
            (guard_zero, bit(1)),
            (guard_one, bit(1)),
            (parameter, bit(2)),
            (result, bit(2)),
        ]
        .into_iter()
        .collect(),
    };

    let report = analyze(&eu, &[(BlockId(0), 10)]);
    let summary = &report.facts[0].sink_recipes[0].case_summary;
    assert_eq!(summary.all_control_merges, vec![parameter]);
    assert!(summary.all_external_frontier.is_empty());
}

#[test]
fn case_recipe_does_not_treat_a_path_local_merge_as_a_sink_leaf() {
    let selector = RegisterId(0);
    let outer = RegisterId(1);
    let true_value = RegisterId(2);
    let false_value = RegisterId(3);
    let fallback = RegisterId(4);
    let zero = RegisterId(5);
    let one = RegisterId(6);
    let guard_zero = RegisterId(7);
    let guard_one = RegisterId(8);
    let path_local = RegisterId(9);
    let joined = RegisterId(10);
    let combined = RegisterId(11);
    let result = RegisterId(12);
    let loaded = RegisterId(13);
    let origin = BasicBlock {
        id: BlockId(0),
        params: vec![selector, outer, true_value, false_value, fallback],
        instructions: vec![
            SIRInstruction::Imm(zero, SIRValue::new(0u8)),
            SIRInstruction::Imm(one, SIRValue::new(1u8)),
            SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
            SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
            SIRInstruction::Load(loaded, address(1), SIROffset::Static(0), 2),
        ],
        terminator: SIRTerminator::Branch {
            cond: guard_zero,
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    };
    let selected_path = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Branch {
            cond: outer,
            true_block: (BlockId(3), Vec::new()),
            false_block: (BlockId(4), Vec::new()),
        },
    };
    let default_path = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(6), vec![fallback]),
    };
    let true_path = BasicBlock {
        id: BlockId(3),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(5), vec![true_value]),
    };
    let false_path = BasicBlock {
        id: BlockId(4),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(5), vec![false_value]),
    };
    let path_merge = BasicBlock {
        id: BlockId(5),
        params: vec![path_local],
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(6), vec![path_local]),
    };
    let sink = BasicBlock {
        id: BlockId(6),
        params: vec![joined],
        instructions: vec![
            SIRInstruction::Binary(combined, joined, BinaryOp::Add, loaded),
            SIRInstruction::Mux(result, guard_one, one, combined),
            SIRInstruction::Store(
                address(0),
                SIROffset::Static(0),
                2,
                result,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Return,
    };
    let eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            (BlockId(0), origin),
            (BlockId(1), selected_path),
            (BlockId(2), default_path),
            (BlockId(3), true_path),
            (BlockId(4), false_path),
            (BlockId(5), path_merge),
            (BlockId(6), sink),
        ]
        .into_iter()
        .collect(),
        register_map: [
            (selector, bit(2)),
            (outer, bit(1)),
            (true_value, bit(2)),
            (false_value, bit(2)),
            (fallback, bit(2)),
            (zero, bit(2)),
            (one, bit(2)),
            (guard_zero, bit(1)),
            (guard_one, bit(1)),
            (path_local, bit(2)),
            (joined, bit(2)),
            (combined, bit(2)),
            (result, bit(2)),
            (loaded, bit(2)),
        ]
        .into_iter()
        .collect(),
    };

    let report = analyze(&eu, &[(BlockId(0), 10)]);
    let summary = &report.facts[0].sink_recipes[0].case_summary;
    assert_eq!(summary.all_non_dominating_control_merges, vec![path_local]);
    assert_eq!(
        summary.non_dominating_control_merge_cases,
        vec![("0".to_owned(), vec![path_local])]
    );
    assert_eq!(
        summary.path_local_placements,
        vec![PathLocalPlacementFact {
            case: "0".to_owned(),
            merge: path_local,
            insertion_block: BlockId(5),
            load_frontier: vec![loaded],
            unavailable_ssa_frontier: Vec::new(),
            stable_load_frontier: vec![loaded],
            unstable_load_frontier: Vec::new(),
            unversioned_load_frontier: Vec::new(),
        }]
    );
    assert!(summary.all_control_merges.is_empty());
}

#[test]
fn case_recipe_rejects_reloading_a_changed_state_version() {
    let selector = RegisterId(0);
    let replacement = RegisterId(1);
    let zero = RegisterId(2);
    let one = RegisterId(3);
    let guard_zero = RegisterId(4);
    let guard_one = RegisterId(5);
    let loaded = RegisterId(6);
    let selected = RegisterId(7);
    let parameter = RegisterId(8);
    let result = RegisterId(9);
    let origin = BasicBlock {
        id: BlockId(0),
        params: vec![selector, replacement],
        instructions: vec![
            SIRInstruction::Imm(zero, SIRValue::new(0u8)),
            SIRInstruction::Imm(one, SIRValue::new(1u8)),
            SIRInstruction::Binary(guard_zero, selector, BinaryOp::Eq, zero),
            SIRInstruction::Binary(guard_one, selector, BinaryOp::Eq, one),
            SIRInstruction::Load(loaded, address(1), SIROffset::Static(0), 2),
            SIRInstruction::Mux(selected, guard_zero, loaded, zero),
            SIRInstruction::Store(
                address(1),
                SIROffset::Static(0),
                2,
                replacement,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), vec![selected]),
    };
    let sink = BasicBlock {
        id: BlockId(1),
        params: vec![parameter],
        instructions: vec![
            SIRInstruction::Mux(result, guard_one, one, parameter),
            SIRInstruction::Store(
                address(0),
                SIROffset::Static(0),
                2,
                result,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Return,
    };
    let eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(BlockId(0), origin), (BlockId(1), sink)]
            .into_iter()
            .collect(),
        register_map: [
            (selector, bit(2)),
            (replacement, bit(2)),
            (zero, bit(2)),
            (one, bit(2)),
            (guard_zero, bit(1)),
            (guard_one, bit(1)),
            (loaded, bit(2)),
            (selected, bit(2)),
            (parameter, bit(2)),
            (result, bit(2)),
        ]
        .into_iter()
        .collect(),
    };

    let report = analyze(&eu, &[(BlockId(0), 10)]);
    let summary = &report.facts[0].sink_recipes[0].case_summary;
    assert!(summary.stable_load_frontier.is_empty());
    assert_eq!(summary.unstable_load_frontier, vec![loaded]);
    assert_eq!(summary.maximum_unstable_loads_per_case, 1);
    assert!(summary.unversioned_load_frontier.is_empty());
}

#[test]
fn sink_recipe_pair_separates_shared_and_path_local_work() {
    let recipe = |sink: (BlockId, usize),
                  source: RegisterId,
                  instructions: Vec<(BlockId, usize)>,
                  frontier: RegisterId| SelectorSinkRecipeFact {
        sink,
        effect: format!("store from r{}", source.0),
        publication: "same exact range".to_string(),
        source,
        recipe_instructions: instructions,
        recipe_blocks: vec![BlockId(0), sink.0],
        selector_control_blocks: vec![BlockId(0)],
        entering_edges: Vec::new(),
        continuations: vec![BlockId(3)],
        constant_frontier: vec![frontier],
        load_frontier: Vec::new(),
        shared_ssa_frontier: Vec::new(),
        external_frontier: Vec::new(),
        control_merges: Vec::new(),
        loop_cutoffs: Vec::new(),
        case_summary: SelectorSinkCaseSummaryFact::default(),
    };
    let left = recipe(
        (BlockId(1), 0),
        RegisterId(10),
        vec![(BlockId(0), 0), (BlockId(1), 0)],
        RegisterId(20),
    );
    let mut right = recipe(
        (BlockId(2), 0),
        RegisterId(11),
        vec![(BlockId(0), 0), (BlockId(2), 0), (BlockId(2), 1)],
        RegisterId(20),
    );
    right.recipe_blocks = vec![BlockId(0), BlockId(2)];

    let pairs = compare_sink_recipes(&[left, right], None);

    assert_eq!(
        pairs,
        vec![SelectorSinkRecipePairFact {
            left_sink: (BlockId(1), 0),
            right_sink: (BlockId(2), 0),
            same_publication: true,
            same_continuation: true,
            common_dominator: None,
            common_postdominator: None,
            common_instructions: 1,
            left_only_instructions: 1,
            right_only_instructions: 2,
            common_blocks: 1,
            common_frontier_values: 1,
        }]
    );
}
