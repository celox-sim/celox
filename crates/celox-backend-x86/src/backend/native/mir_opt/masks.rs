//! Possible-one-bit analysis and redundant mask elimination.

use super::*;

// ────────────────────────────────────────────────────────────────
// Phase 1B: Redundant mask elimination
// ────────────────────────────────────────────────────────────────

/// Return `w` when `mask` is the contiguous low-bit mask `(1 << w) - 1`.
/// Other MIR peepholes also use this shape test when recognizing bit fields.
pub(super) fn mask_width(mask: u64) -> Option<usize> {
    if mask == 0 {
        return Some(0);
    }
    if mask == u64::MAX {
        return Some(64);
    }
    let width = mask.trailing_ones() as usize;
    (mask == (1u64 << width) - 1).then_some(width)
}

/// Redundant mask elimination over the two machine widths represented by MIR.
///
/// A scalar "known width" misses non-contiguous masks and, more importantly,
/// used to forget the zero-extension semantics of the explicit 32-bit MIR
/// operations.  Track a conservative set of bits which may be one instead.
/// This is the ordinary known-bits lattice restricted to the fact needed by
/// this pass: `x & mask == x` exactly when every possible one-bit of `x` is in
/// `mask`. Facts follow SSA values through the complete CFG, including phis.
/// Rewrites remain at the original definition and use sites, so propagating a
/// fact across a block boundary does not extend a live range or add an
/// ordering constraint.
pub(super) fn redundant_mask_eliminate(func: &mut MFunction) {
    let mut constants: HashMap<VReg, u64> = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                constants.insert(*dst, *value);
            }
        }
    }
    let possible_ones = global_possible_one_bits(func, &constants);

    for block in &mut func.blocks {
        let mut definitions: HashMap<VReg, MaskDefinition> = HashMap::default();

        for inst in &mut block.insts {
            let should_replace =
                redundant_mask_action(inst, &possible_ones, &constants, &definitions);

            if let Some(action) = should_replace {
                match action {
                    MaskElimAction::Mov(dst, src) => {
                        *inst = MInst::Mov { dst, src };
                    }
                    MaskElimAction::Mov32(dst, src) => *inst = MInst::Mov32 { dst, src },
                    MaskElimAction::FoldAnd(dst, inner, folded_mask) => {
                        *inst = MInst::AndImm {
                            dst,
                            src: inner,
                            imm: folded_mask,
                        };
                    }
                    MaskElimAction::FoldAnd32(dst, inner, folded_mask) => {
                        *inst = MInst::AndImm32 {
                            dst,
                            src: inner,
                            imm: folded_mask,
                        };
                    }
                }
            }

            if let Some(dst) = inst.def() {
                if let Some(definition) = MaskDefinition::from_inst(inst) {
                    definitions.insert(dst, definition);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ValueDefinition {
    Phi { block: usize, phi: usize },
    Instruction { block: usize, instruction: usize },
}

/// Solve possible-one facts on the SSA def-use graph.
///
/// Facts only move from unknown (`u64::MAX`) toward a smaller set of possible
/// bits. A changed definition schedules exactly its SSA users, so an acyclic
/// chain is visited once after its inputs settle and a loop is revisited only
/// when a backedge fact actually improves.
pub(super) fn global_possible_one_bits(
    func: &MFunction,
    constants: &HashMap<VReg, u64>,
) -> Vec<u64> {
    let value_count = func.vregs.count() as usize;
    let mut definitions = vec![None::<ValueDefinition>; value_count];
    let mut users = vec![Vec::<VReg>::new(); value_count];
    let mut queue = VecDeque::new();
    let mut queued = vec![false; value_count];

    for (block_index, block) in func.blocks.iter().enumerate() {
        for (phi_index, phi) in block.phis.iter().enumerate() {
            let destination = phi.dst.0 as usize;
            definitions[destination] = Some(ValueDefinition::Phi {
                block: block_index,
                phi: phi_index,
            });
            queue.push_back(phi.dst);
            queued[destination] = true;
            for &(_, source) in &phi.sources {
                users[source.0 as usize].push(phi.dst);
            }
        }
        for (instruction_index, instruction) in block.insts.iter().enumerate() {
            let Some(dst) = instruction.def() else {
                continue;
            };
            let destination = dst.0 as usize;
            definitions[destination] = Some(ValueDefinition::Instruction {
                block: block_index,
                instruction: instruction_index,
            });
            queue.push_back(dst);
            queued[destination] = true;
            for source in instruction.uses() {
                users[source.0 as usize].push(dst);
            }
        }
    }

    let mut possible_ones = vec![u64::MAX; value_count];
    while let Some(value) = queue.pop_front() {
        let value_index = value.0 as usize;
        queued[value_index] = false;
        let Some(definition) = definitions[value_index] else {
            continue;
        };
        let computed = match definition {
            ValueDefinition::Phi { block, phi } => func.blocks[block].phis[phi]
                .sources
                .iter()
                .map(|(_, source)| possible_bits(*source, &possible_ones, constants))
                .fold(0, |possible, source| possible | source),
            ValueDefinition::Instruction { block, instruction } => compute_possible_one_bits(
                &func.blocks[block].insts[instruction],
                &possible_ones,
                constants,
            ),
        };
        let old = possible_ones[value_index];
        let improved = old & computed;
        if improved == old {
            continue;
        }
        possible_ones[value_index] = improved;
        for &user in &users[value_index] {
            let user_index = user.0 as usize;
            if !queued[user_index] {
                queued[user_index] = true;
                queue.push_back(user);
            }
        }
    }
    possible_ones
}

enum MaskElimAction {
    Mov(VReg, VReg),
    Mov32(VReg, VReg),
    FoldAnd(VReg, VReg, u64),
    FoldAnd32(VReg, VReg, u32),
}

#[derive(Clone, Copy)]
enum MaskDefinition {
    Register { lhs: VReg, rhs: VReg, word32: bool },
    Immediate { src: VReg, mask: u64, word32: bool },
}

impl MaskDefinition {
    fn from_inst(inst: &MInst) -> Option<Self> {
        match inst {
            MInst::And { lhs, rhs, .. } => Some(Self::Register {
                lhs: *lhs,
                rhs: *rhs,
                word32: false,
            }),
            MInst::And32 { lhs, rhs, .. } => Some(Self::Register {
                lhs: *lhs,
                rhs: *rhs,
                word32: true,
            }),
            MInst::AndImm { src, imm, .. } => Some(Self::Immediate {
                src: *src,
                mask: *imm,
                word32: false,
            }),
            MInst::AndImm32 { src, imm, .. } => Some(Self::Immediate {
                src: *src,
                mask: u64::from(*imm),
                word32: true,
            }),
            _ => None,
        }
    }
}

pub(super) fn possible_bits(
    value: VReg,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
) -> u64 {
    possible_ones
        .get(value.0 as usize)
        .filter(|&&possible| possible != u64::MAX)
        .or_else(|| constants.get(&value))
        .copied()
        .unwrap_or(u64::MAX)
}

fn redundant_32_bit_mask_action(
    dst: VReg,
    src: VReg,
    mask: u32,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
) -> Option<MaskElimAction> {
    let source_bits = possible_bits(src, possible_ones, constants);
    let low_mask = u64::from(mask);
    if source_bits & u64::from(u32::MAX) & !low_mask != 0 {
        return None;
    }
    if source_bits & !u64::from(u32::MAX) == 0 {
        Some(MaskElimAction::Mov(dst, src))
    } else {
        // The mask is redundant in the low word, but the operation's required
        // zero-extension is not.  Preserve that machine-width semantic.
        Some(MaskElimAction::Mov32(dst, src))
    }
}

fn and_repeats_operand(definition: Option<&MaskDefinition>, operand: VReg, word32: bool) -> bool {
    match definition {
        Some(MaskDefinition::Register {
            lhs,
            rhs,
            word32: definition_word32,
        }) if *definition_word32 == word32 => *lhs == operand || *rhs == operand,
        _ => false,
    }
}

fn redundant_mask_action(
    inst: &MInst,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
    definitions: &HashMap<VReg, MaskDefinition>,
) -> Option<MaskElimAction> {
    match inst {
        MInst::AndImm { dst, src, imm } => {
            if possible_bits(*src, possible_ones, constants) & !*imm == 0 {
                return Some(MaskElimAction::Mov(*dst, *src));
            }
            match definitions.get(src) {
                Some(MaskDefinition::Immediate {
                    src: inner,
                    mask: first,
                    word32: false,
                }) => Some(MaskElimAction::FoldAnd(*dst, *inner, *first & *imm)),
                Some(MaskDefinition::Immediate {
                    src: inner,
                    mask: first,
                    word32: true,
                }) => Some(MaskElimAction::FoldAnd32(
                    *dst,
                    *inner,
                    *first as u32 & *imm as u32,
                )),
                _ => None,
            }
        }
        MInst::AndImm32 { dst, src, imm } => {
            redundant_32_bit_mask_action(*dst, *src, *imm, possible_ones, constants).or_else(|| {
                match definitions.get(src) {
                    Some(MaskDefinition::Immediate {
                        src: inner,
                        mask: first,
                        ..
                    }) => Some(MaskElimAction::FoldAnd32(
                        *dst,
                        *inner,
                        *first as u32 & *imm,
                    )),
                    _ => None,
                }
            })
        }
        MInst::And { dst, lhs, rhs } => {
            if let Some(&mask) = constants.get(rhs)
                && possible_bits(*lhs, possible_ones, constants) & !mask == 0
            {
                return Some(MaskElimAction::Mov(*dst, *lhs));
            }
            if let Some(&mask) = constants.get(lhs)
                && possible_bits(*rhs, possible_ones, constants) & !mask == 0
            {
                return Some(MaskElimAction::Mov(*dst, *rhs));
            }
            if and_repeats_operand(definitions.get(lhs), *rhs, false) {
                Some(MaskElimAction::Mov(*dst, *lhs))
            } else if and_repeats_operand(definitions.get(rhs), *lhs, false) {
                Some(MaskElimAction::Mov(*dst, *rhs))
            } else {
                None
            }
        }
        MInst::And32 { dst, lhs, rhs } => {
            if let Some(&mask) = constants.get(rhs) {
                return redundant_32_bit_mask_action(
                    *dst,
                    *lhs,
                    mask as u32,
                    possible_ones,
                    constants,
                );
            }
            if let Some(&mask) = constants.get(lhs) {
                return redundant_32_bit_mask_action(
                    *dst,
                    *rhs,
                    mask as u32,
                    possible_ones,
                    constants,
                );
            }
            if and_repeats_operand(definitions.get(lhs), *rhs, true) {
                Some(MaskElimAction::Mov(*dst, *lhs))
            } else if and_repeats_operand(definitions.get(rhs), *lhs, true) {
                Some(MaskElimAction::Mov(*dst, *rhs))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(super) fn machine_width_mask(size: OpSize) -> u64 {
    match size {
        OpSize::S8 => u64::from(u8::MAX),
        OpSize::S16 => u64::from(u16::MAX),
        OpSize::S32 => u64::from(u32::MAX),
        OpSize::S64 => u64::MAX,
    }
}

fn compute_possible_one_bits(
    inst: &MInst,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
) -> u64 {
    let bits = |value| possible_bits(value, possible_ones, constants);
    let low32 = u64::from(u32::MAX);
    match inst {
        MInst::LoadImm { value, .. } => *value,
        MInst::Load { size, .. }
        | MInst::LoadIndexed { size, .. }
        | MInst::LoadPtr { size, .. }
        | MInst::LoadPtrIndexed { size, .. } => machine_width_mask(*size),
        MInst::Mov { src, .. } => bits(*src),
        MInst::Mov32 { src, .. } => bits(*src) & low32,
        MInst::And { lhs, rhs, .. } => bits(*lhs) & bits(*rhs),
        MInst::And32 { lhs, rhs, .. } => bits(*lhs) & bits(*rhs) & low32,
        MInst::AndImm { src, imm, .. } => bits(*src) & *imm,
        MInst::AndImm32 { src, imm, .. } => bits(*src) & u64::from(*imm),
        MInst::Or { lhs, rhs, .. } | MInst::Xor { lhs, rhs, .. } => bits(*lhs) | bits(*rhs),
        MInst::Or32 { lhs, rhs, .. } | MInst::Xor32 { lhs, rhs, .. } => {
            (bits(*lhs) | bits(*rhs)) & low32
        }
        MInst::OrImm { src, imm, .. } => bits(*src) | *imm,
        MInst::Add32 { .. }
        | MInst::Sub32 { .. }
        | MInst::Mul32 { .. }
        | MInst::MulImm32 { .. } => low32,
        MInst::ShrImm { src, imm, .. } => bits(*src).checked_shr(u32::from(*imm)).unwrap_or(0),
        MInst::ShlImm { src, imm, .. } => bits(*src).checked_shl(u32::from(*imm)).unwrap_or(0),
        MInst::Cmp { .. } | MInst::CmpImm { .. } => 1,
        MInst::Popcnt { .. } => 0x7f,
        // Bit-scan destinations are unspecified for a zero input. This pass has
        // no path-sensitive nonzero fact, so every output bit remains possible.
        MInst::Bsf { .. } | MInst::Bsr { .. } => u64::MAX,
        MInst::BsrOr { zero_value, .. } => 0x3f | u64::from(*zero_value),
        MInst::Select {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpImmSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::GuardedCmpSelect {
            true_val,
            false_val,
            ..
        } => bits(*true_val) | bits(*false_val),
        _ => u64::MAX,
    }
}
