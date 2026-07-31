use std::collections::{BTreeMap, BTreeSet};

use num_bigint::BigUint;

use super::control_region_feasibility::{
    EffectCaseRewritePlan, ExecutableCaseRecipe, plan_best_effect_case_dispatch,
};
use super::shared::def_reg;
use crate::ir::{
    BasicBlock, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, RegisterType,
    SIRInstruction, SIROffset, SIRSwitchCase, SIRTerminator, SIRValue,
};
use crate::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EffectCaseDispatchResult {
    pub origin: BlockId,
    pub selector: RegisterId,
    pub explicit_cases: usize,
    pub sinks: usize,
    pub path_local_exits: usize,
    pub estimated_saving: usize,
    pub planning_ns: u128,
    pub rewrite_ns: u128,
}

pub(super) fn run(
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
    let offset = |offset: &SIROffset, resolve: &mut dyn FnMut(RegisterId) -> Option<RegisterId>| {
        Some(match offset {
            SIROffset::Static(offset) => SIROffset::Static(*offset),
            SIROffset::Dynamic(register) => SIROffset::Dynamic(resolve(*register)?),
            SIROffset::Element {
                index,
                element_width,
                bit_offset,
                dynamic_bit_offset,
            } => SIROffset::Element {
                index: resolve(*index)?,
                element_width: *element_width,
                bit_offset: *bit_offset,
                dynamic_bit_offset: match dynamic_bit_offset {
                    Some(register) => Some(resolve(*register)?),
                    None => None,
                },
            },
            SIROffset::PackedElements {
                bit_offset,
                element_width,
            } => SIROffset::PackedElements {
                bit_offset: *bit_offset,
                element_width: *element_width,
            },
        })
    };
    Some(match instruction {
        SIRInstruction::Imm(_, value) => SIRInstruction::Imm(destination, value.clone()),
        SIRInstruction::Load(_, address, source_offset, width) => SIRInstruction::Load(
            destination,
            *address,
            offset(source_offset, resolve)?,
            *width,
        ),
        SIRInstruction::Binary(_, lhs, operation, rhs) => {
            SIRInstruction::Binary(destination, resolve(*lhs)?, *operation, resolve(*rhs)?)
        }
        SIRInstruction::Unary(_, operation, source) => {
            SIRInstruction::Unary(destination, *operation, resolve(*source)?)
        }
        SIRInstruction::Concat(_, arguments) => SIRInstruction::Concat(
            destination,
            arguments
                .iter()
                .map(|&argument| resolve(argument))
                .collect::<Option<Vec<_>>>()?,
        ),
        SIRInstruction::Slice(_, source, source_offset, width) => {
            SIRInstruction::Slice(destination, resolve(*source)?, *source_offset, *width)
        }
        SIRInstruction::Mux(_, condition, true_value, false_value) => SIRInstruction::Mux(
            destination,
            resolve(*condition)?,
            resolve(*true_value)?,
            resolve(*false_value)?,
        ),
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => return None,
    })
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
    use crate::ir::{AbsoluteAddr, InstanceId, STABLE_REGION};
    use veryl_analyzer::ir::VarId;

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
}
