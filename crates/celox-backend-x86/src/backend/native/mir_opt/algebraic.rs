//! Algebraic simplification and bitwise-mask sinking.

use super::*;

// ────────────────────────────────────────────────────────────────
// Phase 1D: Algebraic simplification
// ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct MaskAndSite {
    block: usize,
    instruction: usize,
}

struct MaskAndPlan {
    block: usize,
    root: usize,
    nodes: Vec<usize>,
    combined_mask: u64,
}

fn is_mask_and_instruction(inst: &MInst) -> bool {
    matches!(
        inst,
        MInst::And { .. } | MInst::And32 { .. } | MInst::AndImm { .. } | MInst::AndImm32 { .. }
    )
}

fn resolve_mask_alias(mut value: VReg, aliases: &HashMap<VReg, VReg>) -> VReg {
    while let Some(&source) = aliases.get(&value) {
        value = source;
    }
    value
}

fn transient_vreg(func: &mut MFunction) -> VReg {
    let value = func.vregs.alloc();
    while func.spill_descs.len() <= value.0 as usize {
        func.spill_descs.push(SpillDesc::transient());
    }
    value
}

/// Sink leaf truncations through an exclusively consumed bitwise AND
/// component.
///
/// Element-strided state deliberately permits non-zero physical padding, so a
/// load of a one-bit element really does require `& 1`. Applying that mask to
/// every leaf is redundant, however:
///
/// `(a & m) & (b & m) & (c & m) == (a & b & c) & m`.
///
/// Definitions stay at their original positions. Mask destinations become
/// aliases of their unmasked sources, each existing AND consumes those aliases,
/// and one combined mask remains at the component root. Restricting traversal
/// to single-use definitions in one block prevents live-range extension across
/// CFG edges and leaves shared expressions untouched.
///
/// Definition, use, and consumer tables are built once. Exclusive components
/// are disjoint, so total traversal and rewriting are O(instructions + operand
/// edges) time and O(vregs + instructions) space.
fn sink_bitwise_and_masks(func: &mut MFunction) {
    let value_count = func.vregs.count() as usize;
    let mut definitions = vec![None::<MaskAndSite>; value_count];
    let mut use_counts = vec![0usize; value_count];
    let mut consumers = vec![None::<MaskAndSite>; value_count];

    for (block_index, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                let index = source.0 as usize;
                use_counts[index] += 1;
                consumers[index] = None;
            }
        }
        for (instruction_index, instruction) in block.insts.iter().enumerate() {
            if let Some(dst) = instruction.def() {
                definitions[dst.0 as usize] = Some(MaskAndSite {
                    block: block_index,
                    instruction: instruction_index,
                });
            }
            let site = MaskAndSite {
                block: block_index,
                instruction: instruction_index,
            };
            for source in instruction.uses() {
                let index = source.0 as usize;
                if use_counts[index] == 0 {
                    consumers[index] = Some(site);
                } else {
                    consumers[index] = None;
                }
                use_counts[index] += 1;
            }
        }
    }

    let mut plans = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (root_index, root_instruction) in block.insts.iter().enumerate() {
            if !is_mask_and_instruction(root_instruction) {
                continue;
            }
            let root_value = root_instruction
                .def()
                .expect("mask/AND instruction must define a value");
            let root_value_index = root_value.0 as usize;
            let has_local_parent = use_counts[root_value_index] == 1
                && consumers[root_value_index].is_some_and(|consumer| {
                    consumer.block == block_index
                        && is_mask_and_instruction(
                            &func.blocks[consumer.block].insts[consumer.instruction],
                        )
                });
            if has_local_parent {
                continue;
            }

            let mut stack = vec![(root_value, true)];
            let mut nodes = Vec::new();
            let mut visited = HashSet::default();
            let mut combined_mask = u64::MAX;
            let mut immediate_masks = 0usize;
            let mut has_binary_and = false;

            while let Some((value, is_root)) = stack.pop() {
                let value_index = value.0 as usize;
                if !is_root && use_counts[value_index] != 1 {
                    continue;
                }
                let Some(site) = definitions[value_index] else {
                    continue;
                };
                if site.block != block_index || !visited.insert(site.instruction) {
                    continue;
                }
                match &func.blocks[site.block].insts[site.instruction] {
                    MInst::And { lhs, rhs, .. } => {
                        has_binary_and = true;
                        nodes.push(site.instruction);
                        stack.push((*rhs, false));
                        stack.push((*lhs, false));
                    }
                    MInst::And32 { lhs, rhs, .. } => {
                        has_binary_and = true;
                        combined_mask &= u64::from(u32::MAX);
                        nodes.push(site.instruction);
                        stack.push((*rhs, false));
                        stack.push((*lhs, false));
                    }
                    MInst::AndImm { src, imm, .. } => {
                        immediate_masks += 1;
                        combined_mask &= *imm;
                        nodes.push(site.instruction);
                        stack.push((*src, false));
                    }
                    MInst::AndImm32 { src, imm, .. } => {
                        immediate_masks += 1;
                        combined_mask &= u64::from(*imm);
                        nodes.push(site.instruction);
                        stack.push((*src, false));
                    }
                    _ => {}
                }
            }

            if has_binary_and && immediate_masks >= 2 {
                nodes.sort_unstable();
                plans.push(MaskAndPlan {
                    block: block_index,
                    root: root_index,
                    nodes,
                    combined_mask,
                });
            }
        }
    }

    let mut removals = vec![HashSet::<usize>::default(); func.blocks.len()];
    let mut replacements = vec![BTreeMap::<usize, Vec<MInst>>::new(); func.blocks.len()];
    for plan in plans {
        let root_instruction = func.blocks[plan.block].insts[plan.root].clone();
        let root_is_mask = matches!(
            root_instruction,
            MInst::AndImm { .. } | MInst::AndImm32 { .. }
        );
        let word32 = plan.combined_mask <= u64::from(u32::MAX);
        let root_raw_destination =
            (!root_is_mask && plan.combined_mask != u64::MAX).then(|| transient_vreg(func));
        let original = &func.blocks[plan.block].insts;
        let mut aliases = HashMap::default();

        for &instruction_index in &plan.nodes {
            match &original[instruction_index] {
                MInst::AndImm { dst, src, .. } | MInst::AndImm32 { dst, src, .. } => {
                    aliases.insert(*dst, resolve_mask_alias(*src, &aliases));
                    if instruction_index != plan.root {
                        removals[plan.block].insert(instruction_index);
                    }
                }
                _ => {}
            }
        }

        for &instruction_index in &plan.nodes {
            let replacement = match &original[instruction_index] {
                MInst::And { dst, lhs, rhs } | MInst::And32 { dst, lhs, rhs } => {
                    let lhs = resolve_mask_alias(*lhs, &aliases);
                    let rhs = resolve_mask_alias(*rhs, &aliases);
                    let raw_dst = if instruction_index == plan.root {
                        root_raw_destination.unwrap_or(*dst)
                    } else {
                        *dst
                    };
                    let raw = if word32 {
                        MInst::And32 {
                            dst: raw_dst,
                            lhs,
                            rhs,
                        }
                    } else {
                        MInst::And {
                            dst: raw_dst,
                            lhs,
                            rhs,
                        }
                    };
                    if instruction_index == plan.root
                        && !root_is_mask
                        && plan.combined_mask != u64::MAX
                    {
                        let mask = if word32 {
                            MInst::AndImm32 {
                                dst: *dst,
                                src: raw_dst,
                                imm: plan.combined_mask as u32,
                            }
                        } else {
                            MInst::AndImm {
                                dst: *dst,
                                src: raw_dst,
                                imm: plan.combined_mask,
                            }
                        };
                        vec![raw, mask]
                    } else {
                        vec![raw]
                    }
                }
                MInst::AndImm { dst, src, .. } | MInst::AndImm32 { dst, src, .. }
                    if instruction_index == plan.root =>
                {
                    let src = resolve_mask_alias(*src, &aliases);
                    if plan.combined_mask == u64::MAX {
                        vec![MInst::Mov { dst: *dst, src }]
                    } else if word32 {
                        vec![MInst::AndImm32 {
                            dst: *dst,
                            src,
                            imm: plan.combined_mask as u32,
                        }]
                    } else {
                        vec![MInst::AndImm {
                            dst: *dst,
                            src,
                            imm: plan.combined_mask,
                        }]
                    }
                }
                _ => continue,
            };
            replacements[plan.block].insert(instruction_index, replacement);
        }
    }

    for (block_index, block) in func.blocks.iter_mut().enumerate() {
        if removals[block_index].is_empty() && replacements[block_index].is_empty() {
            continue;
        }
        let original = std::mem::take(&mut block.insts);
        let mut rewritten = Vec::with_capacity(original.len());
        for (instruction_index, instruction) in original.into_iter().enumerate() {
            if removals[block_index].contains(&instruction_index) {
                continue;
            }
            if let Some(replacement) = replacements[block_index].remove(&instruction_index) {
                rewritten.extend(replacement);
            } else {
                rewritten.push(instruction);
            }
        }
        block.insts = rewritten;
    }
}

