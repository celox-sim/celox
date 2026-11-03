use std::collections::{BTreeMap, BTreeSet};

use num_bigint::BigUint;

use super::control_region_feasibility::{
    EffectCaseRewritePlan, ExecutableCaseRecipe, plan_best_effect_case_dispatch,
};
use super::shared::def_reg;
use crate::ir::{
    BasicBlock, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, RegisterType,
    SIRInstruction, SIRSwitchCase, SIRTerminator, SIRValue,
};
use crate::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::optimizer) struct EffectCaseDispatchResult {
    pub origin: BlockId,
    pub selector: RegisterId,
    pub explicit_cases: usize,
    pub sinks: usize,
    pub path_local_exits: usize,
    pub estimated_saving: usize,
    pub planning_ns: u128,
    pub rewrite_ns: u128,
}

pub(in crate::optimizer) fn run(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> Option<EffectCaseDispatchResult> {
    let planning_start = crate::timing::now();
    let plan = plan_best_effect_case_dispatch(eu)?;
    let planning_ns = planning_start.elapsed().as_nanos();
    let rewrite_start = crate::timing::now();
    let mut rewritten = eu.clone();
    apply_plan(eu, &mut rewritten, &plan)?;
    super::pass_vectorize_concat::remove_dead_definitions(&mut rewritten);
    rewritten.verify_result().ok()?;
    let result = EffectCaseDispatchResult {
        origin: plan.origin,
        selector: plan.selector,
        explicit_cases: plan.explicit_cases.len(),
        sinks: plan.sinks.len(),
        path_local_exits: plan.path_local_exits.len(),
        estimated_saving: plan.estimated_saving,
        planning_ns,
        rewrite_ns: rewrite_start.elapsed().as_nanos(),
    };
    *eu = rewritten;
    Some(result)
}

fn apply_plan(
    source: &ExecutionUnit<RegionedAbsoluteAddr>,
    rewritten: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: &EffectCaseRewritePlan,
) -> Option<()> {
    let mut next_block = source
        .blocks
        .keys()
        .map(|block| block.0)
        .max()
        .unwrap_or(0)
        .checked_add(1)?;
    let mut next_register = source
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0)
        .checked_add(1)?;
    let mut generated = Vec::new();

    for exit in &plan.path_local_exits {
        let original = source.blocks.get(&exit.insertion_block)?;
        let fallback = allocate_block(&mut next_block)?;
        let selected = allocate_block(&mut next_block)?;
        generated.push(BasicBlock {
            id: fallback,
            params: Vec::new(),
            instructions: original.instructions.clone(),
            terminator: original.terminator.clone(),
        });
        let mut instructions = Vec::new();
        let value = materialize_recipe(
            source,
            &exit.recipe,
            &mut next_register,
            &mut rewritten.register_map,
            &mut instructions,
        )?;
        instructions.push(clone_store_with_source(source, exit.sink, value)?);
        generated.push(BasicBlock {
            id: selected,
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Jump(exit.continuation, Vec::new()),
        });
        let insertion = rewritten.blocks.get_mut(&exit.insertion_block)?;
        insertion.instructions.clear();
        insertion.terminator = SIRTerminator::Branch {
            cond: exit.guard,
            true_block: (selected, Vec::new()),
            false_block: (fallback, Vec::new()),
        };
    }

    for sink in &plan.sinks {
        let mut cases = BTreeMap::<BigUint, BlockId>::new();
        let mut default = None;
        for recipe in &sink.cases {
            let target = allocate_block(&mut next_block)?;
            let mut instructions = Vec::new();
            let value = materialize_recipe(
                source,
                recipe,
                &mut next_register,
                &mut rewritten.register_map,
                &mut instructions,
            )?;
            instructions.push(clone_store_with_source(source, sink.sink, value)?);
            generated.push(BasicBlock {
                id: target,
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Jump(sink.continuation, Vec::new()),
            });
            if let Some(selected) = &recipe.selected_case {
                if cases.insert(selected.clone(), target).is_some() {
                    return None;
                }
            } else if default.replace(target).is_some() {
                return None;
            }
        }
        let default = default?;
        let block = rewritten.blocks.get_mut(&sink.sink.0)?;
        block.instructions.truncate(sink.sink.1);
        block.terminator = SIRTerminator::Switch {
            selector: plan.selector,
            cases: cases
                .into_iter()
                .map(|(value, target)| SIRSwitchCase { value, target })
                .collect(),
            default,
        };
    }
    rewritten
        .blocks
        .extend(generated.into_iter().map(|block| (block.id, block)));
    Some(())
}

