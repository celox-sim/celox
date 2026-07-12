//! Vectorize Concat: replace a Concat of single-bit extracts from the same
//! source with a single bitwise `And(src, mask)` when each extracted bit
//! occupies the same position in the Concat output as in the source.
//!
//! Handles two patterns:
//!
//! 1. Register-based: `(reg >> K) & 1` or `Slice(reg, K, 1)`
//!    → `And(reg, mask_constant)`
//!
//! 2. Load-based: `Load(addr, K, 1)` from same address
//!    → `Load(addr, 0, width)` then `And(wide_load, mask_constant)`
//!
//! This eliminates O(N) shift+and+or chains for building masked bitvectors
//! such as Hamming parity masks.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{def_reg, sir_value_to_u64};
use crate::ir::*;
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};
use num_bigint::BigUint;

pub(super) struct VectorizeConcatPass;

impl ExecutionUnitPass for VectorizeConcatPass {
    fn name(&self) -> &'static str {
        "vectorize_concat"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        // This pass rewrites Concat into bitwise/arithmetic operations. In
        // 4-state mode those operations normalize Z to X, while Concat must
        // preserve the original value+mask bits exactly.
        if options.four_state {
            return;
        }

        let mut max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
        let mut any_changed = false;

        // A load-based pack is materialized at the Concat, after the scalar
        // loads it replaces.  That is only the same memory version when this
        // execution unit never writes the source address.  Register-based
        // packs do not need this restriction because SSA already fixes their
        // value.
        let written_addresses = eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|inst| match inst {
                SIRInstruction::Store(address, ..) => Some(*address),
                SIRInstruction::Commit(_, destination, ..) => Some(*destination),
                _ => None,
            })
            .collect::<HashSet<_>>();

        // Each tuple analyzed by recursive lane packing must consume one
        // distinct definition from the input SIR. Credits are shared across
        // blocks, roots and fixed-point iterations and are never refunded,
        // including after a rejected candidate. Together with the input
        // Concat roots themselves, this bounds total tuple work by input size.
        let lane_definition_credits = eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(def_reg)
            .collect::<HashSet<_>>();
        let mut claimed_lane_definitions = HashSet::default();

        // A recursive lane DAG is emitted bottom-up in one iteration, with one
        // Concat per distinct leaf key. The following iteration lowers those
        // leaves using the ordinary bit-extract rules. Thus iteration count is
        // independent of scalar DAG depth; the loop only establishes the real
        // fixed point for newly exposed leaf Concats and exact-Concat CSE.
        loop {
            let mut iteration_changed = false;
            let register_use_counts = collect_register_use_counts(eu);

            // Build global def map across all blocks. Rebuilding it per
            // iteration avoids looking through stale definitions after a
            // Concat has been rewritten.
            let mut global_defs: HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>> =
                HashMap::default();
            for block in eu.blocks.values() {
                for inst in &block.instructions {
                    if let Some(d) = def_reg(inst) {
                        global_defs.insert(d, inst.clone());
                    }
                }
            }

            for block in eu.blocks.values_mut() {
                if vectorize_concats(
                    &mut block.instructions,
                    &mut eu.register_map,
                    &mut max_reg,
                    &global_defs,
                    &written_addresses,
                    &register_use_counts,
                    &lane_definition_credits,
                    &mut claimed_lane_definitions,
                ) {
                    any_changed = true;
                    iteration_changed = true;
                }
            }

            if !iteration_changed {
                break;
            }
        }

        if !any_changed {
            return;
        }

        // Packing disconnects the original scalar DAG from its Concat root.
        // Mark from observable instructions and terminators, then sweep once;
        // repeatedly deleting only the outer dead layer is quadratic in DAG
        // depth.
        remove_dead_definitions(eu);
    }
}

fn push_instruction_uses(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    worklist: &mut Vec<RegisterId>,
) {
    match inst {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            worklist.push(*lhs);
            worklist.push(*rhs);
        }
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, ..) => {
            worklist.push(*source);
        }
        SIRInstruction::Load(_, _, SIROffset::Dynamic(offset), _) => worklist.push(*offset),
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => {}
        SIRInstruction::Store(_, offset, _, source, _, _) => {
            worklist.push(*source);
            if let SIROffset::Dynamic(offset) = offset {
                worklist.push(*offset);
            }
        }
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(offset), _, _) => worklist.push(*offset),
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => {}
        SIRInstruction::Concat(_, args)
        | SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => {
            worklist.extend(args.iter().copied());
        }
        SIRInstruction::Mux(_, condition, then_value, else_value) => {
            worklist.push(*condition);
            worklist.push(*then_value);
            worklist.push(*else_value);
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            worklist.push(*old);
            worklist.push(*new);
        }
    }
}

fn push_terminator_uses(terminator: &SIRTerminator, worklist: &mut Vec<RegisterId>) {
    match terminator {
        SIRTerminator::Jump(_, args) => worklist.extend(args.iter().copied()),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            worklist.push(*cond);
            worklist.extend(true_block.1.iter().copied());
            worklist.extend(false_block.1.iter().copied());
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn collect_register_use_counts(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, usize> {
    let mut counts = HashMap::default();
    let mut uses = Vec::new();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            uses.clear();
            push_instruction_uses(instruction, &mut uses);
            for register in uses.iter().copied() {
                let count = counts.entry(register).or_insert(0usize);
                *count = count.saturating_add(1);
            }
        }
        uses.clear();
        push_terminator_uses(&block.terminator, &mut uses);
        for register in uses.iter().copied() {
            let count = counts.entry(register).or_insert(0usize);
            *count = count.saturating_add(1);
        }
    }
    counts
}

/// Remove dead pure definitions in one O(instructions + operand edges)
/// mark/sweep. Loads are pure SIR values; stores, commits and runtime/capture
/// events are observable roots.
fn remove_dead_definitions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let mut definitions = HashMap::<RegisterId, (BlockId, usize)>::default();
    let mut worklist = Vec::new();

    for (&block_id, block) in &eu.blocks {
        for (instruction_index, inst) in block.instructions.iter().enumerate() {
            if let Some(definition) = def_reg(inst) {
                definitions.insert(definition, (block_id, instruction_index));
            } else {
                push_instruction_uses(inst, &mut worklist);
            }
        }
        // Edge arguments are conservatively terminator roots. This is the
        // existing SIR convention and also keeps live block parameters sound.
        push_terminator_uses(&block.terminator, &mut worklist);
    }

    let mut live = HashSet::default();
    while let Some(register) = worklist.pop() {
        if !live.insert(register) {
            continue;
        }
        if let Some(&(block, instruction)) = definitions.get(&register) {
            push_instruction_uses(&eu.blocks[&block].instructions[instruction], &mut worklist);
        }
    }

    for block in eu.blocks.values_mut() {
        block
            .instructions
            .retain(|inst| def_reg(inst).is_none_or(|definition| live.contains(&definition)));
    }
}

/// A single-bit element in a Concat: either from a register or from a Load.
enum BitSource {
    /// `(reg >> bit_position) & 1` or `Slice(reg, bit_position, 1)`
    Register {
        source: RegisterId,
        bit_position: usize,
    },
    /// `Load(addr, bit_position, 1)`
    Load {
        addr: RegionedAbsoluteAddr,
        bit_position: usize,
    },
}

