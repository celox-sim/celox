//! Target-specific x86 SLP selection and XMM register assignment.
//!
//! Scalar MIR is the semantic representation. This pass replaces only packs
//! whose complete scalar use graph is covered by a cheaper executable x86
//! recipe, so unsupported packs remain ordinary scalar MIR.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::memory_effect::{self, UnknownMemory};
use super::mir::{
    BaseReg, MFunction, MInst, OpSize, VReg, X86SimdBinaryOp, X86SimdInst, X86VecReg,
};
use super::regalloc::assignment::{X86PhysVec, X86VectorLocation};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SlpStats {
    pub vector_zeroes: usize,
    pub vector_packs: usize,
    pub vector_loads: usize,
    pub vector_stores: usize,
    pub vector_binary_ops: usize,
    pub scalar_instructions_removed: usize,
}

#[derive(Debug)]
struct LoadPairPlan {
    block: usize,
    first_load: usize,
    second_load: usize,
    load_base: BaseReg,
    load_offset: i32,
    store_pairs: Vec<(usize, usize, BaseReg, i32)>,
}

#[derive(Debug)]
struct PackPairPlan {
    block: usize,
    low: VReg,
    high: VReg,
    zero: bool,
    store_pairs: Vec<(usize, usize, BaseReg, i32)>,
}

#[derive(Debug, Clone, Copy)]
struct ScalarLoadPair {
    low_instruction: usize,
    high_instruction: usize,
    base: BaseReg,
    offset: i32,
}

#[derive(Debug)]
struct BinaryPairPlan {
    block: usize,
    op: X86SimdBinaryOp,
    lhs: ScalarLoadPair,
    rhs: ScalarLoadPair,
    low_binary: usize,
    high_binary: usize,
    store_pairs: Vec<(usize, usize, BaseReg, i32)>,
}

#[derive(Debug, Clone, Copy)]
struct ScalarLoad {
    instruction: usize,
    base: BaseReg,
    offset: i32,
}

