//! If-conversion of small pure diamonds into `Mux`es.
//!
//! The frontends lower an `if`/`else` of a clocked process to a diamond whose
//! arms compute the assigned values and either store them or pass them to the
//! join block as block arguments; a reset or enable condition produces a chain
//! of such diamonds, one per assigned variable. Each costs a compare, two
//! jumps and three code cache lines for a few instructions of work, and the
//! chains made the clocked region of large designs bound by the instruction
//! cache.
//!
//! When both arms are small and have no effect beyond their values, this pass
//! runs them unconditionally, selects each join argument and each stored
//! value with a `Mux`, and merges the join into the branching block, so a
//! chain becomes straight-line code. A SIR `Branch` condition is a canonical
//! two-state bit, so a `Mux` on it selects exactly the arm the branch would
//! have taken, in four-state mode too. Stores convert only when both arms
//! write the same ranges in the same order without triggers or captures, and
//! no arm reads a range either arm writes. Arms that interact with the host,
//! call, or access state through a dynamic offset stay branches, as do arms
//! above the size budget.

use super::pass_manager::ExecutionUnitPass;
use crate::HashMap;
use crate::PassOptions;
use crate::ir::*;

pub(in crate::optimizer) struct IfConversionPass;

/// The largest cost of a converted diamond: the arm instructions that now run
/// unconditionally, plus one per 64-bit chunk of each selected value.
pub(in crate::optimizer) const MAX_COST: usize = 24;

impl ExecutionUnitPass for IfConversionPass {
    fn name(&self) -> &'static str {
        "if_conversion"
    }

    /// The moved instructions stay between the same state accesses, and a
    /// call is never moved, so a call elsewhere in the unit does not matter.
    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let blocks = eu.blocks.len();
        let converted = if_convert(eu, MAX_COST);
        if converted != 0 {
            tracing::debug!(
                "[if_conversion] converted {converted} diamonds, {blocks} -> {} blocks",
                eu.blocks.len()
            );
        }
    }
}

/// A constant range an arm stores.
#[derive(Clone, PartialEq, Eq)]
struct StoreTarget {
    addr: RegionedAbsoluteAddr,
    offset: SIROffset,
    width: usize,
}

impl StoreTarget {
    fn bit_offset(&self) -> usize {
        self.offset
            .constant_bit_offset()
            .expect("a store target has a constant offset")
    }
}

/// One side of a branch: either a block that only the branch reaches and
/// that jumps on to the join, or the edge straight to the join.
struct Arm {
    block: Option<BlockId>,
    join: BlockId,
    args: Vec<RegisterId>,
    /// The stores of the block in order, with their values.
    stores: Vec<(StoreTarget, RegisterId)>,
    cost: usize,
}

struct Plan {
    pred: BlockId,
    cond: RegisterId,
    then_arm: Arm,
    else_arm: Arm,
    /// Whether the diamond's edges are the join's only predecessors, so the
    /// join merges into the branching block; otherwise the block jumps to
    /// the join with the selected values.
    merge: bool,
}

/// Converts every diamond under `max_cost` to fixpoint and returns how many
/// branches it removed.
pub(in crate::optimizer) fn if_convert(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    max_cost: usize,
) -> usize {
    let mut converted = 0;
    loop {
        let predecessors = predecessor_edge_counts(eu);
        let mut branching: Vec<BlockId> = eu
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .map(|block| block.id)
            .collect();
        branching.sort();
        let before = converted;
        for pred in branching {
            // Only blocks removed by an earlier conversion are missing. The
            // counts stay exact: a removed arm or join was reached only from
            // the diamond, and the join's outgoing edges now leave the
            // merged block instead.
            if !eu.blocks.contains_key(&pred) {
                continue;
            }
            if let Some(plan) = plan(eu, &predecessors, pred, max_cost) {
                apply(eu, plan);
                converted += 1;
            }
        }
        if converted == before {
            return converted;
        }
    }
}