/// Try to resolve a register to a single-bit extraction.
fn resolve_bit_source(
    reg: RegisterId,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<BitSource> {
    let mut current = reg;
    let mut identity_steps = 0usize;
    loop {
        // A valid SSA identity chain cannot contain more definitions than the
        // whole unit. This bound detects malformed cycles without allocating a
        // visited set for every ordinary one-step bit source.
        if identity_steps > defs.len() {
            return None;
        }
        let def = defs.get(&current)?;
        match def {
            // Load(dst, addr, Static(offset), 1)
            SIRInstruction::Load(_, addr, SIROffset::Static(offset), 1) => {
                return Some(BitSource::Load {
                    addr: *addr,
                    bit_position: *offset,
                });
            }

            // Slice(dst, src, offset, 1)
            SIRInstruction::Slice(_, src, offset, 1) => {
                return Some(BitSource::Register {
                    source: *src,
                    bit_position: *offset,
                });
            }

            // Binary(dst, shifted, And, mask_reg) where mask=1
            SIRInstruction::Binary(_, shifted, BinaryOp::And, mask_reg) => {
                let mask_def = defs.get(mask_reg)?;
                let SIRInstruction::Imm(_, mask_val) = mask_def else {
                    return None;
                };
                if sir_value_to_u64(mask_val)? != 1 {
                    return None;
                }
                let shifted_def = defs.get(shifted)?;
                return match shifted_def {
                    SIRInstruction::Binary(_, src, BinaryOp::Shr, shift_reg) => {
                        let shift_def = defs.get(shift_reg)?;
                        let SIRInstruction::Imm(_, shift_val) = shift_def else {
                            return None;
                        };
                        let shift = sir_value_to_u64(shift_val)? as usize;
                        Some(BitSource::Register {
                            source: *src,
                            bit_position: shift,
                        })
                    }
                    _ => Some(BitSource::Register {
                        source: *shifted,
                        bit_position: 0,
                    }),
                };
            }

            // Look through identity without consuming the native call stack.
            SIRInstruction::Unary(_, UnaryOp::Ident, source) => {
                current = *source;
                identity_steps += 1;
            }
            _ => return None,
        }
    }
}

/// Check if a register is a constant zero.
fn is_zero(
    reg: RegisterId,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> bool {
    let Some(def) = defs.get(&reg) else {
        return false;
    };
    matches!(def, SIRInstruction::Imm(_, val) if sir_value_to_u64(val) == Some(0))
}

/// A replacement to apply.
enum Replacement {
    /// Replace Concat with `And(source_reg, mask)`
    RegisterAnd {
        inst_idx: usize,
        dst: RegisterId,
        source: RegisterId,
        mask: u64,
        width: usize,
    },
    /// Replace Concat with `Load(addr, 0, width)` then `And(load, mask)`
    LoadAnd {
        inst_idx: usize,
        dst: RegisterId,
        addr: RegionedAbsoluteAddr,
        mask: u64,
        width: usize,
    },
    /// Replace Concat with grouped shift+mask+or operations.
    /// Used when bits are not in-place but form contiguous groups with constant delta.
    GroupedShift {
        inst_idx: usize,
        dst: RegisterId,
        source: RegisterId,
        /// (src_start, dest_start, group_len)
        groups: Vec<(usize, usize, usize)>,
        width: usize,
    },
    /// Replace `{low[MSB]..., low}` with `(low << n) >>> n`.
    SignExtend {
        inst_idx: usize,
        dst: RegisterId,
        low: RegisterId,
        width: usize,
        prefix_width: usize,
    },
    /// Replace a recursively isomorphic lane DAG with a bottom-up vector DAG.
    /// The sequence is already in SSA dominance order.
    LaneDag {
        inst_idx: usize,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    },
}

fn concat_width(
    args: &[RegisterId],
    register_map: &HashMap<RegisterId, RegisterType>,
) -> Option<usize> {
    args.iter().try_fold(0usize, |acc, arg| {
        Some(acc + register_map.get(arg)?.width())
    })
}

fn sign_bit_matches_low_msb(
    sign: RegisterId,
    low: RegisterId,
    low_width: usize,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> bool {
    if low_width == 0 {
        return false;
    }
    if low_width == 1 && sign == low {
        return true;
    }

    let sign_bit = low_width - 1;
    match resolve_bit_source(sign, defs) {
        Some(BitSource::Register {
            source,
            bit_position,
        }) if source == low && bit_position == sign_bit => return true,
        Some(BitSource::Register {
            source,
            bit_position,
        }) => {
            if let Some(SIRInstruction::Slice(_, low_source, base, width)) = defs.get(&low)
                && source == *low_source
                && low_width == *width
                && bit_position == base + sign_bit
            {
                return true;
            }
        }
        Some(BitSource::Load { addr, bit_position }) => {
            if let Some(SIRInstruction::Load(_, low_addr, SIROffset::Static(base), width)) =
                defs.get(&low)
                && addr == *low_addr
                && low_width == *width
                && bit_position == base + sign_bit
            {
                return true;
            }
        }
        _ => {}
    }

    false
}

fn find_sign_extend(
    args: &[RegisterId],
    concat_width: usize,
    register_map: &HashMap<RegisterId, RegisterType>,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<(RegisterId, usize, usize)> {
    if args.len() < 2 || concat_width > 64 {
        return None;
    }

    let low = *args.last()?;
    let low_width = register_map.get(&low)?.width();
    if low_width == 0 || low_width >= concat_width {
        return None;
    }

    let prefix_width = concat_width - low_width;
    let prefix = &args[..args.len() - 1];
    if prefix.len() != prefix_width {
        return None;
    }

    let sign = prefix[0];
    if !prefix
        .iter()
        .all(|arg| *arg == sign && register_map.get(arg).is_some_and(|rt| rt.width() == 1))
    {
        return None;
    }

    sign_bit_matches_low_msb(sign, low, low_width, defs).then_some((low, low_width, prefix_width))
}

fn normalized_lane_binary_op(op: BinaryOp) -> Option<BinaryOp> {
    match op {
        BinaryOp::And => Some(BinaryOp::And),
        BinaryOp::Or => Some(BinaryOp::Or),
        BinaryOp::Xor => Some(BinaryOp::Xor),
        BinaryOp::LogicAnd => Some(BinaryOp::And),
        BinaryOp::LogicOr => Some(BinaryOp::Or),
        _ => None,
    }
}

fn normalized_lane_unary_op(op: UnaryOp) -> Option<UnaryOp> {
    match op {
        // On a one-bit two-state lane, logical and bitwise negation are the
        // same operation.  The pass is disabled in four-state mode above.
        UnaryOp::LogicNot | UnaryOp::BitNot => Some(UnaryOp::BitNot),
        UnaryOp::Ident => Some(UnaryOp::Ident),
        _ => None,
    }
}

fn lane_binary_shape(
    args: &[RegisterId],
    register_map: &HashMap<RegisterId, RegisterType>,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<(Vec<RegisterId>, Vec<RegisterId>, BinaryOp)> {
    let mut lane_op = None;
    let mut lhs_args = Vec::with_capacity(args.len());
    let mut rhs_args = Vec::with_capacity(args.len());

    for &arg in args {
        let Some(SIRInstruction::Binary(_, lhs, op, rhs)) = defs.get(&arg) else {
            return None;
        };
        let op = normalized_lane_binary_op(*op)?;
        if lane_op.is_some_and(|lane_op| lane_op != op) {
            return None;
        }
        if !register_map.get(lhs).is_some_and(|rt| rt.width() == 1)
            || !register_map.get(rhs).is_some_and(|rt| rt.width() == 1)
        {
            return None;
        }

        lane_op = Some(op);
        lhs_args.push(*lhs);
        rhs_args.push(*rhs);
    }

    Some((lhs_args, rhs_args, lane_op?))
}

fn lane_unary_shape(
    args: &[RegisterId],
    register_map: &HashMap<RegisterId, RegisterType>,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<(Vec<RegisterId>, UnaryOp)> {
    let mut lane_op = None;
    let mut inner_args = Vec::with_capacity(args.len());

    for &arg in args {
        let Some(SIRInstruction::Unary(_, op, inner)) = defs.get(&arg) else {
            return None;
        };
        let op = normalized_lane_unary_op(*op)?;
        if lane_op.is_some_and(|lane_op| lane_op != op) {
            return None;
        }
        if !register_map
            .get(inner)
            .is_some_and(|register| register.width() == 1)
        {
            return None;
        }
        lane_op = Some(op);
        inner_args.push(*inner);
    }

    Some((inner_args, lane_op?))
}

#[derive(Clone)]
enum LanePackKind {
    Leaf,
    Unary {
        inner: Vec<RegisterId>,
        op: UnaryOp,
    },
    Binary {
        lhs: Vec<RegisterId>,
        rhs: Vec<RegisterId>,
        op: BinaryOp,
    },
    NotPackable,
}

impl LanePackKind {
    fn is_packable(&self) -> bool {
        !matches!(self, Self::NotPackable)
    }
}

enum LaneAnalysisWork {
    Enter(Vec<RegisterId>),
    Exit(Vec<RegisterId>, LanePackKind),
}

/// Analyze a lane-vector DAG with an explicit postorder stack. Every distinct
/// vector of lane registers is memoized once, so both depth and reconvergence
/// are bounded by the input DAG rather than the native call stack.
fn analyze_lane_pack(
    root: &[RegisterId],
    register_map: &HashMap<RegisterId, RegisterType>,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
    written_addresses: &HashSet<RegionedAbsoluteAddr>,
    definition_credits: &HashSet<RegisterId>,
    claimed_definitions: &mut HashSet<RegisterId>,
    memo: &mut HashMap<Vec<RegisterId>, LanePackKind>,
) -> bool {
    if root.len() < 3 {
        return false;
    }
    if let Some(known) = memo.get(root) {
        return known.is_packable();
    }

    let root = root.to_vec();
    let mut visiting = HashSet::default();
    let mut stack = vec![LaneAnalysisWork::Enter(root.clone())];
    let mut credit_exhausted = false;

    while let Some(work) = stack.pop() {
        match work {
            LaneAnalysisWork::Enter(key) => {
                if memo.contains_key(&key) {
                    continue;
                }
                if visiting.contains(&key) {
                    // Canonical SIR is acyclic. Keep malformed SSA finite and
                    // let the verifier report the underlying cycle.
                    memo.insert(key, LanePackKind::NotPackable);
                    continue;
                }
                let Some(credit) = key.iter().copied().find(|register| {
                    definition_credits.contains(register) && !claimed_definitions.contains(register)
                }) else {
                    // Failed candidates keep all earlier claims. Therefore a
                    // hostile set of roots cannot repeatedly re-explore the
                    // same synchronous product.
                    credit_exhausted = true;
                    break;
                };
                claimed_definitions.insert(credit);
                visiting.insert(key.clone());

                if is_vectorizable_bit_extract_concat(
                    &key,
                    key.len(),
                    register_map,
                    defs,
                    written_addresses,
                ) {
                    visiting.remove(&key);
                    memo.insert(key, LanePackKind::Leaf);
                } else if let Some((inner, op)) = lane_unary_shape(&key, register_map, defs) {
                    stack.push(LaneAnalysisWork::Exit(
                        key,
                        LanePackKind::Unary {
                            inner: inner.clone(),
                            op,
                        },
                    ));
                    stack.push(LaneAnalysisWork::Enter(inner));
                } else if let Some((lhs, rhs, op)) = lane_binary_shape(&key, register_map, defs) {
                    stack.push(LaneAnalysisWork::Exit(
                        key,
                        LanePackKind::Binary {
                            lhs: lhs.clone(),
                            rhs: rhs.clone(),
                            op,
                        },
                    ));
                    // LHS is processed first; a shared RHS then hits the memo.
                    stack.push(LaneAnalysisWork::Enter(rhs));
                    stack.push(LaneAnalysisWork::Enter(lhs));
                } else {
                    visiting.remove(&key);
                    memo.insert(key, LanePackKind::NotPackable);
                }
            }
            LaneAnalysisWork::Exit(key, candidate) => {
                let children_packable = match &candidate {
                    LanePackKind::Unary { inner, .. } => {
                        memo.get(inner).is_some_and(LanePackKind::is_packable)
                    }
                    LanePackKind::Binary { lhs, rhs, .. } => {
                        memo.get(lhs).is_some_and(LanePackKind::is_packable)
                            && memo.get(rhs).is_some_and(LanePackKind::is_packable)
                    }
                    LanePackKind::Leaf => true,
                    LanePackKind::NotPackable => false,
                };
                visiting.remove(&key);
                memo.insert(
                    key,
                    if children_packable {
                        candidate
                    } else {
                        LanePackKind::NotPackable
                    },
                );
            }
        }
    }

    if credit_exhausted {
        memo.insert(root, LanePackKind::NotPackable);
        return false;
    }
    memo.get(&root).is_some_and(LanePackKind::is_packable)
}

fn alloc_unsigned_reg(
    next_reg: &mut usize,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    width: usize,
) -> RegisterId {
    *next_reg += 1;
    let register = RegisterId(*next_reg);
    register_map.insert(
        register,
        RegisterType::Bit {
            width,
            signed: false,
        },
    );
    register
}

enum LanePlanWork {
    Enter(Vec<RegisterId>),
    Exit(Vec<RegisterId>),
}

/// Count candidate scalar definitions that are guaranteed to become unused
/// when the root Concat is removed. Removed uses propagate only after all uses
/// of a definition disappear. Leaf operands are a boundary because the final
/// packed leaf operation replaces (rather than removes) their source use.
fn guaranteed_dead_definition_count(
    root: &[RegisterId],
    postorder: &[Vec<RegisterId>],
    analysis: &HashMap<Vec<RegisterId>, LanePackKind>,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
    use_counts: &HashMap<RegisterId, usize>,
) -> usize {
    let candidate_registers = postorder
        .iter()
        .flat_map(|key| key.iter().copied())
        .collect::<HashSet<_>>();
    let leaf_registers = postorder
        .iter()
        .filter(|key| matches!(analysis.get(*key), Some(LanePackKind::Leaf)))
        .flat_map(|key| key.iter().copied())
        .collect::<HashSet<_>>();
    let leaf_key_count = postorder
        .iter()
        .filter(|key| matches!(analysis.get(*key), Some(LanePackKind::Leaf)))
        .count();

    let mut removed_uses = HashMap::<RegisterId, usize>::default();
    let mut dead = HashSet::default();
    let mut dead_leaves = HashSet::default();
    let mut worklist = root.to_vec();
    let mut operands = Vec::new();
    while let Some(register) = worklist.pop() {
        if !candidate_registers.contains(&register) {
            continue;
        }
        let removed = removed_uses.entry(register).or_insert(0);
        *removed = removed.saturating_add(1);
        let total = use_counts.get(&register).copied().unwrap_or(0);
        if *removed != total || total == 0 {
            continue;
        }
        if leaf_registers.contains(&register) {
            if defs.contains_key(&register) {
                dead_leaves.insert(register);
            }
            continue;
        }
        let Some(definition) = defs.get(&register) else {
            continue;
        };
        if !dead.insert(register) {
            continue;
        }
        operands.clear();
        push_instruction_uses(definition, &mut operands);
        worklist.extend(
            operands
                .iter()
                .copied()
                .filter(|operand| candidate_registers.contains(operand)),
        );
    }
    // SignExtend may retain one scalar value from each leaf key. Reserve that
    // many otherwise-dead leaves; direct bit-extract lowering retains none.
    dead.len()
        .saturating_add(dead_leaves.len().saturating_sub(leaf_key_count))
}

/// Materialize every uncached node in one bottom-up sequence. Leaf Concats are
/// left for the ordinary bit-extract rewrite in the next (depth-independent)
/// pass iteration. `packed_vectors` is block-local and only contains values
/// defined at an earlier instruction or earlier in this sequence, so reuse
/// preserves SSA dominance. Each unique key emits at most one instruction.
fn materialize_lane_pack(
    root: &[RegisterId],
    destination: RegisterId,
    width: usize,
    analysis: &HashMap<Vec<RegisterId>, LanePackKind>,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
    use_counts: &HashMap<RegisterId, usize>,
    packed_vectors: &mut HashMap<Vec<RegisterId>, RegisterId>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    next_reg: &mut usize,
) -> Option<Vec<SIRInstruction<RegionedAbsoluteAddr>>> {
    if let Some(&packed) = packed_vectors.get(root) {
        return Some(vec![SIRInstruction::Unary(
            destination,
            UnaryOp::Ident,
            packed,
        )]);
    }

    let root = root.to_vec();
    let mut seen = HashSet::default();
    let mut postorder = Vec::new();
    let mut stack = vec![LanePlanWork::Enter(root.clone())];
    while let Some(work) = stack.pop() {
        match work {
            LanePlanWork::Enter(key) => {
                if packed_vectors.contains_key(&key) || !seen.insert(key.clone()) {
                    continue;
                }
                stack.push(LanePlanWork::Exit(key.clone()));
                match analysis.get(&key)? {
                    LanePackKind::Leaf => {}
                    LanePackKind::Unary { inner, .. } => {
                        stack.push(LanePlanWork::Enter(inner.clone()));
                    }
                    LanePackKind::Binary { lhs, rhs, .. } => {
                        stack.push(LanePlanWork::Enter(rhs.clone()));
                        stack.push(LanePlanWork::Enter(lhs.clone()));
                    }
                    LanePackKind::NotPackable => return None,
                }
            }
            LanePlanWork::Exit(key) => postorder.push(key),
        }
    }

    // Validate the complete postorder before allocating registers or updating
    // the dominance cache. A failed/incomplete analysis therefore leaves all
    // caller-owned state unchanged.
    let mut locally_available = HashSet::default();
    for key in &postorder {
        let valid = match analysis.get(key) {
            Some(LanePackKind::Leaf) => true,
            Some(LanePackKind::Unary { inner, .. }) => {
                packed_vectors.contains_key(inner) || locally_available.contains(inner)
            }
            Some(LanePackKind::Binary { lhs, rhs, .. }) => {
                (packed_vectors.contains_key(lhs) || locally_available.contains(lhs))
                    && (packed_vectors.contains_key(rhs) || locally_available.contains(rhs))
            }
            Some(LanePackKind::NotPackable) | None => false,
        };
        if !valid {
            return None;
        }
        locally_available.insert(key.clone());
    }

    // The root Concat itself is removed. Account for the final lowering cost
    // of every leaf (not merely its temporary Concat), then require every
    // additional emitted instruction to be paid for by a scalar definition
    // proven dead above. Recursive packing therefore cannot increase the
    // final instruction count.
    let mut final_instruction_count = 0usize;
    for key in &postorder {
        let cost = match analysis.get(key) {
            Some(LanePackKind::Leaf) => {
                // SignExtend has precedence over bit-extract lowering and
                // emits three instructions, so include that conservative
                // alternative even when the bit-extract form is cheaper.
                bit_extract_pack_instruction_count(key, width, defs)?.max(3)
            }
            Some(LanePackKind::Unary { .. } | LanePackKind::Binary { .. }) => 1,
            Some(LanePackKind::NotPackable) | None => return None,
        };
        final_instruction_count = final_instruction_count.saturating_add(cost);
    }
    let dead_definitions =
        guaranteed_dead_definition_count(&root, &postorder, analysis, defs, use_counts);
    if final_instruction_count > dead_definitions.saturating_add(1) {
        return None;
    }

    let mut instructions = Vec::with_capacity(postorder.len());
    for key in postorder {
        if packed_vectors.contains_key(&key) {
            continue;
        }
        let output = if key == root {
            destination
        } else {
            alloc_unsigned_reg(next_reg, register_map, width)
        };
        let instruction = match analysis
            .get(&key)
            .expect("validated lane-pack key must have analysis")
        {
            LanePackKind::Leaf => SIRInstruction::Concat(output, key.clone()),
            LanePackKind::Unary { inner, op } => {
                SIRInstruction::Unary(output, *op, packed_vectors[inner])
            }
            LanePackKind::Binary { lhs, rhs, op } => {
                SIRInstruction::Binary(output, packed_vectors[lhs], *op, packed_vectors[rhs])
            }
            LanePackKind::NotPackable => unreachable!("validated lane-pack key is packable"),
        };
        instructions.push(instruction);
        packed_vectors.insert(key, output);
    }
    Some(instructions)
}

fn is_vectorizable_bit_extract_concat(
    args: &[RegisterId],
    concat_width: usize,
    register_map: &HashMap<RegisterId, RegisterType>,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
    written_addresses: &HashSet<RegionedAbsoluteAddr>,
) -> bool {
    if !args
        .iter()
        .all(|arg| register_map.get(arg).is_some_and(|rt| rt.width() == 1))
    {
        return false;
    }

    let mut reg_source: Option<RegisterId> = None;
    let mut load_addr: Option<RegionedAbsoluteAddr> = None;
    let mut in_place = true;
    let mut extract_count = 0usize;
    let mut is_load_based = false;

    for (i, &arg) in args.iter().enumerate() {
        let concat_position = concat_width - 1 - i;

        if is_zero(arg, defs) {
            continue;
        }

        match resolve_bit_source(arg, defs) {
            Some(BitSource::Register {
                source,
                bit_position,
            }) => {
                if is_load_based {
                    return false;
                }
                match reg_source {
                    Some(s) if s != source => return false,
                    None => reg_source = Some(source),
                    _ => {}
                }
                if bit_position >= 64 {
                    return false;
                }
                if bit_position != concat_position {
                    in_place = false;
                }
                extract_count += 1;
            }
            Some(BitSource::Load { addr, bit_position }) => {
                if written_addresses.contains(&addr) {
                    return false;
                }
                if reg_source.is_some() {
                    return false;
                }
                is_load_based = true;
                match load_addr {
                    Some(a) if a != addr => return false,
                    None => load_addr = Some(addr),
                    _ => {}
                }
                if bit_position >= 64 {
                    return false;
                }
                if bit_position != concat_position {
                    in_place = false;
                }
                extract_count += 1;
            }
            None => return false,
        }
    }

    if extract_count < 3 {
        return false;
    }

    if in_place {
        reg_source.is_some() || load_addr.is_some()
    } else {
        reg_source.is_some() && find_shift_groups(args, concat_width, defs).is_some()
    }
}

/// Find contiguous shift groups in a non-in-place Concat.
/// Returns groups as (src_start, dest_start, length).
fn find_shift_groups(
    args: &[RegisterId],
    concat_width: usize,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<Vec<(usize, usize, usize)>> {
    // Collect (src_bit, dest_bit) for each non-zero element
    let mut mappings: Vec<(usize, usize)> = Vec::new();
    for (i, &arg) in args.iter().enumerate() {
        let dest_pos = concat_width - 1 - i;
        if is_zero(arg, defs) {
            continue;
        }
        let info = resolve_bit_source(arg, defs)?;
        let src_pos = match info {
            BitSource::Register { bit_position, .. } => bit_position,
            BitSource::Load { bit_position, .. } => bit_position,
        };
        mappings.push((src_pos, dest_pos));
    }

    if mappings.len() < 3 {
        return None;
    }

    // Sort by src_bit
    mappings.sort_by_key(|&(src, _)| src);

    // Find contiguous groups: consecutive src bits with constant (dest - src) delta
    let mut groups: Vec<(usize, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < mappings.len() {
        let (src_start, dest_start) = mappings[i];
        let delta = dest_start as isize - src_start as isize;
        let mut len = 1usize;

        while i + len < mappings.len() {
            let (next_src, next_dest) = mappings[i + len];
            let next_delta = next_dest as isize - next_src as isize;
            if next_src == src_start + len && next_delta == delta {
                len += 1;
            } else {
                break;
            }
        }

        groups.push((src_start, dest_start, len));
        i += len;
    }

    // Only worth it if we have fewer groups than individual bits
    if groups.len() >= mappings.len() / 2 {
        return None;
    }

    Some(groups)
}

/// Exact instruction count after the ordinary bit-extract replacement lowers
/// a temporary leaf Concat. This mirrors the emission cases below and is used
/// only after lane analysis has already proved the leaf shape and load epoch.
fn bit_extract_pack_instruction_count(
    args: &[RegisterId],
    concat_width: usize,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<usize> {
    let mut register_source = None;
    let mut load_address = None;
    let mut mask = 0u64;
    let mut in_place = true;
    let mut extract_count = 0usize;
    let mut load_based = false;

    for (index, &arg) in args.iter().enumerate() {
        let concat_position = concat_width.checked_sub(index + 1)?;
        if is_zero(arg, defs) {
            continue;
        }
        match resolve_bit_source(arg, defs)? {
            BitSource::Register {
                source,
                bit_position,
            } => {
                if load_based || register_source.is_some_and(|known| known != source) {
                    return None;
                }
                register_source.get_or_insert(source);
                if bit_position >= 64 {
                    return None;
                }
                in_place &= bit_position == concat_position;
                mask |= 1u64 << bit_position;
            }
            BitSource::Load { addr, bit_position } => {
                if register_source.is_some()
                    || load_address.is_some_and(|known| known != addr)
                    || bit_position >= 64
                {
                    return None;
                }
                load_based = true;
                load_address.get_or_insert(addr);
                in_place &= bit_position == concat_position;
                mask |= 1u64 << bit_position;
            }
        }
        extract_count += 1;
    }
    if extract_count < 3 {
        return None;
    }

    if in_place {
        let full_mask = mask
            == if concat_width == 64 {
                u64::MAX
            } else {
                (1u64 << concat_width) - 1
            };
        return if register_source.is_some() {
            Some(if full_mask { 1 } else { 2 })
        } else if load_address.is_some() {
            Some(if full_mask { 1 } else { 3 })
        } else {
            None
        };
    }

    register_source?;
    let groups = find_shift_groups(args, concat_width, defs)?;
    // The actual lowering uses 0/2 instructions for extraction, masking and
    // placement, then (groups - 1) ORs plus one final identity.
    let count = groups.iter().fold(0usize, |count, &(src, dst, len)| {
        let group_mask = if len >= 64 {
            u64::MAX
        } else {
            (1u64 << len) - 1
        };
        count
            .saturating_add(if src == 0 { 0 } else { 2 })
            .saturating_add(
                if group_mask == u64::MAX || (src == 0 && len >= concat_width) {
                    0
                } else {
                    2
                },
            )
            .saturating_add(if dst == 0 { 0 } else { 2 })
    });
    Some(
        count
            .saturating_add(groups.len().saturating_sub(1))
            .saturating_add(1),
    )
}

fn vectorize_concats(
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    next_reg: &mut usize,
    global_defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
    written_addresses: &HashSet<RegionedAbsoluteAddr>,
    register_use_counts: &HashMap<RegisterId, usize>,
    lane_definition_credits: &HashSet<RegisterId>,
    claimed_lane_definitions: &mut HashSet<RegisterId>,
) -> bool {
    let defs = global_defs;

    let mut replacements: Vec<Replacement> = Vec::new();
    let mut lane_analysis = HashMap::<Vec<RegisterId>, LanePackKind>::default();
    // Scanning instruction order is dominance order within a block. Cache all
    // exact lane vectors materialized at an earlier Concat (including child
    // vectors of a recursive plan), so a reconvergent DAG is emitted once.
    let mut packed_vectors = HashMap::<Vec<RegisterId>, RegisterId>::default();

    for (idx, inst) in instructions.iter().enumerate() {
        let SIRInstruction::Concat(dst, args) = inst else {
            continue;
        };
        let key = args.clone();

        if let Some(&packed) = packed_vectors.get(&key) {
            replacements.push(Replacement::LaneDag {
                inst_idx: idx,
                instructions: vec![SIRInstruction::Unary(*dst, UnaryOp::Ident, packed)],
            });
            continue;
        }

        let Some(concat_width) = concat_width(args, register_map) else {
            continue;
        };
        if !(3..=64).contains(&concat_width) {
            packed_vectors.insert(key, *dst);
            continue;
        }

        if let Some((low, _low_width, prefix_width)) =
            find_sign_extend(args, concat_width, register_map, defs)
        {
            replacements.push(Replacement::SignExtend {
                inst_idx: idx,
                dst: *dst,
                low,
                width: concat_width,
                prefix_width,
            });
            packed_vectors.insert(key, *dst);
            continue;
        }

        // Check each arg is 1-bit wide
        let all_single_bit = args
            .iter()
            .all(|arg| register_map.get(arg).is_some_and(|rt| rt.width() == 1));
        if !all_single_bit {
            packed_vectors.insert(key, *dst);
            continue;
        }

        if analyze_lane_pack(
            args,
            register_map,
            defs,
            written_addresses,
            lane_definition_credits,
            claimed_lane_definitions,
            &mut lane_analysis,
        ) && !matches!(lane_analysis.get(args), Some(LanePackKind::Leaf))
            && let Some(new_instructions) = materialize_lane_pack(
                args,
                *dst,
                concat_width,
                &lane_analysis,
                defs,
                register_use_counts,
                &mut packed_vectors,
                register_map,
                next_reg,
            )
        {
            replacements.push(Replacement::LaneDag {
                inst_idx: idx,
                instructions: new_instructions,
            });
            continue;
        }

        // Classify: all from same register source, or all from same Load address
        let mut reg_source: Option<RegisterId> = None;
        let mut load_addr: Option<RegionedAbsoluteAddr> = None;
        let mut mask: u64 = 0;
        let mut in_place = true;
        let mut extract_count = 0usize;
        let mut valid = true;
        let mut is_load_based = false;

        for (i, &arg) in args.iter().enumerate() {
            let concat_position = concat_width - 1 - i; // LSB = 0

            if is_zero(arg, defs) {
                continue;
            }

            match resolve_bit_source(arg, defs) {
                Some(BitSource::Register {
                    source,
                    bit_position,
                }) => {
                    if is_load_based {
                        valid = false;
                        break;
                    }
                    match reg_source {
                        Some(s) if s != source => {
                            valid = false;
                            break;
                        }
                        None => reg_source = Some(source),
                        _ => {}
                    }
                    if bit_position >= 64 {
                        valid = false;
                        break;
                    }
                    if bit_position != concat_position {
                        in_place = false;
                    }
                    mask |= 1u64 << bit_position;
                    extract_count += 1;
                }
                Some(BitSource::Load { addr, bit_position }) => {
                    if written_addresses.contains(&addr) {
                        valid = false;
                        break;
                    }
                    if reg_source.is_some() {
                        valid = false;
                        break;
                    }
                    is_load_based = true;
                    match load_addr {
                        Some(a) if a != addr => {
                            valid = false;
                            break;
                        }
                        None => load_addr = Some(addr),
                        _ => {}
                    }
                    if bit_position >= 64 {
                        valid = false;
                        break;
                    }
                    if bit_position != concat_position {
                        in_place = false;
                    }
                    mask |= 1u64 << bit_position;
                    extract_count += 1;
                }
                None => {
                    valid = false;
                    break;
                }
            }
        }

        if !valid || extract_count < 3 {
            packed_vectors.insert(key, *dst);
            continue;
        }

        if in_place {
            if let Some(source) = reg_source {
                replacements.push(Replacement::RegisterAnd {
                    inst_idx: idx,
                    dst: *dst,
                    source,
                    mask,
                    width: concat_width,
                });
            } else if let Some(addr) = load_addr {
                replacements.push(Replacement::LoadAnd {
                    inst_idx: idx,
                    dst: *dst,
                    addr,
                    mask,
                    width: concat_width,
                });
            }
        } else if let Some(source) = reg_source {
            // Non-in-place register case: try grouped shift optimization.
            if let Some(groups) = find_shift_groups(args, concat_width, defs) {
                replacements.push(Replacement::GroupedShift {
                    inst_idx: idx,
                    dst: *dst,
                    source,
                    groups,
                    width: concat_width,
                });
            }
        }
        packed_vectors.insert(key, *dst);
    }

    if replacements.is_empty() {
        return false;
    }

    let immediate_width = |value: usize| (usize::BITS - value.leading_zeros()).max(1) as usize;

    // Apply in reverse to preserve indices
    for repl in replacements.into_iter().rev() {
        // Check if mask covers all bits → And can be omitted
        let is_full_mask = |mask: u64, width: usize| -> bool {
            width <= 64
                && mask
                    == (if width == 64 {
                        u64::MAX
                    } else {
                        (1u64 << width) - 1
                    })
        };

        match repl {
            Replacement::RegisterAnd {
                inst_idx,
                dst,
                source,
                mask,
                width,
            } => {
                if is_full_mask(mask, width) {
                    // All bits extracted → just alias the source
                    instructions[inst_idx] = SIRInstruction::Unary(dst, UnaryOp::Ident, source);
                } else {
                    let mask_reg = alloc_unsigned_reg(next_reg, register_map, width);
                    let mask_value = SIRValue {
                        payload: BigUint::from(mask),
                        mask: BigUint::ZERO,
                    };
                    instructions.insert(inst_idx, SIRInstruction::Imm(mask_reg, mask_value));
                    instructions[inst_idx + 1] =
                        SIRInstruction::Binary(dst, source, BinaryOp::And, mask_reg);
                }
            }
            Replacement::LoadAnd {
                inst_idx,
                dst,
                addr,
                mask,
                width,
            } => {
                if is_full_mask(mask, width) {
                    // All bits extracted → just a wide Load
                    instructions[inst_idx] =
                        SIRInstruction::Load(dst, addr, SIROffset::Static(0), width);
                } else {
                    let load_reg = alloc_unsigned_reg(next_reg, register_map, width);
                    let mask_reg = alloc_unsigned_reg(next_reg, register_map, width);
                    let mask_value = SIRValue {
                        payload: BigUint::from(mask),
                        mask: BigUint::ZERO,
                    };
                    instructions.insert(
                        inst_idx,
                        SIRInstruction::Load(load_reg, addr, SIROffset::Static(0), width),
                    );
                    instructions.insert(inst_idx + 1, SIRInstruction::Imm(mask_reg, mask_value));
                    instructions[inst_idx + 2] =
                        SIRInstruction::Binary(dst, load_reg, BinaryOp::And, mask_reg);
                }
            }
            Replacement::GroupedShift {
                inst_idx,
                dst,
                source,
                groups,
                width,
            } => {
                // Generate: for each group, extract+shift, then OR all together.
                // result = (((src >> s0) & m0) << d0) | (((src >> s1) & m1) << d1) | ...
                let mut new_insts: Vec<SIRInstruction<RegionedAbsoluteAddr>> = Vec::new();
                let mut group_regs: Vec<RegisterId> = Vec::new();

                for &(src_start, dest_start, group_len) in &groups {
                    let group_mask = if group_len >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << group_len) - 1
                    };

                    // Extract: (src >> src_start) & group_mask
                    let extracted = if src_start == 0 {
                        source
                    } else {
                        let shift_reg =
                            alloc_unsigned_reg(next_reg, register_map, immediate_width(src_start));
                        let shifted_reg = alloc_unsigned_reg(next_reg, register_map, width);
                        new_insts.push(SIRInstruction::Imm(
                            shift_reg,
                            SIRValue::new(src_start as u64),
                        ));
                        new_insts.push(SIRInstruction::Binary(
                            shifted_reg,
                            source,
                            BinaryOp::Shr,
                            shift_reg,
                        ));
                        shifted_reg
                    };

                    let masked = if group_mask == u64::MAX || (src_start == 0 && group_len >= width)
                    {
                        extracted
                    } else {
                        let mask_reg = alloc_unsigned_reg(next_reg, register_map, width);
                        let masked_reg = alloc_unsigned_reg(next_reg, register_map, width);
                        new_insts.push(SIRInstruction::Imm(mask_reg, SIRValue::new(group_mask)));
                        new_insts.push(SIRInstruction::Binary(
                            masked_reg,
                            extracted,
                            BinaryOp::And,
                            mask_reg,
                        ));
                        masked_reg
                    };

                    // Place: extracted value (bit-0 based) shifted to dest_start
                    let placed = if dest_start == 0 {
                        masked
                    } else {
                        let shift_reg =
                            alloc_unsigned_reg(next_reg, register_map, immediate_width(dest_start));
                        let placed_reg = alloc_unsigned_reg(next_reg, register_map, width);
                        new_insts.push(SIRInstruction::Imm(
                            shift_reg,
                            SIRValue::new(dest_start as u64),
                        ));
                        new_insts.push(SIRInstruction::Binary(
                            placed_reg,
                            masked,
                            BinaryOp::Shl,
                            shift_reg,
                        ));
                        placed_reg
                    };

                    group_regs.push(placed);
                }

                // OR all group results together
                let mut result = group_regs[0];
                for &gr in &group_regs[1..] {
                    let or_reg = alloc_unsigned_reg(next_reg, register_map, width);
                    new_insts.push(SIRInstruction::Binary(or_reg, result, BinaryOp::Or, gr));
                    result = or_reg;
                }

                // Replace Concat with identity from result
                new_insts.push(SIRInstruction::Unary(dst, UnaryOp::Ident, result));

                // Insert all new instructions at inst_idx, remove the Concat
                instructions.splice(inst_idx..=inst_idx, new_insts);
            }
            Replacement::SignExtend {
                inst_idx,
                dst,
                low,
                width,
                prefix_width,
            } => {
                let shift_reg =
                    alloc_unsigned_reg(next_reg, register_map, immediate_width(prefix_width));
                let shifted_reg = alloc_unsigned_reg(next_reg, register_map, width);
                instructions.splice(
                    inst_idx..=inst_idx,
                    [
                        SIRInstruction::Imm(shift_reg, SIRValue::new(prefix_width as u64)),
                        SIRInstruction::Binary(shifted_reg, low, BinaryOp::Shl, shift_reg),
                        SIRInstruction::Binary(dst, shifted_reg, BinaryOp::Sar, shift_reg),
                    ],
                );
            }
            Replacement::LaneDag {
                inst_idx,
                instructions: new_instructions,
            } => {
                instructions.splice(inst_idx..=inst_idx, new_instructions);
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, BlockId, InstanceId, SIRTerminator, STABLE_REGION};
    use veryl_analyzer::ir::VarId;

    fn test_addr() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        }
    }

    fn make_eu(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        register_map: HashMap<RegisterId, RegisterType>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    #[test]
    fn grouped_high_bit_extract_uses_wide_enough_shift_amount() {
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
        for reg in 1..=5 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }
        register_map.insert(
            RegisterId(6),
            RegisterType::Bit {
                width: 5,
                signed: false,
            },
        );
        let mut eu = make_eu(
            vec![
                SIRInstruction::Slice(RegisterId(1), RegisterId(0), 42, 1),
                SIRInstruction::Slice(RegisterId(2), RegisterId(0), 41, 1),
                SIRInstruction::Slice(RegisterId(3), RegisterId(0), 40, 1),
                SIRInstruction::Slice(RegisterId(4), RegisterId(0), 43, 1),
                SIRInstruction::Imm(RegisterId(5), SIRValue::new(0u8)),
                SIRInstruction::Concat(
                    RegisterId(6),
                    vec![
                        RegisterId(4),
                        RegisterId(1),
                        RegisterId(2),
                        RegisterId(3),
                        RegisterId(5),
                    ],
                ),
                SIRInstruction::RuntimeEvent {
                    site_id: 0,
                    args: vec![RegisterId(6)],
                },
            ],
            register_map,
        );
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];

        VectorizeConcatPass.run(&mut eu, &PassOptions::default());

        eu.verify();
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            let SIRInstruction::Imm(dst, value) = inst else {
                return false;
            };
            value.payload == BigUint::from(40u8) && eu.register_map[dst].width() >= 6
        }));
    }

    #[test]
    fn sign_extend_concat_from_shifted_low_msb() {
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(1),
            RegisterType::Bit {
                width: 16,
                signed: false,
            },
        );
        for reg in 2..=5 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }

        let instructions = vec![
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(7u64)),
            SIRInstruction::Binary(RegisterId(3), RegisterId(0), BinaryOp::Shr, RegisterId(2)),
            SIRInstruction::Imm(RegisterId(4), SIRValue::new(1u64)),
            SIRInstruction::Binary(RegisterId(5), RegisterId(3), BinaryOp::And, RegisterId(4)),
            SIRInstruction::Concat(
                RegisterId(1),
                vec![
                    RegisterId(5),
                    RegisterId(5),
                    RegisterId(5),
                    RegisterId(5),
                    RegisterId(5),
                    RegisterId(5),
                    RegisterId(5),
                    RegisterId(5),
                    RegisterId(0),
                ],
            ),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(1)],
            },
        ];

        let mut eu = make_eu(instructions, register_map);
        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        let block = eu.blocks.get(&BlockId(0)).unwrap();

        assert!(block.instructions.iter().any(|inst| matches!(
            inst,
            SIRInstruction::Binary(RegisterId(1), _, BinaryOp::Sar, _)
        )));
        assert!(
            !block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Concat(..)))
        );
    }

    #[test]
    fn sign_extend_concat_from_load_msb() {
        let addr = test_addr();
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(1),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(2),
            RegisterType::Bit {
                width: 16,
                signed: false,
            },
        );

        let instructions = vec![
            SIRInstruction::Load(RegisterId(0), addr, SIROffset::Static(4), 8),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(11), 1),
            SIRInstruction::Concat(
                RegisterId(2),
                vec![
                    RegisterId(1),
                    RegisterId(1),
                    RegisterId(1),
                    RegisterId(1),
                    RegisterId(1),
                    RegisterId(1),
                    RegisterId(1),
                    RegisterId(1),
                    RegisterId(0),
                ],
            ),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(2)],
            },
        ];

        let mut eu = make_eu(instructions, register_map);
        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        let block = eu.blocks.get(&BlockId(0)).unwrap();

        assert!(block.instructions.iter().any(|inst| matches!(
            inst,
            SIRInstruction::Binary(RegisterId(2), _, BinaryOp::Sar, _)
        )));
    }

    #[test]
    fn lifts_concat_of_bitwise_and_lanes() {
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        );
        register_map.insert(
            RegisterId(1),
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        );
        for reg in 2..=10 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }
        register_map.insert(
            RegisterId(11),
            RegisterType::Bit {
                width: 3,
                signed: false,
            },
        );

        let instructions = vec![
            SIRInstruction::Slice(RegisterId(2), RegisterId(0), 0, 1),
            SIRInstruction::Slice(RegisterId(3), RegisterId(0), 1, 1),
            SIRInstruction::Slice(RegisterId(4), RegisterId(0), 2, 1),
            SIRInstruction::Slice(RegisterId(5), RegisterId(1), 0, 1),
            SIRInstruction::Slice(RegisterId(6), RegisterId(1), 1, 1),
            SIRInstruction::Slice(RegisterId(7), RegisterId(1), 2, 1),
            SIRInstruction::Binary(RegisterId(8), RegisterId(2), BinaryOp::And, RegisterId(5)),
            SIRInstruction::Binary(RegisterId(9), RegisterId(3), BinaryOp::And, RegisterId(6)),
            SIRInstruction::Binary(RegisterId(10), RegisterId(4), BinaryOp::And, RegisterId(7)),
            SIRInstruction::Concat(
                RegisterId(11),
                vec![RegisterId(10), RegisterId(9), RegisterId(8)],
            ),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(11)],
            },
        ];

        let mut eu = make_eu(instructions, register_map);
        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        let block = eu.blocks.get(&BlockId(0)).unwrap();

        let Some(SIRInstruction::Binary(RegisterId(11), lhs_vec, BinaryOp::And, rhs_vec)) =
            block.instructions.iter().find(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(11), _, BinaryOp::And, _)
                )
            })
        else {
            panic!("expected lifted word And");
        };

        assert!(block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Unary(dst, UnaryOp::Ident, RegisterId(0)) if *dst == *lhs_vec
            )
        }));
        assert!(block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Unary(dst, UnaryOp::Ident, RegisterId(1)) if *dst == *rhs_vec
            )
        }));
        assert!(!block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Concat(RegisterId(11), args)
                    if args == &vec![RegisterId(10), RegisterId(9), RegisterId(8)]
            )
        }));
    }

    #[test]
    fn recursively_packs_deep_lane_boolean_dag_to_fixed_point() {
        const LANES: usize = 8;
        const SOURCES: usize = 6;

        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let mut register_map = HashMap::default();
        for source in 0..SOURCES {
            register_map.insert(RegisterId(source), bit(LANES));
        }

        let mut next_reg = SOURCES;
        let mut instructions = Vec::new();
        let mut lane_results = Vec::new();
        for lane in 0..LANES {
            let mut extracted = Vec::new();
            for source in 0..SOURCES {
                let dst = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(dst, bit(1));
                instructions.push(SIRInstruction::Slice(dst, RegisterId(source), lane, 1));
                extracted.push(dst);
            }

            let mut negate = |source: RegisterId| {
                let dst = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(dst, bit(1));
                instructions.push(SIRInstruction::Unary(dst, UnaryOp::LogicNot, source));
                dst
            };
            let not_2 = negate(extracted[2]);
            let not_4 = negate(extracted[4]);
            let not_5 = negate(extracted[5]);

            let mut and = |lhs: RegisterId, rhs: RegisterId| {
                let dst = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(dst, bit(1));
                instructions.push(SIRInstruction::Binary(dst, lhs, BinaryOp::LogicAnd, rhs));
                dst
            };
            let value = and(extracted[0], extracted[1]);
            let value = and(value, not_2);
            let value = and(value, extracted[3]);
            let value = and(value, not_4);
            lane_results.push(and(value, not_5));
        }

        lane_results.reverse();
        let result = RegisterId(next_reg);
        register_map.insert(result, bit(LANES));
        instructions.push(SIRInstruction::Concat(result, lane_results));
        instructions.push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![result],
        });

        let mut eu = make_eu(instructions, register_map);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = (0..SOURCES).map(RegisterId).collect();
        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        eu.verify();

        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert!(
            !instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Concat(..) | SIRInstruction::Slice(..)))
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)))
                .count(),
            3
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::And, _)))
                .count(),
            5
        );
        assert!(instructions.iter().any(|inst| {
            matches!(inst, SIRInstruction::Binary(dst, _, BinaryOp::And, _) if *dst == result)
        }));
    }

    #[test]
    fn packs_very_deep_lane_unary_dag_without_recursion() {
        const LANES: usize = 3;
        const DEPTH: usize = 8_192;

        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(LANES));
        let mut next_reg = 1usize;
        let mut instructions = Vec::new();
        let mut lane_results = Vec::new();
        for lane in 0..LANES {
            let mut value = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(value, bit(1));
            instructions.push(SIRInstruction::Slice(value, RegisterId(0), lane, 1));
            for _ in 0..DEPTH {
                let next = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(next, bit(1));
                instructions.push(SIRInstruction::Unary(next, UnaryOp::LogicNot, value));
                value = next;
            }
            lane_results.push(value);
        }
        lane_results.reverse();
        let result = RegisterId(next_reg);
        register_map.insert(result, bit(LANES));
        instructions.push(SIRInstruction::Concat(result, lane_results));
        instructions.push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![result],
        });

        let mut eu = make_eu(instructions, register_map);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];
        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        eu.verify();

        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)))
                .count(),
            DEPTH
        );
        assert!(
            !instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Concat(..) | SIRInstruction::Slice(..)))
        );
    }

    #[test]
    fn packs_deep_identity_lanes_without_recursive_source_resolution() {
        const LANES: usize = 3;
        const DEPTH: usize = 20_000;

        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(LANES));
        let mut next_reg = 1usize;
        let mut instructions = Vec::new();
        let mut lane_results = Vec::new();
        for lane in 0..LANES {
            let mut value = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(value, bit(1));
            instructions.push(SIRInstruction::Slice(value, RegisterId(0), lane, 1));
            for _ in 0..DEPTH {
                let next = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(next, bit(1));
                instructions.push(SIRInstruction::Unary(next, UnaryOp::Ident, value));
                value = next;
            }
            lane_results.push(value);
        }
        lane_results.reverse();
        let result = RegisterId(next_reg);
        register_map.insert(result, bit(LANES));
        instructions.push(SIRInstruction::Concat(result, lane_results));
        instructions.push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![result],
        });

        let mut eu = make_eu(instructions, register_map);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];
        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        eu.verify();

        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Unary(_, UnaryOp::Ident, _)))
                .count(),
            1
        );
        assert!(matches!(
            instructions.first(),
            Some(SIRInstruction::Unary(dst, UnaryOp::Ident, RegisterId(0))) if *dst == result
        ));
    }

    #[test]
    fn shares_reconvergent_lane_dag_linearly() {
        const LANES: usize = 3;
        const DEPTH: usize = 2_048;

        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(LANES));
        let mut next_reg = 1usize;
        let mut instructions = Vec::new();
        let mut lane_results = Vec::new();
        for lane in 0..LANES {
            let mut value = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(value, bit(1));
            instructions.push(SIRInstruction::Slice(value, RegisterId(0), lane, 1));
            for _ in 0..DEPTH {
                let next = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(next, bit(1));
                instructions.push(SIRInstruction::Binary(
                    next,
                    value,
                    BinaryOp::LogicAnd,
                    value,
                ));
                value = next;
            }
            lane_results.push(value);
        }
        lane_results.reverse();
        let result = RegisterId(next_reg);
        register_map.insert(result, bit(LANES));
        instructions.push(SIRInstruction::Concat(result, lane_results));
        instructions.push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![result],
        });

        let mut eu = make_eu(instructions, register_map);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];
        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        eu.verify();

        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::And, _)))
                .count(),
            DEPTH
        );
        assert!(instructions.len() <= DEPTH + 3, "{instructions:#?}");
    }

    #[test]
    fn rejects_misaligned_shared_lane_product_before_code_growth() {
        // Each lane is a small layered modulo automaton. Following the LHS/RHS
        // edges synchronously represents subset sums modulo all lane moduli;
        // their product has far more tuple states than the sum of scalar
        // states. Structural definition credits must stop that product.
        const MODULI: [usize; 8] = [2, 3, 5, 7, 11, 13, 17, 19];
        const DEPTH: usize = 18;
        let lanes = MODULI.len();
        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(lanes));
        let mut next_reg = 1usize;
        let mut instructions = Vec::new();
        let mut roots = Vec::new();

        for (lane, modulus) in MODULI.into_iter().enumerate() {
            let leaf = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(leaf, bit(1));
            instructions.push(SIRInstruction::Slice(leaf, RegisterId(0), lane, 1));
            let mut previous = vec![leaf; modulus];
            let mut weight = 1usize % modulus;
            for _ in 0..DEPTH {
                let mut current = Vec::with_capacity(modulus);
                for state in 0..modulus {
                    let result = RegisterId(next_reg);
                    next_reg += 1;
                    register_map.insert(result, bit(1));
                    instructions.push(SIRInstruction::Binary(
                        result,
                        previous[state],
                        BinaryOp::And,
                        previous[(state + weight) % modulus],
                    ));
                    current.push(result);
                }
                previous = current;
                weight = (weight * 2) % modulus;
            }
            roots.push(previous[0]);
        }

        roots.reverse();
        let result = RegisterId(next_reg);
        register_map.insert(result, bit(lanes));
        instructions.push(SIRInstruction::Concat(result, roots));
        instructions.push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![result],
        });
        let original_instruction_count = instructions.len();

        let mut eu = make_eu(instructions, register_map);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(0)];
        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        eu.verify();

        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert_eq!(instructions.len(), original_instruction_count);
        assert!(instructions.iter().any(
            |instruction| matches!(instruction, SIRInstruction::Concat(dst, _) if *dst == result)
        ));
    }

    #[test]
    fn many_independent_lane_roots_emit_linear_code() {
        const ROOTS: usize = 256;
        const LANES: usize = 3;
        const DEPTH: usize = 8;
        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let mut register_map = HashMap::default();
        let mut next_reg = ROOTS;
        let mut instructions = Vec::new();
        let mut results = Vec::new();

        for source in 0..ROOTS {
            register_map.insert(RegisterId(source), bit(LANES));
            let mut lane_results = Vec::new();
            for lane in 0..LANES {
                let mut value = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(value, bit(1));
                instructions.push(SIRInstruction::Slice(value, RegisterId(source), lane, 1));
                for _ in 0..DEPTH {
                    let next = RegisterId(next_reg);
                    next_reg += 1;
                    register_map.insert(next, bit(1));
                    instructions.push(SIRInstruction::Unary(next, UnaryOp::LogicNot, value));
                    value = next;
                }
                lane_results.push(value);
            }
            lane_results.reverse();
            let result = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(result, bit(LANES));
            instructions.push(SIRInstruction::Concat(result, lane_results));
            instructions.push(SIRInstruction::RuntimeEvent {
                site_id: source as u32,
                args: vec![result],
            });
            results.push(result);
        }

        let mut eu = make_eu(instructions, register_map);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = (0..ROOTS).map(RegisterId).collect();
        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        eu.verify();

        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert!(
            !instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Concat(..)))
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(
                    instruction,
                    SIRInstruction::Unary(_, UnaryOp::BitNot, _)
                ))
                .count(),
            ROOTS * DEPTH
        );
        assert!(instructions.len() <= ROOTS * (DEPTH + 2));
        assert_eq!(results.len(), ROOTS);
    }

    #[test]
    fn mark_sweep_removes_a_deep_dead_chain_in_one_call() {
        const DEPTH: usize = 20_000;

        let bit = RegisterType::Bit {
            width: 1,
            signed: false,
        };
        let mut register_map = HashMap::default();
        let mut instructions = Vec::with_capacity(DEPTH + 2);
        register_map.insert(RegisterId(0), bit.clone());
        instructions.push(SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)));
        for index in 1..=DEPTH {
            register_map.insert(RegisterId(index), bit.clone());
            instructions.push(SIRInstruction::Unary(
                RegisterId(index),
                UnaryOp::Ident,
                RegisterId(index - 1),
            ));
        }
        instructions.push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: Vec::new(),
        });

        let mut eu = make_eu(instructions, register_map);
        remove_dead_definitions(&mut eu);
        eu.verify();
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions.as_slice(),
            [SIRInstruction::RuntimeEvent { .. }]
        ));
    }

    #[test]
    fn load_based_pack_rejects_an_address_written_by_the_unit() {
        let addr = test_addr();
        let mut register_map = HashMap::default();
        for reg in 0..3 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }
        register_map.insert(
            RegisterId(3),
            RegisterType::Bit {
                width: 3,
                signed: false,
            },
        );
        let mut eu = make_eu(
            vec![
                SIRInstruction::Load(RegisterId(0), addr, SIROffset::Static(0), 1),
                SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(1), 1),
                SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(2), 1),
                SIRInstruction::Concat(
                    RegisterId(3),
                    vec![RegisterId(2), RegisterId(1), RegisterId(0)],
                ),
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    3,
                    RegisterId(3),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            register_map,
        );

        VectorizeConcatPass.run(&mut eu, &PassOptions::default());
        eu.verify();
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Concat(RegisterId(3), _)))
        );
    }

    #[test]
    fn keeps_concat_in_four_state_mode() {
        let mut register_map = HashMap::default();
        for (reg, width) in [(0, 1), (1, 1), (2, 1), (3, 3)] {
            register_map.insert(RegisterId(reg), RegisterType::Logic { width });
        }
        let instructions = vec![SIRInstruction::Concat(
            RegisterId(3),
            vec![RegisterId(2), RegisterId(1), RegisterId(0)],
        )];

        let mut eu = make_eu(instructions, register_map);
        VectorizeConcatPass.run(
            &mut eu,
            &PassOptions {
                four_state: true,
                ..PassOptions::default()
            },
        );
        let block = eu.blocks.get(&BlockId(0)).unwrap();

        assert!(matches!(
            block.instructions.as_slice(),
            [SIRInstruction::Concat(RegisterId(3), _)]
        ));
    }
}