/// Select exact `2 x i64` memory-copy packs.
///
/// The initial recipe intentionally does not pack arbitrary GPR values:
/// `movq + movq + punpcklqdq + store` is not cheaper than two scalar stores.
/// A pair is selected only when both scalar loads and every scalar use can be
/// replaced, yielding `1 + N` vector instructions instead of `2 + 2N`.
pub(crate) fn select(func: &mut MFunction) -> SlpStats {
    let mut stats = SlpStats::default();
    let mut function_uses = HashMap::<VReg, Vec<(usize, usize)>>::new();
    let mut phi_uses = HashSet::<VReg>::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            phi_uses.extend(phi.sources.iter().map(|(_, source)| *source));
        }
        for (instruction, inst) in block.insts.iter().enumerate() {
            for used in inst.uses() {
                function_uses
                    .entry(used)
                    .or_default()
                    .push((block_index, instruction));
            }
        }
    }

    let mut plans = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        let mut loads = HashMap::<VReg, ScalarLoad>::new();
        for (instruction, inst) in block.insts.iter().enumerate() {
            if let MInst::Load {
                dst,
                base,
                offset,
                size: OpSize::S64,
            } = inst
            {
                loads.insert(
                    *dst,
                    ScalarLoad {
                        instruction,
                        base: *base,
                        offset: *offset,
                    },
                );
            }
        }

        // Build non-overlapping adjacent Store pairs and group all destinations
        // fed by the same scalar source pair.
        let mut stores_by_source =
            BTreeMap::<(VReg, VReg), Vec<(usize, usize, BaseReg, i32)>>::new();
        let mut instruction = 0usize;
        while instruction + 1 < block.insts.len() {
            let (
                MInst::Store {
                    base: first_base,
                    offset: first_offset,
                    src: first_src,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: second_base,
                    offset: second_offset,
                    src: second_src,
                    size: OpSize::S64,
                },
            ) = (&block.insts[instruction], &block.insts[instruction + 1])
            else {
                instruction += 1;
                continue;
            };
            if first_base == second_base && first_offset.checked_add(8) == Some(*second_offset) {
                stores_by_source
                    .entry((*first_src, *second_src))
                    .or_default()
                    .push((instruction, instruction + 1, *first_base, *first_offset));
                instruction += 2;
            } else {
                instruction += 1;
            }
        }

        for ((first_src, second_src), store_pairs) in stores_by_source {
            let (Some(first_load), Some(second_load)) =
                (loads.get(&first_src), loads.get(&second_src))
            else {
                continue;
            };
            if first_load.instruction >= second_load.instruction
                || first_load.base != second_load.base
                || first_load.offset.checked_add(8) != Some(second_load.offset)
                || block.insts[first_load.instruction + 1..second_load.instruction]
                    .iter()
                    .any(|inst| writes_direct_range(inst, second_load.base, second_load.offset, 8))
            {
                continue;
            }

            let expected_first = store_pairs
                .iter()
                .map(|(first, _, _, _)| (block_index, *first))
                .collect::<Vec<_>>();
            let expected_second = store_pairs
                .iter()
                .map(|(_, second, _, _)| (block_index, *second))
                .collect::<Vec<_>>();
            if phi_uses.contains(&first_src)
                || phi_uses.contains(&second_src)
                || function_uses.get(&first_src).map(Vec::as_slice)
                    != Some(expected_first.as_slice())
                || function_uses.get(&second_src).map(Vec::as_slice)
                    != Some(expected_second.as_slice())
            {
                continue;
            }

            let last_store = store_pairs
                .iter()
                .map(|(_, second, _, _)| *second)
                .max()
                .expect("a source group has at least one Store pair");
            if block.insts[first_load.instruction..=last_store]
                .iter()
                .any(clobbers_selected_xmm_registers)
            {
                continue;
            }

            plans.push(LoadPairPlan {
                block: block_index,
                first_load: first_load.instruction,
                second_load: second_load.instruction,
                load_base: first_load.base,
                load_offset: first_load.offset,
                store_pairs,
            });
        }
    }

    let mut by_block = BTreeMap::<usize, Vec<(X86VecReg, LoadPairPlan)>>::new();
    for plan in plans {
        let vector = func.alloc_x86_vec();
        by_block.entry(plan.block).or_default().push((vector, plan));
    }

    for (block_index, plans) in by_block {
        let block = &mut func.blocks[block_index];
        let mut replacements = HashMap::<usize, Option<MInst>>::new();
        for (vector, plan) in plans {
            if replacements.contains_key(&plan.first_load)
                || replacements.contains_key(&plan.second_load)
                || plan.store_pairs.iter().any(|(first, second, _, _)| {
                    replacements.contains_key(first) || replacements.contains_key(second)
                })
            {
                continue;
            }
            replacements.insert(
                plan.first_load,
                Some(MInst::X86Simd(X86SimdInst::Load128 {
                    dst: vector,
                    base: plan.load_base,
                    offset: plan.load_offset,
                })),
            );
            replacements.insert(plan.second_load, None);
            for (first, second, base, offset) in plan.store_pairs {
                replacements.insert(
                    first,
                    Some(MInst::X86Simd(X86SimdInst::Store128 {
                        base,
                        offset,
                        src: vector,
                    })),
                );
                replacements.insert(second, None);
                stats.vector_stores += 1;
                stats.scalar_instructions_removed += 1;
            }
            stats.vector_loads += 1;
            stats.scalar_instructions_removed += 1;
        }
        let mut rewritten = Vec::with_capacity(block.insts.len());
        for (instruction, inst) in block.insts.drain(..).enumerate() {
            match replacements.remove(&instruction) {
                Some(Some(replacement)) => rewritten.push(replacement),
                Some(None) => {}
                None => rewritten.push(inst),
            }
        }
        block.insts = rewritten;
    }
    select_binary_store_pairs(func, &mut stats);
    select_store_fanout_packs(func, &mut stats);
    stats.vector_zeroes = 0;
    stats.vector_packs = 0;
    stats.vector_loads = 0;
    stats.vector_binary_ops = 0;
    stats.vector_stores = 0;
    for inst in func.blocks.iter().flat_map(|block| &block.insts) {
        match inst {
            MInst::X86Simd(X86SimdInst::Zero128 { .. }) => stats.vector_zeroes += 1,
            MInst::X86Simd(X86SimdInst::Pack128 { .. }) => stats.vector_packs += 1,
            MInst::X86Simd(X86SimdInst::Load128 { .. }) => stats.vector_loads += 1,
            MInst::X86Simd(X86SimdInst::Binary128 { .. }) => stats.vector_binary_ops += 1,
            MInst::X86Simd(X86SimdInst::Store128 { .. }) => stats.vector_stores += 1,
            _ => {}
        }
    }
    stats
}

fn scalar_binary(inst: &MInst) -> Option<(X86SimdBinaryOp, VReg, VReg, VReg)> {
    match inst {
        MInst::And { dst, lhs, rhs } => Some((X86SimdBinaryOp::And, *dst, *lhs, *rhs)),
        MInst::Or { dst, lhs, rhs } => Some((X86SimdBinaryOp::Or, *dst, *lhs, *rhs)),
        MInst::Xor { dst, lhs, rhs } => Some((X86SimdBinaryOp::Xor, *dst, *lhs, *rhs)),
        _ => None,
    }
}

fn exact_scalar_load_pair(
    block: &super::mir::MBlock,
    definitions: &HashMap<VReg, usize>,
    low: VReg,
    high: VReg,
) -> Option<ScalarLoadPair> {
    let (&low_instruction, &high_instruction) =
        definitions.get(&low).zip(definitions.get(&high))?;
    if low_instruction >= high_instruction {
        return None;
    }
    let (
        MInst::Load {
            base: low_base,
            offset: low_offset,
            size: OpSize::S64,
            ..
        },
        MInst::Load {
            base: high_base,
            offset: high_offset,
            size: OpSize::S64,
            ..
        },
    ) = (
        &block.insts[low_instruction],
        &block.insts[high_instruction],
    )
    else {
        return None;
    };
    if low_base != high_base
        || low_offset.checked_add(8) != Some(*high_offset)
        || block.insts[low_instruction + 1..high_instruction]
            .iter()
            .any(|inst| writes_direct_range(inst, *high_base, *high_offset, 8))
    {
        return None;
    }
    Some(ScalarLoadPair {
        low_instruction,
        high_instruction,
        base: *low_base,
        offset: *low_offset,
    })
}