fn clone_store_with_source(
    source: &ExecutionUnit<RegionedAbsoluteAddr>,
    sink: (BlockId, usize),
    value: RegisterId,
) -> Option<SIRInstruction<RegionedAbsoluteAddr>> {
    let SIRInstruction::Store(address, offset, width, _, triggers, capture_sites) =
        source.blocks.get(&sink.0)?.instructions.get(sink.1)?
    else {
        return None;
    };
    Some(SIRInstruction::Store(
        *address,
        offset.clone(),
        *width,
        value,
        triggers.clone(),
        capture_sites.clone(),
    ))
}

fn materialize_recipe(
    source: &ExecutionUnit<RegionedAbsoluteAddr>,
    recipe: &ExecutableCaseRecipe,
    next_register: &mut usize,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    output: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<RegisterId> {
    let mut cloned = HashMap::<RegisterId, RegisterId>::default();
    let mut known = HashMap::<RegisterId, RegisterId>::default();
    let planned = recipe
        .clone_order
        .iter()
        .map(|&(block, index)| def_reg(&source.blocks[&block].instructions[index]))
        .collect::<Option<HashSet<_>>>()?;
    for &(block, index) in &recipe.clone_order {
        let instruction = source.blocks.get(&block)?.instructions.get(index)?;
        let original_destination = def_reg(instruction)?;
        let destination =
            allocate_register_like(source, original_destination, next_register, register_map)?;
        let instruction = {
            let mut resolve = |register| {
                resolve_register(
                    source,
                    recipe,
                    register,
                    next_register,
                    register_map,
                    output,
                    &cloned,
                    &mut known,
                    &planned,
                )
            };
            remap_instruction(instruction, destination, &mut resolve)?
        };
        output.push(instruction);
        cloned.insert(original_destination, destination);
    }
    resolve_register(
        source,
        recipe,
        recipe.source,
        next_register,
        register_map,
        output,
        &cloned,
        &mut known,
        &planned,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_register(
    source: &ExecutionUnit<RegionedAbsoluteAddr>,
    recipe: &ExecutableCaseRecipe,
    mut register: RegisterId,
    next_register: &mut usize,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    output: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    cloned: &HashMap<RegisterId, RegisterId>,
    known: &mut HashMap<RegisterId, RegisterId>,
    planned: &HashSet<RegisterId>,
) -> Option<RegisterId> {
    let mut active = BTreeSet::new();
    while let Some(&alias) = recipe.aliases.get(&register) {
        if !active.insert(register) {
            return None;
        }
        register = alias;
    }
    if let Some(&replacement) = cloned.get(&register) {
        return Some(replacement);
    }
    if planned.contains(&register) {
        return None;
    }
    if let Some(&value) = recipe.known_values.get(&register) {
        if let Some(&replacement) = known.get(&register) {
            return Some(replacement);
        }
        let replacement = allocate_register_like(source, register, next_register, register_map)?;
        output.push(SIRInstruction::Imm(
            replacement,
            SIRValue::new(u8::from(value)),
        ));
        known.insert(register, replacement);
        return Some(replacement);
    }
    source
        .register_map
        .contains_key(&register)
        .then_some(register)
}

fn remap_instruction(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    destination: RegisterId,
    resolve: &mut impl FnMut(RegisterId) -> Option<RegisterId>,
) -> Option<SIRInstruction<RegionedAbsoluteAddr>> {
    if instruction.has_side_effects() {
        return None;
    }
    let mut remapped = instruction.clone();
    let mut resolved = true;
    remapped.for_each_use_mut(|register| {
        // Stop at the first operand that cannot be resolved, like `?` would.
        if !resolved {
            return;
        }
        match resolve(*register) {
            Some(replacement) => *register = replacement,
            None => resolved = false,
        }
    });
    if !resolved {
        return None;
    }
    *remapped.defined_register_mut()? = destination;
    Some(remapped)
}

fn allocate_block(next: &mut usize) -> Option<BlockId> {
    let block = BlockId(*next);
    *next = next.checked_add(1)?;
    Some(block)
}

fn allocate_register_like(
    source: &ExecutionUnit<RegionedAbsoluteAddr>,
    original: RegisterId,
    next: &mut usize,
    register_map: &mut HashMap<RegisterId, RegisterType>,
) -> Option<RegisterId> {
    let register = RegisterId(*next);
    *next = next.checked_add(1)?;
    register_map.insert(register, source.register_map.get(&original)?.clone());
    Some(register)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AbsoluteAddr, InstanceId, SIROffset, STABLE_REGION};
    use celox_design::StateObjectId as VarId;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn address() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw(0),
            },
        )
    }

    #[test]
    fn path_local_exit_preserves_the_merge_and_bypasses_the_final_dispatch() {
        let selector = RegisterId(0);
        let outer = RegisterId(1);
        let lhs = RegisterId(2);
        let rhs = RegisterId(3);
        let zero = RegisterId(4);
        let one = RegisterId(5);
        let guard_zero = RegisterId(6);
        let merged = RegisterId(7);
        let origin = BasicBlock {
            id: BlockId(0),
            params: vec![selector, outer, lhs, rhs],
            instructions: vec![
                SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                SIRInstruction::Imm(one, SIRValue::new(1u8)),
                SIRInstruction::Binary(guard_zero, selector, crate::ir::BinaryOp::Eq, zero),
            ],
            terminator: SIRTerminator::Branch {
                cond: outer,
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        };
        let true_arm = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), vec![lhs]),
        };
        let false_arm = BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), vec![rhs]),
        };
        let merge = BasicBlock {
            id: BlockId(3),
            params: vec![merged],
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(4), Vec::new()),
        };
        let sink = BasicBlock {
            id: BlockId(4),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Store(
                address(),
                SIROffset::Static(0),
                2,
                lhs,
                Vec::new(),
                Vec::new(),
            )],
            terminator: SIRTerminator::Jump(BlockId(5), Vec::new()),
        };
        let continuation = BasicBlock {
            id: BlockId(5),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        };
        let source = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                (BlockId(0), origin),
                (BlockId(1), true_arm),
                (BlockId(2), false_arm),
                (BlockId(3), merge),
                (BlockId(4), sink),
                (BlockId(5), continuation),
            ]
            .into_iter()
            .collect(),
            register_map: [
                (selector, bit(2)),
                (outer, bit(1)),
                (lhs, bit(2)),
                (rhs, bit(2)),
                (zero, bit(2)),
                (one, bit(2)),
                (guard_zero, bit(1)),
                (merged, bit(2)),
            ]
            .into_iter()
            .collect(),
        };
        let recipe = |selected_case, source| ExecutableCaseRecipe {
            selected_case,
            source,
            clone_order: Vec::new(),
            aliases: BTreeMap::new(),
            known_values: BTreeMap::new(),
        };
        let plan = EffectCaseRewritePlan {
            origin: BlockId(0),
            selector,
            explicit_cases: vec![BigUint::from(0u8), BigUint::from(1u8)],
            sinks: vec![
                super::super::control_region_feasibility::EffectSinkDispatchPlan {
                    sink: (BlockId(4), 0),
                    continuation: BlockId(5),
                    cases: vec![recipe(Some(BigUint::from(1u8)), rhs), recipe(None, lhs)],
                },
            ],
            path_local_exits: vec![
                super::super::control_region_feasibility::PathLocalEffectExitPlan {
                    sink: (BlockId(4), 0),
                    continuation: BlockId(5),
                    insertion_block: BlockId(3),
                    guard: guard_zero,
                    recipe: recipe(Some(BigUint::from(0u8)), merged),
                },
            ],
            estimated_saving: 100,
        };
        let mut rewritten = source.clone();

        apply_plan(&source, &mut rewritten, &plan).expect("the closed plan must apply");

        rewritten
            .verify_result()
            .expect("rewritten SIR must verify");
        assert!(matches!(
            rewritten.blocks[&BlockId(3)].terminator,
            SIRTerminator::Branch { cond, .. } if cond == guard_zero
        ));
        assert!(matches!(
            rewritten.blocks[&BlockId(4)].terminator,
            SIRTerminator::Switch { ref cases, .. } if cases.len() == 1
        ));
        assert_eq!(
            rewritten
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
                .count(),
            3
        );
    }
    // Exercise the real planner, including its profitability threshold, rather
    // than supplying an already-approved rewrite plan to apply_plan.
    fn path_local_exit_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
        use crate::ir::{BinaryOp, UnaryOp};
        let r = RegisterId;
        let mut origin = vec![
            SIRInstruction::Imm(r(5), SIRValue::new(0u8)),
            SIRInstruction::Imm(r(6), SIRValue::new(1u8)),
            SIRInstruction::Binary(r(7), r(0), BinaryOp::Eq, r(5)),
            SIRInstruction::Unary(r(8), UnaryOp::ToTwoState, r(7)),
            SIRInstruction::Binary(r(9), r(0), BinaryOp::Eq, r(6)),
        ];
        let mut register_map = (0..13)
            .map(|i| (r(i), bit(if [1, 7, 8, 9].contains(&i) { 1 } else { 2 })))
            .collect::<HashMap<_, _>>();
        // Expensive alternatives make case dispatch profitable.
        for (base, op) in [(100, BinaryOp::Add), (200, BinaryOp::Sub)] {
            for i in 0..16 {
                register_map.insert(r(base + i), bit(2));
                origin.push(SIRInstruction::Binary(
                    r(base + i),
                    if i == 0 { r(4) } else { r(base + i - 1) },
                    op,
                    r(4),
                ));
            }
        }
        origin.push(SIRInstruction::Mux(r(10), r(9), r(115), r(215)));
        let block = |id, params, instructions, terminator| {
            (
                BlockId(id),
                BasicBlock {
                    id: BlockId(id),
                    params,
                    instructions,
                    terminator,
                },
            )
        };
        ExecutionUnit {
            entry_block_id: BlockId(0),
            register_map,
            blocks: [
                block(
                    0,
                    (0..5).map(r).collect(),
                    origin,
                    SIRTerminator::Branch {
                        cond: r(8),
                        true_block: (BlockId(6), vec![]),
                        false_block: (BlockId(5), vec![]),
                    },
                ),
                block(
                    6,
                    vec![],
                    vec![],
                    SIRTerminator::Branch {
                        cond: r(1),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![],
                    vec![],
                    SIRTerminator::Jump(BlockId(3), vec![r(2)]),
                ),
                block(
                    2,
                    vec![],
                    vec![],
                    SIRTerminator::Jump(BlockId(3), vec![r(3)]),
                ),
                block(
                    3,
                    vec![r(11)],
                    vec![],
                    SIRTerminator::Jump(BlockId(8), vec![]),
                ),
                block(
                    8,
                    vec![],
                    vec![],
                    SIRTerminator::Jump(BlockId(4), vec![r(11)]),
                ),
                block(
                    5,
                    vec![],
                    vec![],
                    SIRTerminator::Jump(BlockId(4), vec![r(10)]),
                ),
                block(
                    4,
                    vec![r(12)],
                    vec![SIRInstruction::Store(
                        address(),
                        SIROffset::Static(0),
                        2,
                        r(12),
                        vec![],
                        vec![],
                    )],
                    SIRTerminator::Jump(BlockId(7), vec![]),
                ),
                block(7, vec![], vec![], SIRTerminator::Return),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn check_dispatch_semantics(
        original: &ExecutionUnit<RegionedAbsoluteAddr>,
        rewritten: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) {
        use crate::optimizer::passes::load_motion_tests::evaluate;
        for selector in 0..4 {
            for outer in 0..2 {
                let inputs = [selector, outer, 2, 3, 1];
                assert_eq!(
                    evaluate(original, &inputs, HashMap::default()),
                    evaluate(rewritten, &inputs, HashMap::default()),
                    "selector={selector}, outer={outer}"
                );
            }
        }
    }

    #[test]
    fn path_local_exit_keeps_effects_on_the_route_to_the_sink() {
        for id in [3, 8] {
            let mut unit = path_local_exit_unit();
            let mut other = address();
            other.instance_id = InstanceId(1);
            unit.blocks
                .get_mut(&BlockId(id))
                .unwrap()
                .instructions
                .push(SIRInstruction::Store(
                    other,
                    SIROffset::Static(0),
                    2,
                    RegisterId(11),
                    vec![],
                    vec![],
                ));
            let original = unit.clone();
            run(&mut unit);
            check_dispatch_semantics(&original, &unit);
        }
    }

    #[test]
    fn path_local_exit_requires_every_route_to_reach_the_sink() {
        let mut unit = path_local_exit_unit();
        unit.blocks.get_mut(&BlockId(8)).unwrap().terminator = SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(4), vec![RegisterId(11)]),
            false_block: (BlockId(7), vec![]),
        };
        let original = unit.clone();
        run(&mut unit);
        check_dispatch_semantics(&original, &unit);
    }

    #[test]
    fn path_local_exit_rejects_non_store_effects_on_the_route() {
        let effects = vec![
            SIRInstruction::Commit(address(), address(), SIROffset::Static(0), 2, vec![]),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(11)],
            },
            SIRInstruction::ExternCall {
                dst: None,
                func: 0,
                args: vec![],
            },
            SIRInstruction::Call {
                function: crate::ir::FunctionId(0),
                frame: RegisterId(4),
                args: vec![],
                outputs: vec![],
                dst: None,
                effects: std::sync::Arc::new(crate::ir::CallEffects::default()),
            },
        ];
        for id in [3, 8] {
            for effect in &effects {
                let mut unit = path_local_exit_unit();
                unit.blocks
                    .get_mut(&BlockId(id))
                    .unwrap()
                    .instructions
                    .push(effect.clone());
                // Check the planner directly: rejecting invalid rewritten SIR
                // would hide an unsound plan for an otherwise valid host call.
                assert!(
                    plan_best_effect_case_dispatch(&unit).is_none(),
                    "block={id}, {effect:?}"
                );
            }
        }
    }

    #[test]
    fn path_local_exit_dispatches_an_effect_free_route_to_the_sink() {
        let mut unit = path_local_exit_unit();
        unit.verify_result().unwrap();
        let original = unit.clone();
        assert_eq!(
            run(&mut unit)
                .expect("safe dispatch must still apply")
                .path_local_exits,
            1
        );
        check_dispatch_semantics(&original, &unit);
    }

    #[test]
    fn path_local_exit_rejects_cycles_and_error_routes() {
        for terminator in [
            SIRTerminator::Error(1),
            SIRTerminator::Jump(BlockId(8), vec![]),
        ] {
            let mut unit = path_local_exit_unit();
            unit.blocks.get_mut(&BlockId(8)).unwrap().terminator = SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(4), vec![RegisterId(11)]),
                false_block: (BlockId(9), vec![]),
            };
            unit.blocks.insert(
                BlockId(9),
                BasicBlock {
                    id: BlockId(9),
                    params: vec![],
                    instructions: vec![],
                    terminator,
                },
            );
            assert!(run(&mut unit).is_none());
        }
    }
}