fn predecessor_edge_counts(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<BlockId, usize> {
    let mut counts: HashMap<BlockId, usize> = HashMap::default();
    for block in eu.blocks.values() {
        for successor in cfg::terminator_successors(&block.terminator) {
            *counts.entry(successor).or_default() += 1;
        }
    }
    counts
}

/// Whether running the instruction when its branch arm is not taken is
/// unobservable: it neither writes state nor interacts with the host, and a
/// load it makes has a constant address.
fn speculable(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    match inst {
        SIRInstruction::Load(_, _, offset, _) => !offset.is_dynamic(),
        inst => inst.is_pure(),
    }
}

/// The store's target and value when it is a plain write of a constant
/// range, which a `Mux` of the arms' values can replace.
fn plain_store(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<(StoreTarget, RegisterId)> {
    match inst {
        SIRInstruction::Store(addr, offset, width, src, triggers, comb_capture_sites)
            if !offset.is_dynamic() && triggers.is_empty() && comb_capture_sites.is_empty() =>
        {
            Some((
                StoreTarget {
                    addr: *addr,
                    offset: offset.clone(),
                    width: *width,
                },
                *src,
            ))
        }
        _ => None,
    }
}

fn arm(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    predecessors: &HashMap<BlockId, usize>,
    pred: BlockId,
    target: BlockId,
    args: &[RegisterId],
) -> Option<Arm> {
    let direct = Arm {
        block: None,
        join: target,
        args: args.to_vec(),
        stores: Vec::new(),
        cost: 0,
    };
    if !args.is_empty() || target == eu.entry_block_id {
        return Some(direct);
    }
    let block = &eu.blocks[&target];
    let SIRTerminator::Jump(join, join_args) = &block.terminator else {
        return Some(direct);
    };
    if predecessors.get(&target) != Some(&1) || target == pred || *join == target {
        return Some(direct);
    }
    debug_assert!(
        block.params.is_empty(),
        "a block entered without arguments has no parameters"
    );
    let mut stores = Vec::new();
    for inst in &block.instructions {
        if let Some(store) = plain_store(inst) {
            stores.push(store);
        } else if !speculable(inst) {
            return None;
        }
    }
    Some(Arm {
        block: Some(target),
        join: *join,
        args: join_args.clone(),
        stores,
        cost: block.instructions.len(),
    })
}

/// Whether a load of either arm may read a range the arms store, which the
/// stores' move past the loads would change.
fn loads_overlap_stores(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    arms: [&Arm; 2],
    targets: &[StoreTarget],
) -> bool {
    arms.iter()
        .filter_map(|arm| arm.block)
        .flat_map(|block| &eu.blocks[&block].instructions)
        .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
        .any(|load| {
            targets.iter().any(|target| {
                load.state_reads()
                    .overlaps(&target.addr, target.bit_offset(), target.width)
            })
        })
}

fn chunks(eu: &ExecutionUnit<RegionedAbsoluteAddr>, register: RegisterId) -> usize {
    eu.register_map[&register].width().div_ceil(64)
}

fn plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    predecessors: &HashMap<BlockId, usize>,
    pred: BlockId,
    max_cost: usize,
) -> Option<Plan> {
    let SIRTerminator::Branch {
        cond,
        true_block: (then_target, then_args),
        false_block: (else_target, else_args),
    } = &eu.blocks[&pred].terminator
    else {
        return None;
    };
    if then_target == else_target
        || !matches!(
            eu.register_map.get(cond),
            Some(RegisterType::Bit { width: 1, .. })
        )
    {
        return None;
    }
    let then_arm = arm(eu, predecessors, pred, *then_target, then_args)?;
    let else_arm = arm(eu, predecessors, pred, *else_target, else_args)?;
    let join = then_arm.join;
    if join != else_arm.join
        || join == pred
        || join == eu.entry_block_id
        || (then_arm.block.is_none() && else_arm.block.is_none())
    {
        return None;
    }
    // A stale count is only ever too high (a conversion earlier in the
    // iteration removed one of the join's edges), which forgoes a merge
    // until the next iteration, never merges a join another block reaches.
    let merge = predecessors.get(&join) == Some(&2);
    let join_block = &eu.blocks[&join];
    if join_block.params.len() != then_arm.args.len()
        || join_block.params.len() != else_arm.args.len()
    {
        return None;
    }
    let targets: Vec<StoreTarget> = then_arm
        .stores
        .iter()
        .map(|(target, _)| target.clone())
        .collect();
    if then_arm.stores.len() != else_arm.stores.len()
        || then_arm.stores.iter().zip(&else_arm.stores).any(
            |((then_target, then_value), (else_target, else_value))| {
                then_target != else_target
                    || eu.register_map[then_value].width() != eu.register_map[else_value].width()
            },
        )
        || loads_overlap_stores(eu, [&then_arm, &else_arm], &targets)
    {
        return None;
    }
    let selected = join_block
        .params
        .iter()
        .zip(then_arm.args.iter().zip(&else_arm.args))
        .filter(|(_, (then_value, else_value))| then_value != else_value)
        .map(|(param, _)| chunks(eu, *param))
        .chain(
            then_arm
                .stores
                .iter()
                .zip(&else_arm.stores)
                .filter(|((_, then_value), (_, else_value))| then_value != else_value)
                .map(|((_, then_value), _)| chunks(eu, *then_value)),
        )
        .sum::<usize>();
    if then_arm.cost + else_arm.cost + selected > max_cost {
        return None;
    }
    Some(Plan {
        pred,
        cond: *cond,
        then_arm,
        else_arm,
        merge,
    })
}

fn apply(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, plan: Plan) {
    let Plan {
        pred,
        cond,
        then_arm,
        else_arm,
        merge,
    } = plan;
    let mut instructions = Vec::new();
    for arm in [&then_arm, &else_arm] {
        if let Some(block) = arm.block {
            let block = eu.blocks.remove(&block).expect("arm block exists");
            instructions.extend(
                block
                    .instructions
                    .into_iter()
                    .filter(|inst| !matches!(inst, SIRInstruction::Store(..))),
            );
        }
    }
    let mut next_register = eu.register_map.keys().map(|r| r.0 + 1).max().unwrap_or(0);
    let join_params = eu.blocks[&then_arm.join].params.clone();
    let mut join_args = Vec::with_capacity(join_params.len());
    for (param, (then_value, else_value)) in join_params
        .into_iter()
        .zip(then_arm.args.into_iter().zip(else_arm.args))
    {
        if then_value == else_value {
            if merge {
                instructions.push(SIRInstruction::Unary(param, UnaryOp::Ident, then_value));
            }
            join_args.push(then_value);
        } else {
            let selected = if merge {
                param
            } else {
                let selected = RegisterId(next_register);
                next_register += 1;
                eu.register_map
                    .insert(selected, eu.register_map[&param].clone());
                selected
            };
            instructions.push(SIRInstruction::Mux(selected, cond, then_value, else_value));
            join_args.push(selected);
        }
    }
    for ((target, then_value), (_, else_value)) in then_arm.stores.into_iter().zip(else_arm.stores)
    {
        let value = if then_value == else_value {
            then_value
        } else {
            let value = RegisterId(next_register);
            next_register += 1;
            // The selected value keeps the arms' masks, so it is four-state
            // unless both arms are two-state.
            let ty = match (&eu.register_map[&then_value], &eu.register_map[&else_value]) {
                (ty @ RegisterType::Bit { .. }, RegisterType::Bit { .. }) => ty.clone(),
                (ty, _) => RegisterType::Logic { width: ty.width() },
            };
            eu.register_map.insert(value, ty);
            instructions.push(SIRInstruction::Mux(value, cond, then_value, else_value));
            value
        };
        instructions.push(SIRInstruction::Store(
            target.addr,
            target.offset,
            target.width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }
    let terminator = if merge {
        let join = eu.blocks.remove(&then_arm.join).expect("join block exists");
        instructions.extend(join.instructions);
        join.terminator
    } else {
        SIRTerminator::Jump(then_arm.join, join_args)
    };
    let pred = eu.blocks.get_mut(&pred).expect("branching block exists");
    pred.instructions.extend(instructions);
    pred.terminator = terminator;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(id: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(id),
            var_id: celox_design::StateObjectId::default(),
        }
    }

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn block(
        id: usize,
        params: Vec<usize>,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        terminator: SIRTerminator,
    ) -> (BlockId, BasicBlock<RegionedAbsoluteAddr>) {
        (
            BlockId(id),
            BasicBlock {
                id: BlockId(id),
                params: params.into_iter().map(RegisterId).collect(),
                instructions,
                terminator,
            },
        )
    }

    fn load(dst: usize, var: usize, width: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Load(RegisterId(dst), addr(var), SIROffset::Static(0), width)
    }

    fn store(var: usize, src: usize, width: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            addr(var),
            SIROffset::Static(0),
            width,
            RegisterId(src),
            vec![],
            vec![],
        )
    }

    fn imm(dst: usize, value: u32) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Imm(RegisterId(dst), SIRValue::new(value))
    }

    fn mux(
        dst: usize,
        cond: usize,
        then: usize,
        otherwise: usize,
    ) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Mux(
            RegisterId(dst),
            RegisterId(cond),
            RegisterId(then),
            RegisterId(otherwise),
        )
    }

    fn branch(cond: usize, then_block: usize, else_block: usize) -> SIRTerminator {
        SIRTerminator::Branch {
            cond: RegisterId(cond),
            true_block: (BlockId(then_block), vec![]),
            false_block: (BlockId(else_block), vec![]),
        }
    }

    fn jump(target: usize, args: Vec<usize>) -> SIRTerminator {
        SIRTerminator::Jump(BlockId(target), args.into_iter().map(RegisterId).collect())
    }

    /// `if (c) q = 0; else q = d;` with the value passed to the join, which
    /// stores it.
    fn reset_diamond() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        register_map.insert(RegisterId(1), bit(8));
        register_map.insert(RegisterId(2), bit(8));
        register_map.insert(RegisterId(3), bit(8));
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: HashMap::from_iter([
                block(0, vec![], vec![load(0, 1, 1)], branch(0, 1, 2)),
                block(1, vec![], vec![imm(1, 0)], jump(3, vec![1])),
                block(2, vec![], vec![load(2, 2, 8)], jump(3, vec![2])),
                block(3, vec![3], vec![store(3, 3, 8)], SIRTerminator::Return),
            ]),
            register_map,
        }
    }

    /// `if (c) q = 0; else q = d;` with a store in each arm.
    fn stored_diamond() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut eu = reset_diamond();
        eu.register_map.remove(&RegisterId(3));
        eu.blocks.insert(
            BlockId(1),
            block(1, vec![], vec![imm(1, 0), store(3, 1, 8)], jump(3, vec![])).1,
        );
        eu.blocks.insert(
            BlockId(2),
            block(
                2,
                vec![],
                vec![load(2, 2, 8), store(3, 2, 8)],
                jump(3, vec![]),
            )
            .1,
        );
        eu.blocks.insert(
            BlockId(3),
            block(3, vec![], vec![], SIRTerminator::Return).1,
        );
        eu
    }

    #[test]
    fn converts_a_pure_diamond_to_a_mux_and_merges_the_join() {
        let mut eu = reset_diamond();
        assert_eq!(if_convert(&mut eu, MAX_COST), 1);
        assert_eq!(eu.blocks.len(), 1);
        let entry = &eu.blocks[&BlockId(0)];
        assert_eq!(entry.terminator, SIRTerminator::Return);
        assert_eq!(
            entry.instructions,
            vec![
                load(0, 1, 1),
                imm(1, 0),
                load(2, 2, 8),
                mux(3, 0, 1, 2),
                store(3, 3, 8),
            ]
        );
    }

    #[test]
    fn converts_a_half_diamond_whose_other_edge_reaches_the_join_directly() {
        let mut eu = reset_diamond();
        // `if (c) q = 0;` with the old value passed on the else edge.
        eu.blocks.remove(&BlockId(2));
        let entry = eu.blocks.get_mut(&BlockId(0)).unwrap();
        entry.instructions.push(load(2, 2, 8));
        entry.terminator = SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), vec![]),
            false_block: (BlockId(3), vec![RegisterId(2)]),
        };
        assert_eq!(if_convert(&mut eu, MAX_COST), 1);
        assert_eq!(eu.blocks.len(), 1);
        assert_eq!(
            eu.blocks[&BlockId(0)].instructions,
            vec![
                load(0, 1, 1),
                load(2, 2, 8),
                imm(1, 0),
                mux(3, 0, 1, 2),
                store(3, 3, 8),
            ]
        );
    }

    #[test]
    fn converts_matching_stores_of_both_arms_to_one_selected_store() {
        let mut eu = stored_diamond();
        assert_eq!(if_convert(&mut eu, MAX_COST), 1);
        assert_eq!(eu.blocks.len(), 1);
        assert_eq!(
            eu.blocks[&BlockId(0)].instructions,
            vec![
                load(0, 1, 1),
                imm(1, 0),
                load(2, 2, 8),
                mux(3, 0, 1, 2),
                store(3, 3, 8),
            ]
        );
        assert_eq!(eu.register_map[&RegisterId(3)], bit(8));
    }

    #[test]
    fn keeps_stores_that_only_one_arm_makes_or_that_an_arm_reads() {
        let mut eu = stored_diamond();
        eu.blocks.get_mut(&BlockId(2)).unwrap().instructions = vec![load(2, 2, 8)];
        assert_eq!(if_convert(&mut eu, MAX_COST), 0);

        let mut eu = stored_diamond();
        // The else arm reads the range the then arm writes.
        eu.blocks.get_mut(&BlockId(2)).unwrap().instructions = vec![load(2, 3, 8), store(3, 2, 8)];
        assert_eq!(if_convert(&mut eu, MAX_COST), 0);

        let mut eu = stored_diamond();
        let SIRInstruction::Store(_, _, _, _, triggers, _) =
            &mut eu.blocks.get_mut(&BlockId(1)).unwrap().instructions[1]
        else {
            unreachable!()
        };
        triggers.push(TriggerIdWithKind {
            kind: celox_design::DomainKind::ClockPosedge,
            id: 0,
        });
        assert_eq!(if_convert(&mut eu, MAX_COST), 0);
    }

    #[test]
    fn keeps_arms_with_other_effects_or_dynamic_loads() {
        let mut eu = reset_diamond();
        eu.blocks
            .get_mut(&BlockId(1))
            .unwrap()
            .instructions
            .push(SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![],
            });
        assert_eq!(if_convert(&mut eu, MAX_COST), 0);

        let mut eu = reset_diamond();
        eu.register_map.insert(RegisterId(4), bit(3));
        let dynamic =
            SIRInstruction::Load(RegisterId(2), addr(2), SIROffset::Dynamic(RegisterId(4)), 8);
        eu.blocks.get_mut(&BlockId(2)).unwrap().instructions = vec![dynamic];
        assert_eq!(if_convert(&mut eu, MAX_COST), 0);
    }

    #[test]
    fn keeps_diamonds_over_the_cost_budget() {
        let mut eu = reset_diamond();
        assert_eq!(if_convert(&mut eu, 2), 0);
        assert_eq!(eu.blocks.len(), 4);
    }

    #[test]
    fn converts_nested_diamonds_inside_out() {
        let mut eu = reset_diamond();
        eu.register_map.insert(RegisterId(5), bit(8));
        eu.blocks.insert(
            BlockId(4),
            block(4, vec![], vec![load(5, 5, 8)], jump(3, vec![5])).1,
        );
        // The else arm is itself a half diamond on the join.
        eu.blocks.get_mut(&BlockId(2)).unwrap().terminator = SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(4), vec![]),
            false_block: (BlockId(3), vec![RegisterId(2)]),
        };
        // b3 has three predecessors: the inner diamond converts without
        // merging it, then the outer one merges it.
        assert_eq!(if_convert(&mut eu, MAX_COST), 2);
        assert_eq!(eu.blocks.len(), 1);
        let entry = &eu.blocks[&BlockId(0)];
        assert_eq!(
            entry
                .instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Mux(..)))
                .count(),
            2
        );
    }

    #[test]
    fn converts_chains_on_one_condition_to_straight_line_code() {
        let mut eu = reset_diamond();
        for register in 4..8 {
            eu.register_map.insert(RegisterId(register), bit(8));
        }
        // A second diamond on the same condition follows the first join.
        eu.blocks.get_mut(&BlockId(3)).unwrap().terminator = branch(0, 4, 5);
        eu.blocks.extend([
            block(4, vec![], vec![imm(4, 0)], jump(6, vec![4])),
            block(5, vec![], vec![load(5, 6, 8)], jump(6, vec![5])),
            block(6, vec![6], vec![store(7, 6, 8)], SIRTerminator::Return),
        ]);
        assert_eq!(if_convert(&mut eu, MAX_COST), 2);
        assert_eq!(eu.blocks.len(), 1);
        let entry = &eu.blocks[&BlockId(0)];
        let count = |pattern: fn(&SIRInstruction<RegionedAbsoluteAddr>) -> bool| {
            entry
                .instructions
                .iter()
                .filter(|inst| pattern(inst))
                .count()
        };
        assert_eq!(count(|inst| matches!(inst, SIRInstruction::Mux(..))), 2);
        assert_eq!(count(|inst| matches!(inst, SIRInstruction::Store(..))), 2);
    }
}