/// Select an exact two-lane scalar Load/Binary/Store cone.
///
/// Every scalar definition must become dead. This deliberately starts with a
/// small complete-cone subset: it never lengthens a scalar live range and it
/// never leaves a GPR/XMM crossing behind for a later pass to repair.
fn select_binary_store_pairs(func: &mut MFunction, stats: &mut SlpStats) {
    let mut function_uses = HashMap::<VReg, Vec<(usize, usize)>>::new();
    let mut phi_uses = HashSet::<VReg>::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            phi_uses.extend(phi.sources.iter().map(|(_, source)| *source));
        }
        for (instruction, inst) in block.insts.iter().enumerate() {
            for used in inst.uses() {
                function_uses
                    .entry(used)
                    .or_default()
                    .push((block_index, instruction));
            }
        }
    }

    let mut plans = Vec::<BinaryPairPlan>::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        let definitions = block
            .insts
            .iter()
            .enumerate()
            .filter_map(|(instruction, inst)| inst.def().map(|dst| (dst, instruction)))
            .collect::<HashMap<_, _>>();
        let mut stores_by_source =
            BTreeMap::<(VReg, VReg), Vec<(usize, usize, BaseReg, i32)>>::new();
        let mut instruction = 0usize;
        while instruction + 1 < block.insts.len() {
            let (
                MInst::Store {
                    base: low_base,
                    offset: low_offset,
                    src: low,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: high_base,
                    offset: high_offset,
                    src: high,
                    size: OpSize::S64,
                },
            ) = (&block.insts[instruction], &block.insts[instruction + 1])
            else {
                instruction += 1;
                continue;
            };
            if low_base == high_base && low_offset.checked_add(8) == Some(*high_offset) {
                stores_by_source.entry((*low, *high)).or_default().push((
                    instruction,
                    instruction + 1,
                    *low_base,
                    *low_offset,
                ));
                instruction += 2;
            } else {
                instruction += 1;
            }
        }

        for ((low_result, high_result), store_pairs) in stores_by_source {
            let (Some(&low_binary), Some(&high_binary)) =
                (definitions.get(&low_result), definitions.get(&high_result))
            else {
                continue;
            };
            let (
                Some((low_op, low_dst, low_lhs, low_rhs)),
                Some((high_op, high_dst, high_lhs, high_rhs)),
            ) = (
                scalar_binary(&block.insts[low_binary]),
                scalar_binary(&block.insts[high_binary]),
            )
            else {
                continue;
            };
            if low_op != high_op || low_dst != low_result || high_dst != high_result {
                continue;
            }
            let expected_low_result = store_pairs
                .iter()
                .map(|(low, _, _, _)| (block_index, *low))
                .collect::<Vec<_>>();
            let expected_high_result = store_pairs
                .iter()
                .map(|(_, high, _, _)| (block_index, *high))
                .collect::<Vec<_>>();
            if phi_uses.contains(&low_result)
                || phi_uses.contains(&high_result)
                || function_uses.get(&low_result).map(Vec::as_slice)
                    != Some(expected_low_result.as_slice())
                || function_uses.get(&high_result).map(Vec::as_slice)
                    != Some(expected_high_result.as_slice())
            {
                continue;
            }

            let orientations = [
                ((low_lhs, high_lhs), (low_rhs, high_rhs)),
                ((low_lhs, high_rhs), (low_rhs, high_lhs)),
            ];
            let Some((lhs_values, rhs_values, lhs, rhs)) =
                orientations
                    .into_iter()
                    .find_map(|(lhs_values, rhs_values)| {
                        let lhs = exact_scalar_load_pair(
                            block,
                            &definitions,
                            lhs_values.0,
                            lhs_values.1,
                        )?;
                        let rhs = exact_scalar_load_pair(
                            block,
                            &definitions,
                            rhs_values.0,
                            rhs_values.1,
                        )?;
                        Some((lhs_values, rhs_values, lhs, rhs))
                    })
            else {
                continue;
            };
            let expected_low_binary = [(block_index, low_binary)];
            let expected_high_binary = [(block_index, high_binary)];
            if [lhs_values.0, rhs_values.0].into_iter().any(|value| {
                phi_uses.contains(&value)
                    || function_uses.get(&value).map(Vec::as_slice)
                        != Some(expected_low_binary.as_slice())
            }) || [lhs_values.1, rhs_values.1].into_iter().any(|value| {
                phi_uses.contains(&value)
                    || function_uses.get(&value).map(Vec::as_slice)
                        != Some(expected_high_binary.as_slice())
            }) {
                continue;
            }
            let first = lhs.low_instruction.min(rhs.low_instruction);
            let last = store_pairs
                .last()
                .expect("binary result has at least one Store pair")
                .1;
            if block.insts[first..=last]
                .iter()
                .any(clobbers_selected_xmm_registers)
            {
                continue;
            }
            plans.push(BinaryPairPlan {
                block: block_index,
                op: low_op,
                lhs,
                rhs,
                low_binary,
                high_binary,
                store_pairs,
            });
        }
    }

    let mut by_block =
        BTreeMap::<usize, Vec<(X86VecReg, X86VecReg, X86VecReg, BinaryPairPlan)>>::new();
    for plan in plans {
        let lhs = func.alloc_x86_vec();
        let rhs = func.alloc_x86_vec();
        let result = func.alloc_x86_vec();
        by_block
            .entry(plan.block)
            .or_default()
            .push((lhs, rhs, result, plan));
    }
    for (block_index, plans) in by_block {
        let block = &mut func.blocks[block_index];
        let mut replacements = HashMap::<usize, Option<MInst>>::new();
        for (lhs_vector, rhs_vector, result_vector, plan) in plans {
            let touched = [
                plan.lhs.low_instruction,
                plan.lhs.high_instruction,
                plan.rhs.low_instruction,
                plan.rhs.high_instruction,
                plan.low_binary,
                plan.high_binary,
            ];
            if touched
                .into_iter()
                .any(|instruction| replacements.contains_key(&instruction))
                || plan.store_pairs.iter().any(|(low, high, _, _)| {
                    replacements.contains_key(low) || replacements.contains_key(high)
                })
            {
                continue;
            }
            for (pair, vector) in [(plan.lhs, lhs_vector), (plan.rhs, rhs_vector)] {
                replacements.insert(
                    pair.low_instruction,
                    Some(MInst::X86Simd(X86SimdInst::Load128 {
                        dst: vector,
                        base: pair.base,
                        offset: pair.offset,
                    })),
                );
                replacements.insert(pair.high_instruction, None);
                stats.vector_loads += 1;
                stats.scalar_instructions_removed += 1;
            }
            replacements.insert(
                plan.low_binary,
                Some(MInst::X86Simd(X86SimdInst::Binary128 {
                    op: plan.op,
                    dst: result_vector,
                    lhs: lhs_vector,
                    rhs: rhs_vector,
                })),
            );
            replacements.insert(plan.high_binary, None);
            stats.vector_binary_ops += 1;
            stats.scalar_instructions_removed += 1;
            for (low, high, base, offset) in plan.store_pairs {
                replacements.insert(
                    low,
                    Some(MInst::X86Simd(X86SimdInst::Store128 {
                        base,
                        offset,
                        src: result_vector,
                    })),
                );
                replacements.insert(high, None);
                stats.vector_stores += 1;
                stats.scalar_instructions_removed += 1;
            }
        }
        let mut rewritten = Vec::with_capacity(block.insts.len());
        for (instruction, inst) in block.insts.drain(..).enumerate() {
            match replacements.remove(&instruction) {
                Some(Some(replacement)) => rewritten.push(replacement),
                Some(None) => {}
                None => rewritten.push(inst),
            }
        }
        block.insts = rewritten;
    }
}