/// Algebraic simplification: identity, annihilation, self-inverse, and
/// strength reduction rules.
pub(super) fn algebraic_simplify(func: &mut MFunction) {
    sink_bitwise_and_masks(func);

    // Build def map for constant lookups
    let mut consts: HashMap<VReg, u64> = HashMap::default();
    let mut and_immediates: HashMap<VReg, (VReg, u64)> = HashMap::default();
    let mut and_immediates32: HashMap<VReg, (VReg, u32)> = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            match inst {
                MInst::LoadImm { dst, value } => {
                    consts.insert(*dst, *value);
                }
                MInst::AndImm { dst, src, imm } => {
                    and_immediates.insert(*dst, (*src, *imm));
                }
                MInst::AndImm32 { dst, src, imm } => {
                    and_immediates32.insert(*dst, (*src, *imm));
                }
                _ => {}
            }
        }
    }

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let replacement = match inst {
                // Identity: add x, 0 → x
                MInst::Add { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else {
                        None
                    }
                }
                // The 32-bit form includes a zero extension, so its identity
                // replacement must remain Mov32 rather than a full-word copy.
                MInst::Add32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else {
                        None
                    }
                }
                // Identity: sub x, 0 → x; self: sub x, x → 0
                MInst::Sub { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                MInst::Sub32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // Identity: mul x, 1 → x; annihilation: mul x, 0 → 0
                // Strength reduction: mul x, 2^n → shl x, n
                MInst::Mul { dst, lhs, rhs } => try_simplify_mul(*dst, *lhs, *rhs, &consts),
                MInst::Mul32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(1) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(1) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else if const32(&consts, *rhs) == Some(0) || const32(&consts, *lhs) == Some(0)
                    {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // Identity: and x, -1 → x; annihilation: and x, 0 → 0
                MInst::And { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&u64::MAX) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&u64::MAX) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else if consts.get(rhs) == Some(&0) || consts.get(lhs) == Some(&0) {
                        Some(Simplification::Const(*dst, 0))
                    } else if lhs == rhs {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else {
                        None
                    }
                }
                MInst::And32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(u32::MAX) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(u32::MAX) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else if const32(&consts, *rhs) == Some(0) || const32(&consts, *lhs) == Some(0)
                    {
                        Some(Simplification::Const(*dst, 0))
                    } else if lhs == rhs {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else {
                        None
                    }
                }
                // Identity: or x, 0 → x; self: or x, x → x
                MInst::Or { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else {
                        None
                    }
                }
                MInst::Or32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else {
                        None
                    }
                }
                // Identity: xor x, 0 → x; self: xor x, x → 0
                MInst::Xor { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                MInst::Xor32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // Identity: shr/shl/sar x, 0 → x
                MInst::Shr { dst, lhs, rhs }
                | MInst::Shl { dst, lhs, rhs }
                | MInst::Sar { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else {
                        None
                    }
                }
                MInst::ShrImm { dst, src, imm: 0 }
                | MInst::ShlImm { dst, src, imm: 0 }
                | MInst::SarImm { dst, src, imm: 0 } => Some(Simplification::Mov(*dst, *src)),
                // AND chain: and(x, m) with immediate where m is mask
                MInst::AndImm { dst, src, imm } => {
                    if *imm == u64::MAX {
                        Some(Simplification::Mov(*dst, *src))
                    } else if *imm == 0 {
                        Some(Simplification::Const(*dst, 0))
                    } else if let Some(&(original, previous)) = and_immediates.get(src) {
                        let combined = previous & *imm;
                        if combined == 0 {
                            Some(Simplification::Const(*dst, 0))
                        } else if combined == u64::MAX {
                            Some(Simplification::Mov(*dst, original))
                        } else {
                            Some(Simplification::AndImm(*dst, original, combined))
                        }
                    } else if let Some(&(original, previous)) = and_immediates32.get(src) {
                        // A 32-bit definition has already zero-extended the
                        // high half. Preserve that fact by keeping the
                        // replacement in the 32-bit instruction domain.
                        let combined = previous & (*imm as u32);
                        if combined == 0 {
                            Some(Simplification::Const(*dst, 0))
                        } else if combined == u32::MAX {
                            Some(Simplification::Mov32(*dst, original))
                        } else {
                            Some(Simplification::AndImm32(*dst, original, combined))
                        }
                    } else {
                        None
                    }
                }
                MInst::AndImm32 { dst, src, imm } => {
                    if *imm == u32::MAX {
                        Some(Simplification::Mov32(*dst, *src))
                    } else if *imm == 0 {
                        Some(Simplification::Const(*dst, 0))
                    } else if let Some(&(original, previous)) = and_immediates32.get(src) {
                        let combined = previous & *imm;
                        if combined == 0 {
                            Some(Simplification::Const(*dst, 0))
                        } else if combined == u32::MAX {
                            Some(Simplification::Mov32(*dst, original))
                        } else {
                            Some(Simplification::AndImm32(*dst, original, combined))
                        }
                    } else if let Some(&(original, previous)) = and_immediates.get(src) {
                        // And32 observes only the low 32 bits and
                        // zero-extends its result, regardless of the producer
                        // width.
                        let combined = (previous as u32) & *imm;
                        if combined == 0 {
                            Some(Simplification::Const(*dst, 0))
                        } else if combined == u32::MAX {
                            Some(Simplification::Mov32(*dst, original))
                        } else {
                            Some(Simplification::AndImm32(*dst, original, combined))
                        }
                    } else {
                        None
                    }
                }
                // OrImm identity and `(x & keep) | set → x | set` when the OR
                // restores every bit cleared by the preceding AND.
                MInst::OrImm { dst, src, imm } => {
                    if *imm == 0 {
                        Some(Simplification::Mov(*dst, *src))
                    } else if and_immediates
                        .get(src)
                        .is_some_and(|(_, keep_mask)| *keep_mask | *imm == u64::MAX)
                    {
                        let (original, _) = and_immediates[src];
                        Some(Simplification::OrImm(*dst, original, *imm))
                    } else {
                        None
                    }
                }
                // Double negate
                MInst::BitNot { dst, src } => {
                    if let Some(&c) = consts.get(src) {
                        Some(Simplification::Const(*dst, !c))
                    } else {
                        None
                    }
                }
                MInst::Neg { dst, src } => {
                    if let Some(&c) = consts.get(src) {
                        Some(Simplification::Const(*dst, c.wrapping_neg()))
                    } else {
                        None
                    }
                }
                // Select with constant condition
                MInst::Select {
                    dst,
                    cond,
                    true_val,
                    false_val,
                } => {
                    if let Some(&c) = consts.get(cond) {
                        if c != 0 {
                            Some(Simplification::Mov(*dst, *true_val))
                        } else {
                            Some(Simplification::Mov(*dst, *false_val))
                        }
                    } else {
                        None
                    }
                }
                // Mov of constant → LoadImm (enables further constant folding)
                MInst::Mov { dst, src } => {
                    if let Some(&c) = consts.get(src) {
                        Some(Simplification::Const(*dst, c))
                    } else {
                        None
                    }
                }
                _ => None,
            };

            if let Some(simp) = replacement {
                match simp {
                    Simplification::Mov(dst, src) => {
                        *inst = MInst::Mov { dst, src };
                    }
                    Simplification::Mov32(dst, src) => {
                        *inst = MInst::Mov32 { dst, src };
                    }
                    Simplification::Const(dst, value) => {
                        *inst = MInst::LoadImm { dst, value };
                        consts.insert(dst, value);
                    }
                    Simplification::Shl(dst, src, imm) => {
                        *inst = MInst::ShlImm { dst, src, imm };
                    }
                    Simplification::OrImm(dst, src, imm) => {
                        *inst = MInst::OrImm { dst, src, imm };
                    }
                    Simplification::AndImm(dst, src, imm) => {
                        *inst = MInst::AndImm { dst, src, imm };
                    }
                    Simplification::AndImm32(dst, src, imm) => {
                        *inst = MInst::AndImm32 { dst, src, imm };
                    }
                }
            }
        }
    }
}