fn writes_direct_range(inst: &MInst, base: BaseReg, offset: i32, byte_len: usize) -> bool {
    let writes = memory_effect::writes(inst);
    if matches!(writes.unknown_memory(), Some(UnknownMemory::Direct(write_base)) if write_base == base)
    {
        return true;
    }
    let start = i64::from(offset);
    let Some(end) = start.checked_add(i64::try_from(byte_len).expect("small direct range")) else {
        return true;
    };
    writes.ranges().any(|write| {
        if write.base != base {
            return false;
        }
        let Some(write_end) = write.end() else {
            return true;
        };
        start < write_end && write.offset < end
    })
}

/// Pack one scalar pair once when it feeds enough adjacent destination pairs
/// to repay the x86 pack recipe.
///
/// Two scalar stores cost `2N`. A general pair costs three instructions to
/// pack. A splat uses `movq + punpcklqdq self`, but both recipes require at
/// least four destinations: the three-destination tie loses on measured x86
/// front-end/ALU cost even though it reduces store-port pressure.
fn select_store_fanout_packs(func: &mut MFunction, stats: &mut SlpStats) {
    let constants = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match inst {
            MInst::LoadImm { dst, value } => Some((*dst, *value)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut pack_plans = Vec::<PackPairPlan>::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        let mut stores_by_source =
            BTreeMap::<(VReg, VReg), Vec<(usize, usize, BaseReg, i32)>>::new();
        let mut instruction = 0usize;
        while instruction + 1 < block.insts.len() {
            let (
                MInst::Store {
                    base: first_base,
                    offset: first_offset,
                    src: first_src,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: second_base,
                    offset: second_offset,
                    src: second_src,
                    size: OpSize::S64,
                },
            ) = (&block.insts[instruction], &block.insts[instruction + 1])
            else {
                instruction += 1;
                continue;
            };
            if first_base == second_base && first_offset.checked_add(8) == Some(*second_offset) {
                stores_by_source
                    .entry((*first_src, *second_src))
                    .or_default()
                    .push((instruction, instruction + 1, *first_base, *first_offset));
                instruction += 2;
            } else {
                instruction += 1;
            }
        }

        for ((low, high), store_pairs) in stores_by_source {
            if store_pairs.len() < 4 {
                continue;
            }
            let first_store = store_pairs[0].0;
            let last_store = store_pairs.last().expect("non-empty store fanout").1;
            if block.insts[first_store..=last_store]
                .iter()
                .any(clobbers_selected_xmm_registers)
            {
                continue;
            }
            pack_plans.push(PackPairPlan {
                block: block_index,
                low,
                high,
                zero: constants.get(&low) == Some(&0) && constants.get(&high) == Some(&0),
                store_pairs,
            });
        }
    }

    let mut by_block = BTreeMap::<usize, Vec<(X86VecReg, PackPairPlan)>>::new();
    for plan in pack_plans {
        let vector = func.alloc_x86_vec();
        by_block.entry(plan.block).or_default().push((vector, plan));
    }
    let mut replacements_by_block = BTreeMap::<usize, HashMap<usize, Vec<MInst>>>::new();
    for (block_index, plans) in by_block {
        let replacements = replacements_by_block.entry(block_index).or_default();
        for (vector, plan) in plans {
            let store_pair_count = plan.store_pairs.len();
            let pack_cost = if plan.zero {
                1
            } else if plan.low == plan.high {
                2
            } else {
                3
            };
            for (pair_index, (first, second, base, offset)) in
                plan.store_pairs.into_iter().enumerate()
            {
                let replacement = replacements.entry(first).or_default();
                if pair_index == 0 {
                    if plan.zero {
                        replacement.push(MInst::X86Simd(X86SimdInst::Zero128 { dst: vector }));
                        stats.vector_zeroes += 1;
                    } else {
                        replacement.push(MInst::X86Simd(X86SimdInst::Pack128 {
                            dst: vector,
                            low: plan.low,
                            high: plan.high,
                        }));
                        stats.vector_packs += 1;
                    }
                }
                replacement.push(MInst::X86Simd(X86SimdInst::Store128 {
                    base,
                    offset,
                    src: vector,
                }));
                replacements.insert(second, Vec::new());
                stats.vector_stores += 1;
            }
            stats.scalar_instructions_removed += store_pair_count - pack_cost;
        }
    }
    for (block_index, mut replacements) in replacements_by_block {
        let block = &mut func.blocks[block_index];
        let mut rewritten = Vec::with_capacity(block.insts.len());
        for (instruction, inst) in block.insts.drain(..).enumerate() {
            match replacements.remove(&instruction) {
                Some(replacements) => rewritten.extend(replacements),
                None => rewritten.push(inst),
            }
        }
        block.insts = rewritten;
    }
}

/// XMM registers which are currently not owned by the scalar spill cache,
/// callee-save machinery, or native tick counter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct X86VectorAllocation {
    pub assignments: Vec<(X86VecReg, X86VectorLocation)>,
    pub spill_bytes: u32,
    pub spilled_values: usize,
}

/// Color vector SSA live ranges from the XMM registers available to this
/// emitted body. The fused native tick loop owns XMM0..XMM14; a
/// standalone body keeps XMM9..XMM14 available for ABI-boundary GPR saves.
///
/// If an interval must spill, allocation is repeated with XMM5 reserved as an
/// explicit emission scratch. This lets pressure use every register when it
/// fits while retaining a finite executable fallback when it does not.
pub(crate) fn allocate(
    func: &MFunction,
    scalar_spill_bytes: u32,
    tick_loop: bool,
) -> X86VectorAllocation {
    let register_limit = if tick_loop { 15u8 } else { 9u8 };
    let registers = (0..register_limit).map(X86PhysVec).collect::<Vec<_>>();
    let first = allocate_from_registers(func, scalar_spill_bytes, &registers);
    if first.spilled_values == 0
        || !first
            .assignments
            .iter()
            .any(|(_, location)| matches!(location, X86VectorLocation::Register(X86PhysVec(5))))
    {
        return first;
    }

    let registers = registers
        .into_iter()
        .filter(|register| register.0 != 5)
        .collect::<Vec<_>>();
    allocate_from_registers(func, scalar_spill_bytes, &registers)
}

fn allocate_from_registers(
    func: &MFunction,
    scalar_spill_bytes: u32,
    registers: &[X86PhysVec],
) -> X86VectorAllocation {
    let vector_spill_base = scalar_spill_bytes
        .checked_add(15)
        .expect("vector spill-frame alignment overflow")
        & !15;
    let mut result = X86VectorAllocation {
        assignments: Vec::with_capacity(func.x86_vec_count() as usize),
        ..X86VectorAllocation::default()
    };

    for block in &func.blocks {
        let mut intervals = HashMap::<X86VecReg, (usize, usize)>::new();
        for (instruction, inst) in block.insts.iter().enumerate() {
            if let Some(definition) = inst.x86_vec_def() {
                intervals.insert(definition, (instruction, instruction));
            }
            for used in inst.x86_vec_uses().into_iter().flatten() {
                if let Some((_, end)) = intervals.get_mut(&used) {
                    *end = (*end).max(instruction);
                }
            }
        }
        let mut intervals = intervals.into_iter().collect::<Vec<_>>();
        intervals.sort_by_key(|(value, (start, _))| (*start, *value));
        let mut active = Vec::<(usize, X86PhysVec)>::new();
        for (value, (start, end)) in intervals {
            active.retain(|(active_end, _)| *active_end >= start);
            let used = active
                .iter()
                .map(|(_, register)| *register)
                .collect::<HashSet<_>>();
            let register = registers.iter().copied().find(|register| {
                !used.contains(register)
                    && !block.insts[start..=end]
                        .iter()
                        .any(|inst| clobbers_xmm(inst, *register))
            });
            if let Some(register) = register {
                result
                    .assignments
                    .push((value, X86VectorLocation::Register(register)));
                active.push((end, register));
            } else {
                let offset = vector_spill_base
                    .checked_add(result.spill_bytes)
                    .expect("vector spill-frame size overflow");
                result.assignments.push((
                    value,
                    X86VectorLocation::Stack(
                        i32::try_from(offset).expect("vector spill offset exceeds i32"),
                    ),
                ));
                result.spill_bytes = result
                    .spill_bytes
                    .checked_add(16)
                    .expect("vector spill-frame size overflow");
                result.spilled_values += 1;
            }
        }
    }
    result.assignments.sort_by_key(|(value, _)| *value);
    result
}

fn clobbers_selected_xmm_registers(inst: &MInst) -> bool {
    matches!(
        inst,
        MInst::PackedLaneCompare { .. } | MInst::PackedByteAffineCompare { .. }
    )
}

pub(crate) fn clobbers_xmm(inst: &MInst, register: X86PhysVec) -> bool {
    match inst {
        MInst::PackedLaneCompare { .. } => register.0 <= 5,
        MInst::PackedByteAffineCompare { .. } => register.0 <= 4,
        MInst::MemCopy { .. } => register.0 == 0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::mir::{BlockId, MBlock, PhiNode, SpillDesc, VRegAllocator};

    #[test]
    fn shared_adjacent_load_pair_becomes_one_vector_value() {
        let mut scalar = VRegAllocator::new();
        let low = scalar.alloc();
        let high = scalar.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: low,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: high,
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 128,
                src: low,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 136,
                src: high,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 256,
                src: low,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 264,
                src: high,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut func = MFunction::new(scalar, vec![SpillDesc::transient(), SpillDesc::transient()]);
        func.blocks.push(block);

        let stats = select(&mut func);

        assert_eq!(stats.vector_loads, 1);
        assert_eq!(stats.vector_stores, 2);
        assert_eq!(stats.scalar_instructions_removed, 3);
        assert!(matches!(
            func.blocks[0].insts.as_slice(),
            [
                MInst::X86Simd(X86SimdInst::Load128 { .. }),
                MInst::X86Simd(X86SimdInst::Store128 { .. }),
                MInst::X86Simd(X86SimdInst::Store128 { .. }),
                MInst::Return
            ]
        ));
        let assignment = allocate(&func, 0, true);
        assert_eq!(
            assignment.assignments,
            vec![(X86VecReg(0), X86VectorLocation::Register(X86PhysVec(0)))]
        );
        assert_eq!(assignment.spill_bytes, 0);
    }

    #[test]
    fn independent_instruction_between_adjacent_loads_still_vectorizes() {
        let mut scalar = VRegAllocator::new();
        let low = scalar.alloc();
        let unrelated = scalar.alloc();
        let high = scalar.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: low,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: unrelated,
                value: 7,
            },
            MInst::Load {
                dst: high,
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 128,
                src: low,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 136,
                src: high,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut func = MFunction::new(
            scalar,
            vec![
                SpillDesc::transient(),
                SpillDesc::transient(),
                SpillDesc::transient(),
            ],
        );
        func.blocks.push(block);

        let stats = select(&mut func);

        assert_eq!(stats.vector_loads, 1);
        assert_eq!(stats.vector_stores, 1);
        assert!(matches!(
            func.blocks[0].insts.as_slice(),
            [
                MInst::X86Simd(X86SimdInst::Load128 { .. }),
                MInst::LoadImm { .. },
                MInst::X86Simd(X86SimdInst::Store128 { .. }),
                MInst::Return
            ]
        ));
        func.verify();
    }

    #[test]
    fn intervening_write_to_second_lane_blocks_load_hoisting() {
        let mut scalar = VRegAllocator::new();
        let low = scalar.alloc();
        let replacement = scalar.alloc();
        let high = scalar.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: low,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: replacement,
                value: 9,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 40,
                src: replacement,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: high,
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 128,
                src: low,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 136,
                src: high,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut func = MFunction::new(
            scalar,
            vec![
                SpillDesc::transient(),
                SpillDesc::transient(),
                SpillDesc::transient(),
            ],
        );
        func.blocks.push(block);

        let stats = select(&mut func);

        assert_eq!(stats, SlpStats::default());
        assert!(matches!(func.blocks[0].insts[0], MInst::Load { .. }));
        assert!(matches!(func.blocks[0].insts[3], MInst::Load { .. }));
        func.verify();
    }

    #[test]
    fn complete_two_lane_bitwise_cone_stays_in_vector_registers() {
        let mut scalar = VRegAllocator::new();
        let lhs_low = scalar.alloc();
        let lhs_high = scalar.alloc();
        let rhs_low = scalar.alloc();
        let rhs_high = scalar.alloc();
        let result_low = scalar.alloc();
        let result_high = scalar.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: lhs_low,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: lhs_high,
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: rhs_low,
                base: BaseReg::SimState,
                offset: 64,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: rhs_high,
                base: BaseReg::SimState,
                offset: 72,
                size: OpSize::S64,
            },
            MInst::And {
                dst: result_low,
                lhs: lhs_low,
                rhs: rhs_low,
            },
            // Commutative operand orientation need not match the low lane.
            MInst::And {
                dst: result_high,
                lhs: rhs_high,
                rhs: lhs_high,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 128,
                src: result_low,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 136,
                src: result_high,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut func = MFunction::new(scalar, vec![SpillDesc::transient(); 6]);
        func.blocks.push(block);

        let stats = select(&mut func);

        assert_eq!(stats.vector_loads, 2);
        assert_eq!(stats.vector_binary_ops, 1);
        assert_eq!(stats.vector_stores, 1);
        assert_eq!(stats.scalar_instructions_removed, 4);
        assert!(matches!(
            func.blocks[0].insts.as_slice(),
            [
                MInst::X86Simd(X86SimdInst::Load128 { .. }),
                MInst::X86Simd(X86SimdInst::Load128 { .. }),
                MInst::X86Simd(X86SimdInst::Binary128 {
                    op: X86SimdBinaryOp::And,
                    ..
                }),
                MInst::X86Simd(X86SimdInst::Store128 { .. }),
                MInst::Return,
            ]
        ));
        func.verify();
        assert_eq!(allocate(&func, 0, true).spilled_values, 0);
    }

    #[test]
    fn scalar_use_outside_bitwise_cone_prevents_partial_vectorization() {
        let mut scalar = VRegAllocator::new();
        let lhs_low = scalar.alloc();
        let lhs_high = scalar.alloc();
        let rhs_low = scalar.alloc();
        let rhs_high = scalar.alloc();
        let result_low = scalar.alloc();
        let result_high = scalar.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: lhs_low,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: lhs_high,
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: rhs_low,
                base: BaseReg::SimState,
                offset: 64,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: rhs_high,
                base: BaseReg::SimState,
                offset: 72,
                size: OpSize::S64,
            },
            MInst::And {
                dst: result_low,
                lhs: lhs_low,
                rhs: rhs_low,
            },
            MInst::And {
                dst: result_high,
                lhs: lhs_high,
                rhs: rhs_high,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 128,
                src: result_low,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 136,
                src: result_high,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 192,
                src: lhs_low,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut func = MFunction::new(scalar, vec![SpillDesc::transient(); 6]);
        func.blocks.push(block);

        let stats = select(&mut func);

        assert_eq!(stats, SlpStats::default());
        assert!(
            func.blocks[0]
                .insts
                .iter()
                .all(|inst| !matches!(inst, MInst::X86Simd(_)))
        );
        func.verify();
    }

    #[test]
    fn vector_pressure_uses_a_stack_home_instead_of_failing() {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let vectors = (0..16).map(|_| func.alloc_x86_vec()).collect::<Vec<_>>();
        let mut block = MBlock::new(BlockId(0));
        for (index, &vector) in vectors.iter().enumerate() {
            block.push(MInst::X86Simd(X86SimdInst::Load128 {
                dst: vector,
                base: BaseReg::SimState,
                offset: i32::try_from(index * 16).unwrap(),
            }));
        }
        for (index, &vector) in vectors.iter().enumerate() {
            block.push(MInst::X86Simd(X86SimdInst::Store128 {
                base: BaseReg::SimState,
                offset: i32::try_from(256 + index * 16).unwrap(),
                src: vector,
            }));
        }
        block.push(MInst::Return);
        func.blocks.push(block);

        let assignment = allocate(&func, 24, true);

        assert_eq!(assignment.assignments.len(), 16);
        // One register is reserved for executable stack-to-stack vector
        // recipes once pressure exceeds all fifteen architectural registers.
        assert_eq!(assignment.spilled_values, 2);
        assert_eq!(assignment.spill_bytes, 32);
        assert!(
            assignment
                .assignments
                .iter()
                .any(|(_, location)| { matches!(location, X86VectorLocation::Stack(32)) })
        );
    }

    #[test]
    fn fused_vector_pressure_can_use_xmm6_through_xmm14() {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let vectors = (0..15).map(|_| func.alloc_x86_vec()).collect::<Vec<_>>();
        let mut block = MBlock::new(BlockId(0));
        for (index, &vector) in vectors.iter().enumerate() {
            block.push(MInst::X86Simd(X86SimdInst::Load128 {
                dst: vector,
                base: BaseReg::SimState,
                offset: i32::try_from(index * 16).unwrap(),
            }));
        }
        for (index, &vector) in vectors.iter().enumerate() {
            block.push(MInst::X86Simd(X86SimdInst::Store128 {
                base: BaseReg::SimState,
                offset: i32::try_from(512 + index * 16).unwrap(),
                src: vector,
            }));
        }
        block.push(MInst::Return);
        func.blocks.push(block);

        let assignment = allocate(&func, 0, true);

        assert_eq!(assignment.spilled_values, 0);
        assert_eq!(
            assignment
                .assignments
                .iter()
                .filter_map(|(_, location)| match location {
                    X86VectorLocation::Register(register) => Some(register.0),
                    X86VectorLocation::Stack(_) => None,
                })
                .collect::<HashSet<_>>(),
            (0..15).collect()
        );
    }

    #[test]
    fn scalar_pair_with_four_store_destinations_is_packed_once() {
        let mut scalar = VRegAllocator::new();
        let low = scalar.alloc();
        let high = scalar.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: low, value: 1 });
        block.push(MInst::LoadImm {
            dst: high,
            value: 2,
        });
        for offset in [32, 64, 96, 128] {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset,
                src: low,
                size: OpSize::S64,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: offset + 8,
                src: high,
                size: OpSize::S64,
            });
        }
        block.push(MInst::Return);
        let mut func = MFunction::new(scalar, vec![SpillDesc::transient(), SpillDesc::transient()]);
        func.blocks.push(block);

        let stats = select(&mut func);

        assert_eq!(stats.vector_packs, 1);
        assert_eq!(stats.vector_stores, 4);
        assert_eq!(stats.scalar_instructions_removed, 1);
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::X86Simd(X86SimdInst::Pack128 { .. })))
                .count(),
            1
        );
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::X86Simd(X86SimdInst::Store128 { .. })))
                .count(),
            4
        );
        func.verify();
    }

    #[test]
    fn zero_fanout_uses_dependency_breaking_vector_zero() {
        let mut scalar = VRegAllocator::new();
        let zero = scalar.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        for offset in [32, 64, 96, 128] {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset,
                src: zero,
                size: OpSize::S64,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: offset + 8,
                src: zero,
                size: OpSize::S64,
            });
        }
        block.push(MInst::Return);
        let mut func = MFunction::new(scalar, vec![SpillDesc::transient()]);
        func.blocks.push(block);

        let stats = select(&mut func);

        assert_eq!(stats.vector_zeroes, 1);
        assert_eq!(stats.vector_packs, 0);
        assert_eq!(stats.vector_stores, 4);
        assert_eq!(stats.scalar_instructions_removed, 3);
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::X86Simd(X86SimdInst::Zero128 { .. })))
                .count(),
            1
        );
        func.verify();
    }

    #[test]
    fn scalar_splat_with_three_store_destinations_remains_scalar() {
        let mut scalar = VRegAllocator::new();
        let value = scalar.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: value,
            value: 1,
        });
        for offset in [32, 64, 96] {
            for lane_offset in [0, 8] {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: offset + lane_offset,
                    src: value,
                    size: OpSize::S64,
                });
            }
        }
        block.push(MInst::Return);
        let mut func = MFunction::new(scalar, vec![SpillDesc::transient()]);
        func.blocks.push(block);

        let stats = select(&mut func);

        assert_eq!(stats.vector_packs, 0);
        assert_eq!(stats.vector_stores, 0);
        assert_eq!(stats.scalar_instructions_removed, 0);
        func.verify();
    }

    #[test]
    fn load_pair_with_cross_block_use_remains_scalar() {
        let mut scalar = VRegAllocator::new();
        let low = scalar.alloc();
        let high = scalar.alloc();
        let mut first = MBlock::new(BlockId(0));
        first.insts = vec![
            MInst::Load {
                dst: low,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: high,
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 128,
                src: low,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 136,
                src: high,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut second = MBlock::new(BlockId(1));
        second.insts = vec![
            MInst::Store {
                base: BaseReg::SimState,
                offset: 256,
                src: low,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut func = MFunction::new(scalar, vec![SpillDesc::transient(), SpillDesc::transient()]);
        func.blocks = vec![first, second];

        let stats = select(&mut func);

        assert_eq!(stats, SlpStats::default());
        assert!(matches!(func.blocks[0].insts[0], MInst::Load { .. }));
        assert!(matches!(func.blocks[0].insts[1], MInst::Load { .. }));
    }

    #[test]
    fn load_pair_with_phi_use_remains_scalar() {
        let mut scalar = VRegAllocator::new();
        let low = scalar.alloc();
        let high = scalar.alloc();
        let merged = scalar.alloc();
        let mut first = MBlock::new(BlockId(0));
        first.insts = vec![
            MInst::Load {
                dst: low,
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: high,
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 128,
                src: low,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 136,
                src: high,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut second = MBlock::new(BlockId(1));
        second.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(0), low)],
        });
        second.insts.push(MInst::Return);
        let mut func = MFunction::new(
            scalar,
            vec![
                SpillDesc::transient(),
                SpillDesc::transient(),
                SpillDesc::transient(),
            ],
        );
        func.blocks = vec![first, second];

        let stats = select(&mut func);

        assert_eq!(stats, SlpStats::default());
        assert!(matches!(func.blocks[0].insts[0], MInst::Load { .. }));
        assert!(matches!(func.blocks[0].insts[1], MInst::Load { .. }));
    }
}
