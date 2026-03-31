//! Instruction Selection: lowers SIR (bit-level SSA) to MIR (word-level SSA).
//!
//! Supports 2-state and 4-state (IEEE 1800) with full mask propagation.
//! Handles arbitrary widths: narrow (≤64-bit) and wide (>64-bit, chunk-based).

use crate::ir::{
    BinaryOp, ExecutionUnit, RegisterId, RegisterType, SIRInstruction, SIROffset, SIRTerminator,
    UnaryOp,
};
use crate::ir::{RegionedAbsoluteAddr, STABLE_REGION};

use super::mir::*;
use crate::backend::MemoryLayout;

/// Maps SIR RegisterId → MIR VReg for the current execution unit.
struct RegMap {
    map: Vec<Option<VReg>>,
}

impl RegMap {
    fn new(capacity: usize) -> Self {
        Self {
            map: vec![None; capacity],
        }
    }

    fn get(&self, reg: RegisterId) -> VReg {
        self.map[reg.0].unwrap_or_else(|| panic!("SIR register r{} not yet defined", reg.0))
    }

    fn set(&mut self, reg: RegisterId, vreg: VReg) {
        self.map[reg.0] = Some(vreg);
    }
}

/// Lower a single SIR execution unit to a MIR function.
///
/// Only handles 2-state values ≤64 bits for now.
pub fn lower_execution_unit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
) -> MFunction {
    let mut vregs = VRegAllocator::new();
    let mut spill_descs: Vec<SpillDesc> = Vec::new();
    let max_sir_regs = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0) + 1;
    let mut reg_map = RegMap::new(max_sir_regs);

    // Pre-allocate a VReg for each SIR register
    for sir_reg_id in eu.register_map.keys() {
        let vreg = vregs.alloc();
        reg_map.set(*sir_reg_id, vreg);
        // Spill desc will be filled during instruction lowering.
        // For now, default to transient.
        spill_descs.push(SpillDesc::transient());
    }

    let mut func = MFunction::new(vregs.clone(), spill_descs);

    // Walk blocks in SIR order (entry first, then others).
    // Collect block IDs in a deterministic order.
    let entry_id = eu.entry_block_id;
    let mut block_ids: Vec<crate::ir::BlockId> = Vec::new();
    block_ids.push(entry_id);
    let mut sorted_ids: Vec<_> = eu.blocks.keys().copied().collect();
    sorted_ids.sort();
    for bid in sorted_ids {
        if bid != entry_id {
            block_ids.push(bid);
        }
    }

    let mut mask_map = RegMap::new(max_sir_regs);
    // Pre-allocate mask VRegs for 4-state
    if four_state {
        for sir_reg_id in eu.register_map.keys() {
            let mvreg = func.vregs.alloc();
            mask_map.set(*sir_reg_id, mvreg);
            func.spill_descs.push(SpillDesc::transient());
        }
    }

    let mut ctx = ISelContext {
        vregs: &mut func.vregs,
        spill_descs: &mut func.spill_descs,
        reg_map: &mut reg_map,
        register_types: &eu.register_map,
        layout,
        wide_regs: WideRegMap::default(),
        reg_addrs: crate::HashMap::default(),
        consts: ConstMap::default(),
        four_state,
        mask_map,
        known_bits: crate::HashMap::default(),
        wide_masks: WideRegMap::default(),
    };

    // Collect mask phi sources per-block (captures mask state at each terminator)
    let mut mask_phi_sources: std::collections::HashMap<
        BlockId,
        Vec<(BlockId, usize, Option<VReg>)>,
    > = std::collections::HashMap::new();

    for &sir_block_id in &block_ids {
        let sir_block = &eu.blocks[&sir_block_id];
        let mir_block_id = BlockId(sir_block_id.0 as u32);
        let mut mblock = MBlock::new(mir_block_id);

        // Pre-scan: record Store target addresses for Slice memory fallback.
        // When SIR does Store(addr, ..., src_reg, ...) followed by
        // Slice(dst, var_reg, ...), we need to know var_reg's addr.
        // SIR Load(var_reg, addr, ...) gives us var_reg → addr.
        // But if there's no Load (only Store + Slice), we scan for
        // Store instructions that write to the same addr as a Slice's src.
        for inst in &sir_block.instructions {
            if let SIRInstruction::Load(dst, addr, _, _) = inst {
                ctx.reg_addrs.insert(*dst, *addr);
            }
        }

        // Lower instructions
        for inst in &sir_block.instructions {
            lower_instruction(&mut ctx, &mut mblock, inst);

            // Track known bit width for redundant mask elimination.
            let dst_reg = match inst {
                SIRInstruction::Imm(d, _)
                | SIRInstruction::Binary(d, _, _, _)
                | SIRInstruction::Unary(d, _, _)
                | SIRInstruction::Load(d, _, _, _)
                | SIRInstruction::Concat(d, _)
                | SIRInstruction::Slice(d, _, _, _) => Some(*d),
                SIRInstruction::Store(..) | SIRInstruction::Commit(..) => None,
            };
            if let Some(dr) = dst_reg {
                let w = ctx.sir_width(&dr);
                if w <= 64 {
                    let vreg = ctx.reg_map.get(dr);
                    ctx.known_bits.insert(vreg, w);
                }
            }
        }

        // Lower terminator
        lower_terminator(&mut ctx, &mut mblock, &sir_block.terminator);

        // Capture mask phi sources from this block's terminator (before mask_map changes)
        if four_state {
            let pred_mir_id = BlockId(sir_block_id.0 as u32);
            let edges: Vec<(crate::ir::BlockId, &[RegisterId])> = match &sir_block.terminator {
                SIRTerminator::Jump(target, args) => vec![(*target, args.as_slice())],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => vec![
                    (true_block.0, true_block.1.as_slice()),
                    (false_block.0, false_block.1.as_slice()),
                ],
                _ => vec![],
            };
            for (target_sir_id, args) in edges {
                if args.is_empty() {
                    continue;
                }
                let target_mir_id = BlockId(target_sir_id.0 as u32);
                for (i, arg_reg) in args.iter().enumerate() {
                    let mask_vreg = ctx.mask_map.map.get(arg_reg.0).copied().flatten();
                    mask_phi_sources.entry(target_mir_id).or_default().push((
                        pred_mir_id,
                        i,
                        mask_vreg,
                    ));
                }
            }
        }

        func.blocks.push(mblock);
    }

    // Extract mask_map for phi node construction (ctx borrows func fields)
    let saved_mask_map = std::mem::replace(&mut ctx.mask_map, RegMap::new(0));
    drop(ctx); // Release borrows on func

    // Build phi nodes from SIR block params and predecessor terminators.
    // For each SIR block with params, find all predecessors that pass args.
    {
        use std::collections::HashMap;
        // Collect phi sources: target_block → [(pred_block, param_idx, arg_vreg)]
        let mut phi_sources: HashMap<BlockId, Vec<(BlockId, usize, VReg)>> = HashMap::new();
        for &sir_block_id in &block_ids {
            let sir_block = &eu.blocks[&sir_block_id];
            let pred_mir_id = BlockId(sir_block_id.0 as u32);
            let edges: Vec<(crate::ir::BlockId, &[RegisterId])> = match &sir_block.terminator {
                SIRTerminator::Jump(target, args) => vec![(*target, args.as_slice())],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => vec![
                    (true_block.0, true_block.1.as_slice()),
                    (false_block.0, false_block.1.as_slice()),
                ],
                _ => vec![],
            };
            for (target_sir_id, args) in edges {
                if args.is_empty() {
                    continue;
                }
                let target_mir_id = BlockId(target_sir_id.0 as u32);
                for (i, arg_reg) in args.iter().enumerate() {
                    let arg_vreg = reg_map.get(*arg_reg);
                    phi_sources
                        .entry(target_mir_id)
                        .or_default()
                        .push((pred_mir_id, i, arg_vreg));
                }
            }
        }
        // Build phi nodes on target blocks
        for mblock in &mut func.blocks {
            if let Some(sources) = phi_sources.remove(&mblock.id) {
                let sir_block_id = crate::ir::BlockId(mblock.id.0 as usize);
                let sir_block = &eu.blocks[&sir_block_id];
                for (param_idx, param_reg) in sir_block.params.iter().enumerate() {
                    let dst = reg_map.get(*param_reg);
                    let phi_srcs: Vec<(BlockId, VReg)> = sources
                        .iter()
                        .filter(|(_, idx, _)| *idx == param_idx)
                        .map(|(pred, _, vreg)| (*pred, *vreg))
                        .collect();
                    if !phi_srcs.is_empty() {
                        mblock.phis.push(PhiNode {
                            dst,
                            sources: phi_srcs,
                        });
                    }

                    // 4-state: add mask phi node
                    if four_state {
                        if let Some(mask_dst) =
                            saved_mask_map.map.get(param_reg.0).copied().flatten()
                        {
                            if let Some(m_sources) = mask_phi_sources.get(&mblock.id) {
                                let mask_phi_srcs: Vec<(BlockId, VReg)> = m_sources
                                    .iter()
                                    .filter(|(_, idx, _)| *idx == param_idx)
                                    .filter_map(|(pred, _, m)| m.map(|mv| (*pred, mv)))
                                    .collect();
                                if !mask_phi_srcs.is_empty() {
                                    mblock.phis.push(PhiNode {
                                        dst: mask_dst,
                                        sources: mask_phi_srcs,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Update spill_descs to match final vreg count
    while func.spill_descs.len() < func.vregs.count() as usize {
        func.spill_descs.push(SpillDesc::transient());
    }

    func
}

/// Compute a bitmask of `width` bits (e.g., width=8 → 0xFF).
/// Returns u64::MAX for width >= 64 to avoid shift overflow.
#[inline]
fn mask_for_width(width: usize) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Tracks wide (>64-bit) register values as multiple 64-bit chunks.
/// Key: SIR RegisterId, Value: chunks in LSB-first order (vreg, width_bits).
/// Used for Concat results and multi-word arithmetic (Shl/And/Or/BitNot on >64-bit values).
type WideRegMap = crate::HashMap<RegisterId, Vec<(VReg, usize)>>;

/// Tracks known constant values for SIR registers (for constant folding in ISel).
type ConstMap = crate::HashMap<RegisterId, u64>;

struct ISelContext<'a> {
    vregs: &'a mut VRegAllocator,
    spill_descs: &'a mut Vec<SpillDesc>,
    reg_map: &'a mut RegMap,
    register_types: &'a crate::HashMap<RegisterId, RegisterType>,
    layout: &'a MemoryLayout,
    wide_regs: WideRegMap,
    /// Known constant values for SIR registers (from Imm, Mul of constants, etc.)
    consts: ConstMap,
    /// RegisterId → sim state address, recorded at Store instructions.
    /// Used by Slice to load directly from memory instead of stale VRegs.
    reg_addrs: crate::HashMap<RegisterId, RegionedAbsoluteAddr>,
    /// Whether 4-state simulation is enabled.
    four_state: bool,
    /// Maps SIR RegisterId → mask VReg (parallel to reg_map).
    mask_map: RegMap,
    /// Known effective bit width per VReg. If a VReg is known to have at most
    /// `w` significant bits (upper bits guaranteed zero), AND masking to `w`
    /// bits can be elided. Populated by Load (movzx), Cmp (0/1), AndImm, etc.
    known_bits: crate::HashMap<VReg, usize>,
    /// Wide mask chunks (parallel to wide_regs).
    wide_masks: WideRegMap,
}

impl<'a> ISelContext<'a> {
    /// Allocate a fresh VReg with the given spill descriptor.
    fn alloc_vreg(&mut self, desc: SpillDesc) -> VReg {
        let vreg = self.vregs.alloc();
        // Grow spill_descs if needed
        while self.spill_descs.len() <= vreg.0 as usize {
            self.spill_descs.push(SpillDesc::transient());
        }
        self.spill_descs[vreg.0 as usize] = desc;
        vreg
    }

    /// Get the bit width of a SIR register.
    fn sir_width(&self, reg: &RegisterId) -> usize {
        self.register_types[reg].width()
    }

    /// Get the mask VReg for a SIR register (zero constant if not 4-state).
    fn get_mask(&mut self, reg: RegisterId, block: &mut MBlock) -> VReg {
        if self.four_state {
            self.mask_map.map[reg.0].unwrap_or_else(|| {
                // Not yet defined — return zero
                let z = self.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                z
            })
        } else {
            let z = self.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        }
    }

    /// Set the mask VReg for a SIR register.
    fn set_mask(&mut self, reg: RegisterId, vreg: VReg) {
        if self.four_state {
            self.mask_map.set(reg, vreg);
        }
    }

    /// Resolve the mask byte offset for a variable.
    /// The mask is stored immediately after the value in memory.
    fn mask_byte_offset(&self, addr: &RegionedAbsoluteAddr, bit_offset: usize) -> i32 {
        let abs_addr = addr.absolute_addr();
        let width = self.layout.widths.get(&abs_addr).copied().unwrap_or(0);
        let byte_size = crate::backend::get_byte_size(width);
        self.byte_offset(addr, bit_offset) + byte_size as i32
    }

    /// Whether the given address refers to a 4-state variable.
    fn is_4state_var(&self, addr: &RegionedAbsoluteAddr) -> bool {
        self.four_state
            && self
                .layout
                .is_4states
                .get(&addr.absolute_addr())
                .copied()
                .unwrap_or(false)
    }

    /// Resolve byte offset for a regioned address + bit offset.
    fn byte_offset(&self, addr: &RegionedAbsoluteAddr, bit_offset: usize) -> i32 {
        let abs_addr = addr.absolute_addr();
        let base = if addr.region == STABLE_REGION {
            *self.layout.offsets.get(&abs_addr).unwrap_or(&0)
        } else {
            self.layout.working_base_offset
                + *self.layout.working_offsets.get(&abs_addr).unwrap_or(&0)
        };
        (base + bit_offset / 8) as i32
    }

    /// Choose OpSize for a given bit width, clamping to the smallest
    /// native size that fits.
    fn op_size_for_width(width_bits: usize) -> OpSize {
        match width_bits {
            0..=8 => OpSize::S8,
            9..=16 => OpSize::S16,
            17..=32 => OpSize::S32,
            _ => OpSize::S64,
        }
    }

    /// Emit AND with immediate, handling 64-bit values that don't fit i32.
    /// Elides the AND entirely if the source is already known to fit within
    /// the mask (redundant mask elimination).
    fn emit_and_imm(&mut self, block: &mut MBlock, dst: VReg, src: VReg, imm: u64) {
        let signed = imm as i64;
        if imm == u64::MAX {
            // AND with all-ones is identity
            if dst != src {
                block.push(MInst::Mov { dst, src });
            }
            return;
        }

        // Check if src is already known to fit within the mask.
        // mask_for_width(w) = (1 << w) - 1. If src's known_bits <= w,
        // the AND is redundant.
        if let Some(&src_bits) = self.known_bits.get(&src) {
            // imm = mask_for_width(w) means all bits above w are 0.
            // If src_bits <= w, src already has zeros above w.
            let mask_width = 64 - imm.leading_zeros() as usize; // bits needed to represent imm
            if imm == mask_for_width(mask_width) && src_bits <= mask_width {
                // Redundant AND: src is already within mask
                if dst != src {
                    block.push(MInst::Mov { dst, src });
                    self.known_bits.insert(dst, src_bits);
                } else {
                    // dst == src: complete no-op
                }
                return;
            }
        }

        // Track output known bits
        let out_bits = 64 - imm.leading_zeros() as usize;
        if imm == mask_for_width(out_bits) {
            self.known_bits.insert(dst, out_bits);
        }

        if (signed >= i32::MIN as i64 && signed <= i32::MAX as i64) || imm <= u32::MAX as u64 {
            block.push(MInst::AndImm { dst, src, imm });
        } else {
            // 64-bit immediate: decompose into LoadImm + And
            let tmp = self.alloc_vreg(SpillDesc::remat(imm));
            block.push(MInst::LoadImm {
                dst: tmp,
                value: imm,
            });
            block.push(MInst::And {
                dst,
                lhs: src,
                rhs: tmp,
            });
        }
    }

    /// Emit bitfield insert: dst = (base_word & ~(mask << shift)) | ((val & mask) << shift)
    /// Decomposes into basic ALU ops (no pseudo-instruction).
    fn emit_bfi(
        &mut self,
        block: &mut MBlock,
        dst: VReg,
        base_word: VReg,
        val: VReg,
        shift: u8,
        mask: u64,
    ) {
        let clear_mask = !(mask << shift);
        // cleared = base_word & clear_mask
        let cleared = self.alloc_vreg(SpillDesc::transient());
        self.emit_and_imm(block, cleared, base_word, clear_mask);
        // masked_val = val & mask
        let masked_val = self.alloc_vreg(SpillDesc::transient());
        if mask != u64::MAX {
            self.emit_and_imm(block, masked_val, val, mask);
        } else {
            block.push(MInst::Mov {
                dst: masked_val,
                src: val,
            });
        }
        // shifted_val = masked_val << shift
        if shift > 0 {
            let shifted = self.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: shifted,
                src: masked_val,
                imm: shift,
            });
            block.push(MInst::Or {
                dst,
                lhs: cleared,
                rhs: shifted,
            });
        } else {
            block.push(MInst::Or {
                dst,
                lhs: cleared,
                rhs: masked_val,
            });
        }
    }

    /// Number of 64-bit chunks needed for a given bit width.
    fn num_chunks(width_bits: usize) -> usize {
        width_bits.div_ceil(64)
    }

    /// Get or create wide chunks for a SIR register.
    /// If the register is already tracked as wide, returns existing chunks.
    /// If it's a scalar (≤64-bit), promotes it to a wide value with zero-extended chunks.
    fn get_wide_chunks(&mut self, reg: &RegisterId, block: &mut MBlock) -> Vec<(VReg, usize)> {
        if let Some(chunks) = self.wide_regs.get(reg) {
            return chunks.clone();
        }
        // Scalar register: promote to wide by putting it in chunk 0, zeros elsewhere
        let vreg = self.reg_map.get(*reg);
        let width = self.sir_width(reg);
        let n_chunks = Self::num_chunks(width);
        let mut chunks = Vec::with_capacity(n_chunks);
        let chunk0_width = width.min(64);
        chunks.push((vreg, chunk0_width));
        for _ in 1..n_chunks {
            let zero = self.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            chunks.push((zero, 64));
        }
        chunks
    }

    /// Store wide chunks for a SIR register in the wide_regs map.
    fn set_wide_chunks(&mut self, reg: RegisterId, chunks: Vec<(VReg, usize)>) {
        self.wide_regs.insert(reg, chunks);
    }

    /// Get chunk `i` from a wide value, or emit a zero constant if missing.
    fn wide_chunk_or_zero(
        &mut self,
        chunks: &[(VReg, usize)],
        i: usize,
        block: &mut MBlock,
    ) -> VReg {
        chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
            let z = self.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        })
    }
}

fn lower_instruction(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) {
    match inst {
        SIRInstruction::Imm(dst, val) => {
            let d_width = ctx.sir_width(dst);
            let digits = val.payload.to_u64_digits();
            let imm_val = digits.first().copied().unwrap_or(0);

            let vreg = ctx.reg_map.get(*dst);
            ctx.spill_descs[vreg.0 as usize] = SpillDesc::remat(imm_val);
            block.push(MInst::LoadImm {
                dst: vreg,
                value: imm_val,
            });
            // Track constant value for later folding
            ctx.consts.insert(*dst, imm_val);
            // Known bits from the constant value
            let imm_bits = if imm_val == 0 {
                0
            } else {
                64 - imm_val.leading_zeros() as usize
            };
            ctx.known_bits.insert(vreg, imm_bits.min(d_width));

            // For wide values, also store chunks in wide_regs
            if d_width > 64 {
                let n_chunks = ISelContext::num_chunks(d_width);
                let mut chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    let chunk_val = digits.get(i).copied().unwrap_or(0);
                    if i == 0 {
                        chunks.push((vreg, 64));
                    } else {
                        let cv = ctx.alloc_vreg(SpillDesc::remat(chunk_val));
                        block.push(MInst::LoadImm {
                            dst: cv,
                            value: chunk_val,
                        });
                        chunks.push((cv, 64));
                    }
                }
                ctx.set_wide_chunks(*dst, chunks);
            }

            // 4-state: load mask immediate
            if ctx.four_state {
                let mask_digits = val.mask.to_u64_digits();
                let mask_val = mask_digits.first().copied().unwrap_or(0);
                let mvreg = ctx.alloc_vreg(SpillDesc::remat(mask_val));
                block.push(MInst::LoadImm {
                    dst: mvreg,
                    value: mask_val,
                });
                ctx.set_mask(*dst, mvreg);

                if d_width > 64 {
                    let n_chunks = ISelContext::num_chunks(d_width);
                    let mut mchunks = Vec::with_capacity(n_chunks);
                    mchunks.push((mvreg, 64));
                    for i in 1..n_chunks {
                        let cv = mask_digits.get(i).copied().unwrap_or(0);
                        let mv = ctx.alloc_vreg(SpillDesc::remat(cv));
                        block.push(MInst::LoadImm { dst: mv, value: cv });
                        mchunks.push((mv, 64));
                    }
                    ctx.wide_masks.insert(*dst, mchunks);
                }
            }
        }

        SIRInstruction::Load(dst, addr, offset, width_bits) => {
            ctx.reg_addrs.insert(*dst, *addr);
            let vreg = ctx.reg_map.get(*dst);

            match offset {
                SIROffset::Static(bit_off) => {
                    // Wide load (>64 bits): chunk-by-chunk
                    if *width_bits > 64 {
                        let n_chunks = ISelContext::num_chunks(*width_bits);
                        let mut chunks = Vec::with_capacity(n_chunks);
                        let mut remaining = *width_bits;
                        let mut bit_pos = *bit_off;
                        for _ in 0..n_chunks {
                            let chunk_bits = remaining.min(64);
                            let chunk_byte_off = ctx.byte_offset(addr, bit_pos);
                            let chunk_size = ISelContext::op_size_for_width(chunk_bits);
                            let chunk_vreg = ctx.alloc_vreg(SpillDesc::sim_state(
                                *addr, bit_pos, chunk_bits, false,
                            ));
                            block.push(MInst::Load {
                                dst: chunk_vreg,
                                base: BaseReg::SimState,
                                offset: chunk_byte_off,
                                size: chunk_size,
                            });
                            chunks.push((chunk_vreg, chunk_bits));
                            bit_pos += chunk_bits;
                            remaining -= chunk_bits;
                        }
                        // Also store chunk[0] in reg_map scalar slot for
                        // fallback paths that read the scalar VReg.
                        block.push(MInst::Mov {
                            dst: vreg,
                            src: chunks[0].0,
                        });
                        ctx.wide_regs.insert(*dst, chunks);

                        // 4-state: load wide mask chunks
                        if ctx.is_4state_var(addr) {
                            let mut mchunks = Vec::with_capacity(n_chunks);
                            let mut m_remaining = *width_bits;
                            let mut m_bit_pos = *bit_off;
                            for _ in 0..n_chunks {
                                let chunk_bits = m_remaining.min(64);
                                let chunk_byte_off = ctx.mask_byte_offset(addr, m_bit_pos);
                                let chunk_size = ISelContext::op_size_for_width(chunk_bits);
                                let mv = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Load {
                                    dst: mv,
                                    base: BaseReg::SimState,
                                    offset: chunk_byte_off,
                                    size: chunk_size,
                                });
                                mchunks.push((mv, chunk_bits));
                                m_bit_pos += chunk_bits;
                                m_remaining -= chunk_bits;
                            }
                            let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Mov {
                                dst: mvreg,
                                src: mchunks[0].0,
                            });
                            ctx.set_mask(*dst, mvreg);
                            ctx.wide_masks.insert(*dst, mchunks);
                        } else if ctx.four_state {
                            let mvreg = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: mvreg,
                                value: 0,
                            });
                            ctx.set_mask(*dst, mvreg);
                        }
                        return;
                    }

                    let byte_off = ctx.byte_offset(addr, *bit_off);
                    let intra_byte = bit_off % 8;
                    let op_size = ISelContext::op_size_for_width(*width_bits);

                    // Update spill desc
                    ctx.spill_descs[vreg.0 as usize] =
                        SpillDesc::sim_state(*addr, *bit_off, *width_bits, false);

                    if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
                        // Word-aligned, native size: single load.
                        // If the load is wider than the variable (SIR optimizer widening),
                        // mask the result to the variable's actual width.
                        let var_width = ctx
                            .layout
                            .widths
                            .get(&addr.absolute_addr())
                            .copied()
                            .unwrap_or(*width_bits);
                        if var_width < *width_bits && var_width < 64 {
                            let raw = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: raw,
                                base: BaseReg::SimState,
                                offset: byte_off,
                                size: op_size,
                            });
                            ctx.emit_and_imm(block, vreg, raw, mask_for_width(var_width));
                        } else {
                            block.push(MInst::Load {
                                dst: vreg,
                                base: BaseReg::SimState,
                                offset: byte_off,
                                size: op_size,
                            });
                        }
                    } else {
                        // Unaligned or non-native width: load containing word + shift + mask
                        let containing_byte_off = ctx.byte_offset(addr, 0) + (bit_off / 8) as i32;
                        let load_size = ISelContext::op_size_for_width(*width_bits + intra_byte);

                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: tmp,
                            base: BaseReg::SimState,
                            offset: containing_byte_off,
                            size: load_size,
                        });

                        if intra_byte > 0 {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: shifted,
                                src: tmp,
                                imm: intra_byte as u8,
                            });
                            let mask = mask_for_width(*width_bits);
                            ctx.emit_and_imm(block, vreg, shifted, mask);
                        } else {
                            // Byte-aligned but non-native width: just mask
                            let mask = mask_for_width(*width_bits);
                            ctx.emit_and_imm(block, vreg, tmp, mask);
                        }
                    }
                }
                SIROffset::Dynamic(offset_reg) => {
                    // Dynamic offset: offset_reg holds total bit offset.
                    // byte_offset = total_bit_offset >> 3
                    // bit_shift = total_bit_offset & 7
                    let offset_vreg = ctx.reg_map.get(*offset_reg);
                    let base_off = ctx.byte_offset(addr, 0);

                    // Compute byte offset and intra-byte bit shift
                    let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: byte_off,
                        src: offset_vreg,
                        imm: 3,
                    });
                    let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                    ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);

                    // Load containing word at [sim + base_off + byte_off]
                    // Use a slightly larger load to account for bit_shift
                    let load_size = ISelContext::op_size_for_width(*width_bits + 7);
                    let raw = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::LoadIndexed {
                        dst: raw,
                        base: BaseReg::SimState,
                        offset: base_off,
                        index: byte_off,
                        size: load_size,
                    });

                    // Shift right by bit_shift (dynamic)
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Shr {
                        dst: shifted,
                        lhs: raw,
                        rhs: bit_shift,
                    });

                    // Mask to width
                    if *width_bits < 64 {
                        let mask = mask_for_width(*width_bits);
                        ctx.emit_and_imm(block, vreg, shifted, mask);
                    } else {
                        block.push(MInst::Mov {
                            dst: vreg,
                            src: shifted,
                        });
                    }
                }
            }

            // 4-state: load mask from memory (narrow path; wide handled above in early-return)
            if ctx.is_4state_var(addr) {
                match offset {
                    SIROffset::Static(bit_off) => {
                        if *width_bits <= 64 {
                            let mask_off = ctx.mask_byte_offset(addr, *bit_off);
                            let intra_byte = bit_off % 8;
                            let op_size = ISelContext::op_size_for_width(*width_bits);
                            let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                            let var_width = ctx
                                .layout
                                .widths
                                .get(&addr.absolute_addr())
                                .copied()
                                .unwrap_or(*width_bits);

                            if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
                                // Mask to actual variable width if SIR optimizer widened the load
                                if var_width < *width_bits && var_width < 64 {
                                    let raw = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Load {
                                        dst: raw,
                                        base: BaseReg::SimState,
                                        offset: mask_off,
                                        size: op_size,
                                    });
                                    ctx.emit_and_imm(block, mvreg, raw, mask_for_width(var_width));
                                } else {
                                    block.push(MInst::Load {
                                        dst: mvreg,
                                        base: BaseReg::SimState,
                                        offset: mask_off,
                                        size: op_size,
                                    });
                                }
                            } else {
                                let containing_off =
                                    ctx.mask_byte_offset(addr, 0) + (bit_off / 8) as i32;
                                let load_size =
                                    ISelContext::op_size_for_width(*width_bits + intra_byte);
                                let tmp = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Load {
                                    dst: tmp,
                                    base: BaseReg::SimState,
                                    offset: containing_off,
                                    size: load_size,
                                });
                                if intra_byte > 0 {
                                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::ShrImm {
                                        dst: shifted,
                                        src: tmp,
                                        imm: intra_byte as u8,
                                    });
                                    ctx.emit_and_imm(
                                        block,
                                        mvreg,
                                        shifted,
                                        mask_for_width(*width_bits),
                                    );
                                } else {
                                    ctx.emit_and_imm(
                                        block,
                                        mvreg,
                                        tmp,
                                        mask_for_width(*width_bits),
                                    );
                                }
                            }
                            ctx.set_mask(*dst, mvreg);
                        }
                    }
                    SIROffset::Dynamic(offset_reg) => {
                        // Dynamic: load mask similarly with indexed addressing
                        let offset_vreg = ctx.reg_map.get(*offset_reg);
                        let mask_base_off = ctx.mask_byte_offset(addr, 0);
                        let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: byte_off,
                            src: offset_vreg,
                            imm: 3,
                        });
                        let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                        ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);
                        let load_size = ISelContext::op_size_for_width(*width_bits + 7);
                        let raw = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::LoadIndexed {
                            dst: raw,
                            base: BaseReg::SimState,
                            offset: mask_base_off,
                            index: byte_off,
                            size: load_size,
                        });
                        let shifted = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Shr {
                            dst: shifted,
                            lhs: raw,
                            rhs: bit_shift,
                        });
                        let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                        if *width_bits < 64 {
                            ctx.emit_and_imm(block, mvreg, shifted, mask_for_width(*width_bits));
                        } else {
                            block.push(MInst::Mov {
                                dst: mvreg,
                                src: shifted,
                            });
                        }
                        ctx.set_mask(*dst, mvreg);
                    }
                }
            } else if ctx.four_state {
                // Non-4-state variable in 4-state mode: mask is always 0
                let mvreg = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: mvreg,
                    value: 0,
                });
                ctx.set_mask(*dst, mvreg);
            }
        }

        SIRInstruction::Store(addr, offset, width_bits, src_reg, triggers) => {
            match offset {
                SIROffset::Static(bit_off) => {
                    // Check for wide value from Concat
                    if *width_bits > 64 {
                        if let Some(chunks) = ctx.wide_regs.get(src_reg).cloned() {
                            // Wide store: emit chunk-by-chunk stores
                            let mut bit_pos = 0usize;
                            for (chunk_vreg, chunk_width) in &chunks {
                                let chunk_bit_off = *bit_off + bit_pos;
                                let chunk_byte_off = ctx.byte_offset(addr, chunk_bit_off);
                                let intra = chunk_bit_off % 8;

                                if intra == 0 && OpSize::from_bits(*chunk_width).is_some() {
                                    block.push(MInst::Store {
                                        base: BaseReg::SimState,
                                        offset: chunk_byte_off,
                                        src: *chunk_vreg,
                                        size: OpSize::from_bits(*chunk_width).unwrap(),
                                    });
                                } else if *chunk_width <= 64 {
                                    // Non-aligned chunk: RMW via BitFieldInsert
                                    let containing_off =
                                        ctx.byte_offset(addr, 0) + (chunk_bit_off / 8) as i32;
                                    let load_size =
                                        ISelContext::op_size_for_width(*chunk_width + intra);
                                    let old = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Load {
                                        dst: old,
                                        base: BaseReg::SimState,
                                        offset: containing_off,
                                        size: load_size,
                                    });
                                    let mask = mask_for_width(*chunk_width);
                                    let new = ctx.alloc_vreg(SpillDesc::transient());
                                    ctx.emit_bfi(block, new, old, *chunk_vreg, intra as u8, mask);
                                    block.push(MInst::Store {
                                        base: BaseReg::SimState,
                                        offset: containing_off,
                                        src: new,
                                        size: load_size,
                                    });
                                }
                                bit_pos += chunk_width;
                            }
                        } else {
                            // Wide store without Concat source: chunk-by-chunk copy
                            // This shouldn't happen in practice since wide stores
                            // come from Concat, but handle it as raw memory copy.
                            let mut remaining = *width_bits;
                            let mut off = ctx.byte_offset(addr, *bit_off);
                            while remaining > 0 {
                                let chunk_bits = remaining.min(64);
                                let chunk_size = ISelContext::op_size_for_width(chunk_bits);
                                let tmp = ctx.alloc_vreg(SpillDesc::transient());
                                // We can't split a single vreg. This is a fallback.
                                block.push(MInst::LoadImm { dst: tmp, value: 0 });
                                block.push(MInst::Store {
                                    base: BaseReg::SimState,
                                    offset: off,
                                    src: tmp,
                                    size: chunk_size,
                                });
                                let advance = chunk_bits.div_ceil(8);
                                off += advance as i32;
                                remaining -= chunk_bits;
                            }
                        }
                        // Skip the rest of the static offset handling
                    } else {
                        let src_vreg = ctx.reg_map.get(*src_reg);
                        let byte_off = ctx.byte_offset(addr, *bit_off);
                        let intra_byte = bit_off % 8;

                        if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
                            // Word-aligned, native size: direct store
                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: byte_off,
                                src: src_vreg,
                                size: OpSize::from_bits(*width_bits).unwrap(),
                            });
                        } else {
                            // Unaligned: RMW via BitFieldInsert
                            let containing_byte_off =
                                ctx.byte_offset(addr, 0) + (bit_off / 8) as i32;
                            let load_size =
                                ISelContext::op_size_for_width(*width_bits + intra_byte);

                            let old_word = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: old_word,
                                base: BaseReg::SimState,
                                offset: containing_byte_off,
                                size: load_size,
                            });

                            let mask = mask_for_width(*width_bits);
                            let new_word = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_bfi(
                                block,
                                new_word,
                                old_word,
                                src_vreg,
                                intra_byte as u8,
                                mask,
                            );

                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: containing_byte_off,
                                src: new_word,
                                size: load_size,
                            });
                        }
                    }
                }
                SIROffset::Dynamic(offset_reg) => {
                    // Dynamic offset store: RMW with register-indexed addressing.
                    let src_vreg = ctx.reg_map.get(*src_reg);
                    let offset_vreg = ctx.reg_map.get(*offset_reg);
                    let base_off = ctx.byte_offset(addr, 0);

                    let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: byte_off,
                        src: offset_vreg,
                        imm: 3,
                    });
                    let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                    ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);

                    let rw_size = ISelContext::op_size_for_width(*width_bits + 7);

                    // Load old word
                    let old_word = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::LoadIndexed {
                        dst: old_word,
                        base: BaseReg::SimState,
                        offset: base_off,
                        index: byte_off,
                        size: rw_size,
                    });

                    // Build mask and shifted value
                    let mask_val = if *width_bits < 64 {
                        mask_for_width(*width_bits)
                    } else {
                        u64::MAX
                    };

                    // masked_src = src & mask
                    let masked_src = ctx.alloc_vreg(SpillDesc::transient());
                    if mask_val != u64::MAX {
                        ctx.emit_and_imm(block, masked_src, src_vreg, mask_val);
                    } else {
                        block.push(MInst::Mov {
                            dst: masked_src,
                            src: src_vreg,
                        });
                    }

                    // shifted_src = masked_src << bit_shift
                    let shifted_src = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Shl {
                        dst: shifted_src,
                        lhs: masked_src,
                        rhs: bit_shift,
                    });

                    // Create shifted mask for clearing: mask_imm_vreg = mask_val
                    let mask_imm = ctx.alloc_vreg(SpillDesc::remat(mask_val));
                    block.push(MInst::LoadImm {
                        dst: mask_imm,
                        value: mask_val,
                    });

                    // shifted_mask = mask_imm << bit_shift
                    let shifted_mask = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Shl {
                        dst: shifted_mask,
                        lhs: mask_imm,
                        rhs: bit_shift,
                    });

                    // not_mask = ~shifted_mask
                    let not_mask = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::BitNot {
                        dst: not_mask,
                        src: shifted_mask,
                    });

                    // cleared = old_word & not_mask
                    let cleared = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::And {
                        dst: cleared,
                        lhs: old_word,
                        rhs: not_mask,
                    });

                    // result = cleared | shifted_src
                    let result = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: result,
                        lhs: cleared,
                        rhs: shifted_src,
                    });

                    // Store back
                    block.push(MInst::StoreIndexed {
                        base: BaseReg::SimState,
                        offset: base_off,
                        index: byte_off,
                        src: result,
                        size: rw_size,
                    });
                }
            }

            // 4-state: store mask to memory
            if ctx.is_4state_var(addr) {
                let mask_vreg = ctx.get_mask(*src_reg, block);
                match offset {
                    SIROffset::Static(bit_off) => {
                        if *width_bits > 64 {
                            // Wide mask store: chunk-by-chunk
                            if let Some(mchunks) = ctx.wide_masks.get(src_reg).cloned() {
                                let mut bit_pos = 0usize;
                                for (mv, cw) in &mchunks {
                                    let off = ctx.mask_byte_offset(addr, *bit_off + bit_pos);
                                    if let Some(sz) = OpSize::from_bits(*cw) {
                                        block.push(MInst::Store {
                                            base: BaseReg::SimState,
                                            offset: off,
                                            src: *mv,
                                            size: sz,
                                        });
                                    }
                                    bit_pos += cw;
                                }
                            }
                        } else {
                            let mask_off = ctx.mask_byte_offset(addr, *bit_off);
                            let intra_byte = bit_off % 8;
                            if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
                                block.push(MInst::Store {
                                    base: BaseReg::SimState,
                                    offset: mask_off,
                                    src: mask_vreg,
                                    size: OpSize::from_bits(*width_bits).unwrap(),
                                });
                            } else {
                                let containing_off =
                                    ctx.mask_byte_offset(addr, 0) + (bit_off / 8) as i32;
                                let load_size =
                                    ISelContext::op_size_for_width(*width_bits + intra_byte);
                                let old = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Load {
                                    dst: old,
                                    base: BaseReg::SimState,
                                    offset: containing_off,
                                    size: load_size,
                                });
                                let new_word = ctx.alloc_vreg(SpillDesc::transient());
                                ctx.emit_bfi(
                                    block,
                                    new_word,
                                    old,
                                    mask_vreg,
                                    intra_byte as u8,
                                    mask_for_width(*width_bits),
                                );
                                block.push(MInst::Store {
                                    base: BaseReg::SimState,
                                    offset: containing_off,
                                    src: new_word,
                                    size: load_size,
                                });
                            }
                        }
                    }
                    SIROffset::Dynamic(offset_reg) => {
                        // Dynamic mask store: same RMW pattern as value store,
                        // but targeting the mask memory region.
                        let offset_vreg = ctx.reg_map.get(*offset_reg);
                        let mask_base_off = ctx.mask_byte_offset(addr, 0);

                        let m_byte_off = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: m_byte_off,
                            src: offset_vreg,
                            imm: 3,
                        });
                        let m_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                        ctx.emit_and_imm(block, m_bit_shift, offset_vreg, 7);

                        let rw_size = ISelContext::op_size_for_width(*width_bits + 7);

                        // Load old mask word
                        let old_mask_word = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::LoadIndexed {
                            dst: old_mask_word,
                            base: BaseReg::SimState,
                            offset: mask_base_off,
                            index: m_byte_off,
                            size: rw_size,
                        });

                        let width_mask = if *width_bits < 64 {
                            mask_for_width(*width_bits)
                        } else {
                            u64::MAX
                        };

                        // masked_m = mask_vreg & width_mask
                        let masked_m = ctx.alloc_vreg(SpillDesc::transient());
                        if width_mask != u64::MAX {
                            ctx.emit_and_imm(block, masked_m, mask_vreg, width_mask);
                        } else {
                            block.push(MInst::Mov {
                                dst: masked_m,
                                src: mask_vreg,
                            });
                        }

                        // shifted_m = masked_m << bit_shift
                        let shifted_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Shl {
                            dst: shifted_m,
                            lhs: masked_m,
                            rhs: m_bit_shift,
                        });

                        // Create clearing mask
                        let clear_mask_imm = ctx.alloc_vreg(SpillDesc::remat(width_mask));
                        block.push(MInst::LoadImm {
                            dst: clear_mask_imm,
                            value: width_mask,
                        });
                        let shifted_clear = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Shl {
                            dst: shifted_clear,
                            lhs: clear_mask_imm,
                            rhs: m_bit_shift,
                        });
                        let not_clear = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_clear,
                            src: shifted_clear,
                        });

                        // cleared = old_mask_word & ~shifted_clear
                        let cleared = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: cleared,
                            lhs: old_mask_word,
                            rhs: not_clear,
                        });

                        // result = cleared | shifted_m
                        let m_result = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: m_result,
                            lhs: cleared,
                            rhs: shifted_m,
                        });

                        // Store back
                        block.push(MInst::StoreIndexed {
                            base: BaseReg::SimState,
                            offset: mask_base_off,
                            index: m_byte_off,
                            src: m_result,
                            size: rw_size,
                        });
                    }
                }
            } else if ctx.four_state {
                // Store to non-4state (bit) variable: zero the source register's mask.
                // This is critical because SIR optimizer inlines intermediate variables,
                // so subsequent uses of src_reg must see mask=0.
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                ctx.set_mask(*src_reg, zero);
            }

            // Trigger detection: compare old vs new value, set triggered_bits
            if !triggers.is_empty() {
                if let SIROffset::Static(bit_off) = offset {
                    // Load new value (just stored)
                    let byte_off = ctx.byte_offset(addr, *bit_off);
                    let new_val = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Load {
                        dst: new_val,
                        base: BaseReg::SimState,
                        offset: byte_off,
                        size: ISelContext::op_size_for_width(*width_bits),
                    });

                    for trigger in triggers {
                        let trigger_byte_idx = trigger.id / 8;
                        let trigger_bit_idx = trigger.id % 8;
                        let trigger_offset = ctx.layout.triggered_bits_offset + trigger_byte_idx;

                        // Check new_val for trigger condition
                        let triggered = ctx.alloc_vreg(SpillDesc::transient());
                        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm {
                            dst: zero,
                            value: 0,
                        });

                        // For posedge/async_high: triggered if new_val != 0
                        // (old value comparison is handled by Simulation's step())
                        block.push(MInst::Cmp {
                            dst: triggered,
                            lhs: new_val,
                            rhs: zero,
                            kind: CmpKind::Ne,
                        });

                        // Load current triggered byte, OR in the bit, store back
                        let old_byte = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: old_byte,
                            base: BaseReg::SimState,
                            offset: trigger_offset as i32,
                            size: OpSize::S8,
                        });

                        let bit_mask = ctx.alloc_vreg(SpillDesc::remat(1u64 << trigger_bit_idx));
                        block.push(MInst::LoadImm {
                            dst: bit_mask,
                            value: 1u64 << trigger_bit_idx,
                        });

                        // conditional: if triggered, OR in the bit
                        let selected_mask = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Select {
                            dst: selected_mask,
                            cond: triggered,
                            true_val: bit_mask,
                            false_val: zero,
                        });

                        let new_byte = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: new_byte,
                            lhs: old_byte,
                            rhs: selected_mask,
                        });

                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: trigger_offset as i32,
                            src: new_byte,
                            size: OpSize::S8,
                        });
                    }
                }
            }
        }

        SIRInstruction::Commit(src_addr, dst_addr, offset, width_bits, _triggers) => {
            // Commit = load from src region, store to dst region (same offset/width)
            match offset {
                SIROffset::Static(bit_off) => {
                    let src_byte_off = ctx.byte_offset(src_addr, *bit_off);
                    let dst_byte_off = ctx.byte_offset(dst_addr, *bit_off);
                    let op_size = ISelContext::op_size_for_width(*width_bits);

                    // For wide commits (> 64 bits), emit chunk-by-chunk
                    if *width_bits <= 64 {
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: tmp,
                            base: BaseReg::SimState,
                            offset: src_byte_off,
                            size: op_size,
                        });
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: dst_byte_off,
                            src: tmp,
                            size: op_size,
                        });
                    } else {
                        // Chunk-by-chunk copy (64 bits at a time)
                        let mut remaining = *width_bits;
                        let mut src_off = src_byte_off;
                        let mut dst_off = dst_byte_off;
                        while remaining > 0 {
                            let chunk_bits = remaining.min(64);
                            let chunk_size = ISelContext::op_size_for_width(chunk_bits);
                            let tmp = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: tmp,
                                base: BaseReg::SimState,
                                offset: src_off,
                                size: chunk_size,
                            });
                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: dst_off,
                                src: tmp,
                                size: chunk_size,
                            });
                            let advance = chunk_bits.div_ceil(8);
                            src_off += advance as i32;
                            dst_off += advance as i32;
                            remaining -= chunk_bits;
                        }
                    }
                }
                SIROffset::Dynamic(offset_reg) => {
                    // Dynamic offset commit: copy from src to dst region.
                    // Both use the same dynamic offset.
                    let offset_vreg = ctx.reg_map.get(*offset_reg);
                    let src_base_off = ctx.byte_offset(src_addr, 0);
                    let dst_base_off = ctx.byte_offset(dst_addr, 0);

                    let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: byte_off,
                        src: offset_vreg,
                        imm: 3,
                    });

                    // For simplicity, copy the containing bytes chunk-by-chunk.
                    // The physical width covers width_bits + up to 7 bit shift.
                    let phys_bytes = (*width_bits).div_ceil(8);
                    let mut copied = 0usize;
                    while copied < phys_bytes {
                        let remaining = phys_bytes - copied;
                        let chunk_size = if remaining >= 8 {
                            OpSize::S64
                        } else if remaining >= 4 {
                            OpSize::S32
                        } else if remaining >= 2 {
                            OpSize::S16
                        } else {
                            OpSize::S8
                        };
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::LoadIndexed {
                            dst: tmp,
                            base: BaseReg::SimState,
                            offset: src_base_off + copied as i32,
                            index: byte_off,
                            size: chunk_size,
                        });
                        block.push(MInst::StoreIndexed {
                            base: BaseReg::SimState,
                            offset: dst_base_off + copied as i32,
                            index: byte_off,
                            src: tmp,
                            size: chunk_size,
                        });
                        copied += chunk_size.bytes() as usize;
                    }
                }
            }

            // 4-state: also commit mask
            if ctx.is_4state_var(src_addr) && ctx.is_4state_var(dst_addr) {
                match offset {
                    SIROffset::Static(bit_off) => {
                        let src_mask_off = ctx.mask_byte_offset(src_addr, *bit_off);
                        let dst_mask_off = ctx.mask_byte_offset(dst_addr, *bit_off);
                        if *width_bits <= 64 {
                            let op_size = ISelContext::op_size_for_width(*width_bits);
                            let tmp = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: tmp,
                                base: BaseReg::SimState,
                                offset: src_mask_off,
                                size: op_size,
                            });
                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: dst_mask_off,
                                src: tmp,
                                size: op_size,
                            });
                        } else {
                            let mut remaining = *width_bits;
                            let mut s_off = src_mask_off;
                            let mut d_off = dst_mask_off;
                            while remaining > 0 {
                                let cb = remaining.min(64);
                                let cs = ISelContext::op_size_for_width(cb);
                                let tmp = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Load {
                                    dst: tmp,
                                    base: BaseReg::SimState,
                                    offset: s_off,
                                    size: cs,
                                });
                                block.push(MInst::Store {
                                    base: BaseReg::SimState,
                                    offset: d_off,
                                    src: tmp,
                                    size: cs,
                                });
                                let adv = cb.div_ceil(8);
                                s_off += adv as i32;
                                d_off += adv as i32;
                                remaining -= cb;
                            }
                        }
                    }
                    SIROffset::Dynamic(offset_reg) => {
                        let offset_vreg = ctx.reg_map.get(*offset_reg);
                        let src_mask_base = ctx.mask_byte_offset(src_addr, 0);
                        let dst_mask_base = ctx.mask_byte_offset(dst_addr, 0);
                        let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: byte_off,
                            src: offset_vreg,
                            imm: 3,
                        });
                        let phys_bytes = (*width_bits).div_ceil(8);
                        let mut copied = 0usize;
                        while copied < phys_bytes {
                            let remaining = phys_bytes - copied;
                            let cs = if remaining >= 8 {
                                OpSize::S64
                            } else if remaining >= 4 {
                                OpSize::S32
                            } else if remaining >= 2 {
                                OpSize::S16
                            } else {
                                OpSize::S8
                            };
                            let tmp = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::LoadIndexed {
                                dst: tmp,
                                base: BaseReg::SimState,
                                offset: src_mask_base + copied as i32,
                                index: byte_off,
                                size: cs,
                            });
                            block.push(MInst::StoreIndexed {
                                base: BaseReg::SimState,
                                offset: dst_mask_base + copied as i32,
                                index: byte_off,
                                src: tmp,
                                size: cs,
                            });
                            copied += cs.bytes() as usize;
                        }
                    }
                }
            }
        }

        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let d_width = ctx.sir_width(dst);
            let lhs_width = ctx.sir_width(lhs);

            // Wide (>64-bit) binary operations: dispatch to multi-word handler.
            // For comparisons/logic, the result may be narrow (1 bit) but the
            // operands can be wide — dispatch based on operand width too.
            if d_width > 64 || lhs_width > 64 {
                lower_wide_binary(ctx, block, *dst, *lhs, op, *rhs);
                if ctx.four_state {
                    lower_wide_binary_mask(ctx, block, *dst, *lhs, op, *rhs, d_width);
                }
                return;
            }
            // Also check if operands have wide chunks (may have been loaded wide
            // even if sir_width reports ≤64 due to optimizer width changes).
            if ctx.wide_regs.contains_key(lhs) || ctx.wide_regs.contains_key(rhs) {
                lower_wide_binary(ctx, block, *dst, *lhs, op, *rhs);
                if ctx.four_state {
                    lower_wide_binary_mask(ctx, block, *dst, *lhs, op, *rhs, d_width);
                }
                return;
            }

            // Constant folding: if both operands are known constants, compute result
            let lhs_const = ctx.consts.get(lhs).copied();
            let rhs_const = ctx.consts.get(rhs).copied();
            if let (Some(lc), Some(rc)) = (lhs_const, rhs_const) {
                let result = match op {
                    BinaryOp::Add => Some(lc.wrapping_add(rc)),
                    BinaryOp::Sub => Some(lc.wrapping_sub(rc)),
                    BinaryOp::Mul => Some(lc.wrapping_mul(rc)),
                    BinaryOp::And => Some(lc & rc),
                    BinaryOp::Or => Some(lc | rc),
                    BinaryOp::Xor => Some(lc ^ rc),
                    BinaryOp::Shl => Some(lc.wrapping_shl(rc as u32)),
                    BinaryOp::Shr => Some(lc.wrapping_shr(rc as u32)),
                    _ => None,
                };
                if let Some(val) = result {
                    let mask = mask_for_width(d_width);
                    let val = val & mask;
                    let dst_vreg = ctx.reg_map.get(*dst);
                    ctx.spill_descs[dst_vreg.0 as usize] = SpillDesc::remat(val);
                    block.push(MInst::LoadImm {
                        dst: dst_vreg,
                        value: val,
                    });
                    ctx.consts.insert(*dst, val);
                    // 4-state: constants always have mask=0
                    if ctx.four_state {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        ctx.set_mask(*dst, z);
                    }
                    return;
                }
            }

            let dst_vreg = ctx.reg_map.get(*dst);
            let lhs_vreg = ctx.reg_map.get(*lhs);
            let rhs_vreg = ctx.reg_map.get(*rhs);

            match op {
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                    // 64-bit arithmetic may produce upper bits; mask to output width.
                    let raw = if d_width < 64 {
                        ctx.alloc_vreg(SpillDesc::transient())
                    } else {
                        dst_vreg
                    };
                    match op {
                        BinaryOp::Add => block.push(MInst::Add {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Sub => block.push(MInst::Sub {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Mul => block.push(MInst::Mul {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        _ => unreachable!(),
                    }
                    if d_width < 64 {
                        ctx.emit_and_imm(block, dst_vreg, raw, mask_for_width(d_width));
                    }
                }
                BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
                    // For bitwise ops, result width = max(lhs_bits, rhs_bits).
                    // If both inputs fit within d_width, AND mask is redundant.
                    let lhs_bits = ctx.known_bits.get(&lhs_vreg).copied().unwrap_or(64);
                    let rhs_bits = ctx.known_bits.get(&rhs_vreg).copied().unwrap_or(64);
                    let result_bits = lhs_bits.max(rhs_bits);
                    let needs_mask = d_width < 64 && result_bits > d_width;

                    let raw = if needs_mask {
                        ctx.alloc_vreg(SpillDesc::transient())
                    } else {
                        dst_vreg
                    };
                    match op {
                        BinaryOp::And => block.push(MInst::And {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Or => block.push(MInst::Or {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Xor => block.push(MInst::Xor {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        _ => unreachable!(),
                    }
                    if needs_mask {
                        ctx.emit_and_imm(block, dst_vreg, raw, mask_for_width(d_width));
                    }
                }
                BinaryOp::Shr => {
                    // Check for wide-to-narrow extraction: lhs is >64 bits, dst is ≤64 bits
                    let lhs_width = ctx.sir_width(lhs);
                    if lhs_width > 64 {
                        lower_wide_extract(ctx, block, *dst, *lhs, *rhs);
                        return;
                    }

                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        block.push(MInst::ShrImm {
                            dst: shifted,
                            src: lhs_vreg,
                            imm: shift_amt as u8,
                        });
                        // Track known bits: shr reduces width
                        let lhs_bits = ctx.known_bits.get(&lhs_vreg).copied().unwrap_or(64);
                        let shifted_bits = lhs_bits.saturating_sub(shift_amt as usize);
                        ctx.known_bits.insert(shifted, shifted_bits);
                    } else {
                        let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Mov {
                            dst: rhs_copy,
                            src: rhs_vreg,
                        });
                        block.push(MInst::Shr {
                            dst: shifted,
                            lhs: lhs_vreg,
                            rhs: rhs_copy,
                        });
                    }
                    // Mask to destination width
                    if d_width < 64 {
                        let mask = mask_for_width(d_width);
                        ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                    } else {
                        block.push(MInst::Mov {
                            dst: dst_vreg,
                            src: shifted,
                        });
                    }
                }
                BinaryOp::Shl => {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        block.push(MInst::ShlImm {
                            dst: shifted,
                            src: lhs_vreg,
                            imm: shift_amt as u8,
                        });
                    } else {
                        let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Mov {
                            dst: rhs_copy,
                            src: rhs_vreg,
                        });
                        block.push(MInst::Shl {
                            dst: shifted,
                            lhs: lhs_vreg,
                            rhs: rhs_copy,
                        });
                    }
                    if d_width < 64 {
                        let mask = mask_for_width(d_width);
                        ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                    } else {
                        block.push(MInst::Mov {
                            dst: dst_vreg,
                            src: shifted,
                        });
                    }
                }
                BinaryOp::Sar => {
                    // Arithmetic shift right: sign-extend lhs to 64 bits, shift, mask result.
                    let width = ctx.sir_width(lhs);
                    if width < 64 {
                        let sext_shift = (64 - width) as u8;
                        let shifted_up = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShlImm {
                            dst: shifted_up,
                            src: lhs_vreg,
                            imm: sext_shift,
                        });
                        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::SarImm {
                            dst: sign_extended,
                            src: shifted_up,
                            imm: sext_shift,
                        });
                        // Now do the actual shift
                        let sar_result = ctx.alloc_vreg(SpillDesc::transient());
                        if let Some(&shift_amt) = ctx.consts.get(rhs) {
                            block.push(MInst::SarImm {
                                dst: sar_result,
                                src: sign_extended,
                                imm: shift_amt as u8,
                            });
                        } else {
                            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Mov {
                                dst: rhs_copy,
                                src: rhs_vreg,
                            });
                            block.push(MInst::Sar {
                                dst: sar_result,
                                lhs: sign_extended,
                                rhs: rhs_copy,
                            });
                        }
                        // Mask to output width
                        let mask = mask_for_width(width);
                        ctx.emit_and_imm(block, dst_vreg, sar_result, mask);
                    } else {
                        if let Some(&shift_amt) = ctx.consts.get(rhs) {
                            block.push(MInst::SarImm {
                                dst: dst_vreg,
                                src: lhs_vreg,
                                imm: shift_amt as u8,
                            });
                        } else {
                            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Mov {
                                dst: rhs_copy,
                                src: rhs_vreg,
                            });
                            block.push(MInst::Sar {
                                dst: dst_vreg,
                                lhs: lhs_vreg,
                                rhs: rhs_copy,
                            });
                        }
                    }
                }
                BinaryOp::Eq => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::Eq,
                }),
                BinaryOp::Ne => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::Ne,
                }),
                BinaryOp::LtU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::LtU,
                }),
                BinaryOp::LtS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::LtS,
                    });
                }
                BinaryOp::LeU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::LeU,
                }),
                BinaryOp::LeS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::LeS,
                    });
                }
                BinaryOp::GtU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GtU,
                }),
                BinaryOp::GtS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::GtS,
                    });
                }
                BinaryOp::GeU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GeU,
                }),
                BinaryOp::GeS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::GeS,
                    });
                }
                BinaryOp::Div | BinaryOp::Rem => {
                    // div/rem with zero guard: dst = rhs == 0 ? 0 : lhs op rhs
                    // Skip guard when rhs is a known non-zero constant.
                    let effective_rhs = if ctx.consts.get(rhs).is_some_and(|&v| v != 0) {
                        rhs_vreg
                    } else {
                        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm {
                            dst: zero,
                            value: 0,
                        });
                        let one = ctx.alloc_vreg(SpillDesc::remat(1));
                        block.push(MInst::LoadImm { dst: one, value: 1 });
                        let is_zero = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: is_zero,
                            lhs: rhs_vreg,
                            rhs: zero,
                            kind: CmpKind::Eq,
                        });
                        let safe_rhs = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Select {
                            dst: safe_rhs,
                            cond: is_zero,
                            true_val: one,
                            false_val: rhs_vreg,
                        });
                        safe_rhs
                    };
                    if matches!(op, BinaryOp::Div) {
                        block.push(MInst::UDiv {
                            dst: dst_vreg,
                            lhs: lhs_vreg,
                            rhs: effective_rhs,
                        });
                    } else {
                        block.push(MInst::URem {
                            dst: dst_vreg,
                            lhs: lhs_vreg,
                            rhs: effective_rhs,
                        });
                    }
                }
                BinaryOp::LogicAnd => {
                    // dst = (lhs != 0) && (rhs != 0) ? 1 : 0
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    let l_bool = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: l_bool,
                        lhs: lhs_vreg,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    let r_bool = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: r_bool,
                        lhs: rhs_vreg,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    block.push(MInst::And {
                        dst: dst_vreg,
                        lhs: l_bool,
                        rhs: r_bool,
                    });
                }
                BinaryOp::LogicOr => {
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    let l_bool = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: l_bool,
                        lhs: lhs_vreg,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    let r_bool = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: r_bool,
                        lhs: rhs_vreg,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    block.push(MInst::Or {
                        dst: dst_vreg,
                        lhs: l_bool,
                        rhs: r_bool,
                    });
                }
                BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
                    if ctx.four_state {
                        // IEEE 1800 ==?/!=?: RHS X/Z bits are wildcards (don't care)
                        let l_m = ctx.get_mask(*lhs, block);
                        let r_m = ctx.get_mask(*rhs, block);

                        // compare_mask = ~r_m (non-wildcard positions)
                        let compare_mask = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: compare_mask,
                            src: r_m,
                        });

                        // Compare only at non-wildcard positions
                        let l_eff = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: l_eff,
                            lhs: lhs_vreg,
                            rhs: compare_mask,
                        });
                        let r_eff = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: r_eff,
                            lhs: rhs_vreg,
                            rhs: compare_mask,
                        });

                        let kind = if matches!(op, BinaryOp::EqWildcard) {
                            CmpKind::Eq
                        } else {
                            CmpKind::Ne
                        };
                        block.push(MInst::Cmp {
                            dst: dst_vreg,
                            lhs: l_eff,
                            rhs: r_eff,
                            kind,
                        });

                        // Mask: check for LHS X at non-wildcard positions
                        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm {
                            dst: zero,
                            value: 0,
                        });
                        let x_at_compared = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: x_at_compared,
                            lhs: l_m,
                            rhs: compare_mask,
                        });
                        let has_x = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: has_x,
                            lhs: x_at_compared,
                            rhs: zero,
                            kind: CmpKind::Ne,
                        });

                        // If definite mismatch at compared positions → mask=0
                        let l_xor_r = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Xor {
                            dst: l_xor_r,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        });
                        let l_definite = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: l_definite,
                            src: l_m,
                        });
                        let definite_compare = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: definite_compare,
                            lhs: compare_mask,
                            rhs: l_definite,
                        });
                        let mismatch = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: mismatch,
                            lhs: l_xor_r,
                            rhs: definite_compare,
                        });
                        let has_mismatch = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: has_mismatch,
                            lhs: mismatch,
                            rhs: zero,
                            kind: CmpKind::Ne,
                        });

                        // mask = has_mismatch ? 0 : (has_x ? 1 : 0)
                        let res_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Select {
                            dst: res_m,
                            cond: has_mismatch,
                            true_val: zero,
                            false_val: has_x,
                        });
                        ctx.set_mask(*dst, res_m);
                        // Skip the general 4-state mask computation below
                    } else {
                        // 2-state: wildcards are same as Eq/Ne
                        let kind = if matches!(op, BinaryOp::EqWildcard) {
                            CmpKind::Eq
                        } else {
                            CmpKind::Ne
                        };
                        block.push(MInst::Cmp {
                            dst: dst_vreg,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                            kind,
                        });
                    }
                }
            }

            // 4-state: compute result mask (skip for wildcards which handle it inline)
            if ctx.four_state && !matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
                let l_m = ctx.get_mask(*lhs, block);
                let r_m = ctx.get_mask(*rhs, block);
                let res_m =
                    lower_binary_mask(ctx, block, op, lhs_vreg, rhs_vreg, l_m, r_m, d_width);
                ctx.set_mask(*dst, res_m);

                // Normalize: X positions must have v=1 (X encoding = v:1, m:1)
                let old_v = ctx.reg_map.get(*dst);
                let normalized = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: normalized,
                    lhs: old_v,
                    rhs: res_m,
                });
                ctx.reg_map.set(*dst, normalized);
            }
        }

        SIRInstruction::Unary(dst, op, src) => {
            let d_width = ctx.sir_width(dst);
            let src_width = ctx.sir_width(src);
            if d_width > 64 || src_width > 64 || ctx.wide_regs.contains_key(src) {
                lower_wide_unary(ctx, block, *dst, op, *src);
                if ctx.four_state {
                    lower_wide_unary_mask(ctx, block, *dst, op, *src, d_width, src_width);
                }
                return;
            }
            let dst_vreg = ctx.reg_map.get(*dst);
            let src_vreg = ctx.reg_map.get(*src);

            match op {
                UnaryOp::Ident => {
                    block.push(MInst::Mov {
                        dst: dst_vreg,
                        src: src_vreg,
                    });
                }
                UnaryOp::Minus => {
                    block.push(MInst::Neg {
                        dst: dst_vreg,
                        src: src_vreg,
                    });
                }
                UnaryOp::BitNot => {
                    let width = ctx.sir_width(src);
                    if width < 64 {
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: tmp,
                            src: src_vreg,
                        });
                        ctx.emit_and_imm(block, dst_vreg, tmp, mask_for_width(width));
                    } else {
                        block.push(MInst::BitNot {
                            dst: dst_vreg,
                            src: src_vreg,
                        });
                    }
                }
                UnaryOp::LogicNot => {
                    // dst = (src == 0) ? 1 : 0
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: src_vreg,
                        rhs: zero,
                        kind: CmpKind::Eq,
                    });
                }
                UnaryOp::And => {
                    // Reduction AND: dst = (src == all_ones_mask) ? 1 : 0
                    let width = ctx.sir_width(src);
                    let mask = if width >= 64 {
                        u64::MAX
                    } else {
                        mask_for_width(width)
                    };
                    let mask_vreg = ctx.alloc_vreg(SpillDesc::remat(mask));
                    block.push(MInst::LoadImm {
                        dst: mask_vreg,
                        value: mask,
                    });
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: src_vreg,
                        rhs: mask_vreg,
                        kind: CmpKind::Eq,
                    });
                }
                UnaryOp::Or => {
                    // Reduction OR: dst = (src != 0) ? 1 : 0
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: src_vreg,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                }
                UnaryOp::Xor => {
                    // Reduction XOR: dst = popcount(src) & 1
                    // Uses x86 POPCNT (SSE4.2) for single-instruction parity.
                    let pc = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Popcnt {
                        dst: pc,
                        src: src_vreg,
                    });
                    ctx.emit_and_imm(block, dst_vreg, pc, 1);
                    ctx.known_bits.insert(dst_vreg, 1);
                }
            }

            // 4-state: compute result mask for unary ops
            if ctx.four_state {
                let s_m = ctx.get_mask(*src, block);
                let res_m = lower_unary_mask(ctx, block, op, src_vreg, s_m, d_width, src_width);
                ctx.set_mask(*dst, res_m);
            }
        }

        SIRInstruction::Concat(dst, args) => {
            // Concat: build a wide value from chunks.
            // For ≤64-bit result, shift and OR the pieces together.
            let dst_vreg = ctx.reg_map.get(*dst);
            let dst_width = ctx.sir_width(dst);

            if dst_width <= 64 {
                // args are [MSB, ..., LSB]
                // Build from LSB to MSB
                let mut accumulated: Option<VReg> = None;
                let mut shift_pos = 0usize;

                for arg in args.iter().rev() {
                    let arg_vreg = ctx.reg_map.get(*arg);
                    let arg_width = ctx.sir_width(arg);

                    match accumulated {
                        None => {
                            // First (LSB) element
                            accumulated = Some(arg_vreg);
                        }
                        Some(acc) => {
                            // Shift this arg and OR with accumulator
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: shifted,
                                src: arg_vreg,
                                imm: shift_pos as u8,
                            });
                            let merged = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: merged,
                                lhs: acc,
                                rhs: shifted,
                            });
                            accumulated = Some(merged);
                        }
                    }
                    shift_pos += arg_width;
                }

                if let Some(result) = accumulated {
                    if result != dst_vreg {
                        block.push(MInst::Mov {
                            dst: dst_vreg,
                            src: result,
                        });
                    }
                }

                // 4-state: concat masks the same way
                if ctx.four_state {
                    let mut m_acc: Option<VReg> = None;
                    let mut m_shift = 0usize;
                    for arg in args.iter().rev() {
                        let m = ctx.get_mask(*arg, block);
                        let aw = ctx.sir_width(arg);
                        match m_acc {
                            None => {
                                m_acc = Some(m);
                            }
                            Some(a) => {
                                let sh = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShlImm {
                                    dst: sh,
                                    src: m,
                                    imm: m_shift as u8,
                                });
                                let mg = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: mg,
                                    lhs: a,
                                    rhs: sh,
                                });
                                m_acc = Some(mg);
                            }
                        }
                        m_shift += aw;
                    }
                    if let Some(m_res) = m_acc {
                        ctx.set_mask(*dst, m_res);
                    }
                }
            } else {
                // Wide concat (>64 bits): record chunk vregs for use by Store.
                // args are [MSB, ..., LSB]. Collect bits in LSB-first order,
                // then repack into uniform 64-bit chunks so Slice can use
                // bit_offset / 64 for indexing.
                let total_width = args.iter().map(|a| ctx.sir_width(a)).sum::<usize>();
                let n_dst_chunks = ISelContext::num_chunks(total_width);

                // Collect a flat bit stream: list of (vreg, width) in LSB-first order
                let mut flat_bits: Vec<(VReg, usize)> = Vec::new();
                for arg in args.iter().rev() {
                    let arg_width = ctx.sir_width(arg);
                    if arg_width > 64 {
                        let arg_chunks = ctx.get_wide_chunks(arg, block);
                        for ch in arg_chunks {
                            flat_bits.push(ch);
                        }
                    } else {
                        let arg_vreg = ctx.reg_map.get(*arg);
                        flat_bits.push((arg_vreg, arg_width));
                    }
                }

                // Repack into 64-bit uniform chunks via shift+or
                let mut dst_chunks: Vec<(VReg, usize)> = Vec::new();
                let mut _bit_pos = 0usize; // current position in the flat stream
                let mut flat_idx = 0usize;
                let mut flat_consumed = 0usize; // bits consumed from flat_bits[flat_idx]

                for chunk_i in 0..n_dst_chunks {
                    let chunk_width = if chunk_i == n_dst_chunks - 1 {
                        let rem = total_width % 64;
                        if rem == 0 { 64 } else { rem }
                    } else {
                        64
                    };

                    let mut acc = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: acc, value: 0 });
                    let mut acc_pos = 0usize;

                    while acc_pos < chunk_width && flat_idx < flat_bits.len() {
                        let (fv, fw) = flat_bits[flat_idx];
                        let remaining_in_flat = fw - flat_consumed;
                        let need = chunk_width - acc_pos;
                        let take = remaining_in_flat.min(need);

                        // Extract `take` bits from fv starting at flat_consumed
                        let mut piece = fv;
                        if flat_consumed > 0 {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: shifted,
                                src: piece,
                                imm: flat_consumed as u8,
                            });
                            piece = shifted;
                        }
                        if take < 64 {
                            let masked = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_and_imm(block, masked, piece, mask_for_width(take));
                            piece = masked;
                        }

                        // Place into acc at acc_pos
                        if acc_pos > 0 {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: shifted,
                                src: piece,
                                imm: acc_pos as u8,
                            });
                            let merged = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: merged,
                                lhs: acc,
                                rhs: shifted,
                            });
                            acc = merged;
                        } else {
                            let merged = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: merged,
                                lhs: acc,
                                rhs: piece,
                            });
                            acc = merged;
                        }

                        acc_pos += take;
                        flat_consumed += take;
                        if flat_consumed >= fw {
                            flat_idx += 1;
                            flat_consumed = 0;
                        }
                    }

                    dst_chunks.push((acc, chunk_width));
                    _bit_pos += chunk_width;
                }

                ctx.wide_regs.insert(*dst, dst_chunks);

                // 4-state: repack mask chunks the same way
                if ctx.four_state {
                    let mut mask_flat: Vec<(VReg, usize)> = Vec::new();
                    for arg in args.iter().rev() {
                        let arg_width = ctx.sir_width(arg);
                        if arg_width > 64 {
                            let mc = get_wide_mask_chunks(
                                ctx,
                                block,
                                arg,
                                ISelContext::num_chunks(arg_width),
                            );
                            for (i, mv) in mc.into_iter().enumerate() {
                                let cw = if i == ISelContext::num_chunks(arg_width) - 1 {
                                    let r = arg_width % 64;
                                    if r == 0 { 64 } else { r }
                                } else {
                                    64
                                };
                                mask_flat.push((mv, cw));
                            }
                        } else {
                            let m = ctx.get_mask(*arg, block);
                            mask_flat.push((m, arg_width));
                        }
                    }

                    // Repack masks into uniform 64-bit chunks (same algorithm as values)
                    let mut dst_m_chunks: Vec<(VReg, usize)> = Vec::new();
                    let mut mf_idx = 0usize;
                    let mut mf_consumed = 0usize;
                    for chunk_i in 0..n_dst_chunks {
                        let chunk_width = if chunk_i == n_dst_chunks - 1 {
                            let rem = total_width % 64;
                            if rem == 0 { 64 } else { rem }
                        } else {
                            64
                        };
                        let mut acc = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: acc, value: 0 });
                        let mut ap = 0usize;
                        while ap < chunk_width && mf_idx < mask_flat.len() {
                            let (fv, fw) = mask_flat[mf_idx];
                            let rem = fw - mf_consumed;
                            let take = rem.min(chunk_width - ap);
                            let mut piece = fv;
                            if mf_consumed > 0 {
                                let s = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm {
                                    dst: s,
                                    src: piece,
                                    imm: mf_consumed as u8,
                                });
                                piece = s;
                            }
                            if take < 64 {
                                let m = ctx.alloc_vreg(SpillDesc::transient());
                                ctx.emit_and_imm(block, m, piece, mask_for_width(take));
                                piece = m;
                            }
                            if ap > 0 {
                                let s = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShlImm {
                                    dst: s,
                                    src: piece,
                                    imm: ap as u8,
                                });
                                let mg = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: mg,
                                    lhs: acc,
                                    rhs: s,
                                });
                                acc = mg;
                            } else {
                                let mg = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: mg,
                                    lhs: acc,
                                    rhs: piece,
                                });
                                acc = mg;
                            }
                            ap += take;
                            mf_consumed += take;
                            if mf_consumed >= fw {
                                mf_idx += 1;
                                mf_consumed = 0;
                            }
                        }
                        dst_m_chunks.push((acc, chunk_width));
                    }
                    ctx.set_mask(*dst, dst_m_chunks[0].0);
                    ctx.wide_masks.insert(*dst, dst_m_chunks);
                }
            }
        }

        SIRInstruction::Slice(dst, src, bit_offset, width) => {
            let dst_vreg = ctx.reg_map.get(*dst);
            let src_width = ctx.sir_width(src);

            // If src has a known sim-state address (from a preceding Load/Store)
            // and no wide_regs entry, load directly from memory. This handles
            // the case where partial Stores updated memory but not VRegs.
            if *width <= 64 && !ctx.wide_regs.contains_key(src) {
                if let Some(addr) = ctx.reg_addrs.get(src).cloned() {
                    let byte_off = ctx.byte_offset(&addr, *bit_offset);
                    let intra_byte = *bit_offset % 8;
                    if intra_byte == 0 && OpSize::from_bits(*width).is_some() {
                        block.push(MInst::Load {
                            dst: dst_vreg,
                            base: BaseReg::SimState,
                            offset: byte_off,
                            size: OpSize::from_bits(*width).unwrap(),
                        });
                    } else {
                        let load_width = *width + intra_byte;
                        let load_size = ISelContext::op_size_for_width(load_width);
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: tmp,
                            base: BaseReg::SimState,
                            offset: byte_off,
                            size: load_size,
                        });
                        if intra_byte > 0 {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: shifted,
                                src: tmp,
                                imm: intra_byte as u8,
                            });
                            let mask = mask_for_width(*width);
                            ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                        } else {
                            let mask = mask_for_width(*width);
                            ctx.emit_and_imm(block, dst_vreg, tmp, mask);
                        }
                    }
                    return;
                }
            }

            if *width <= 64 && src_width <= 64 {
                let src_vreg = ctx.reg_map.get(*src);
                if *bit_offset == 0 && *width == src_width {
                    block.push(MInst::Mov {
                        dst: dst_vreg,
                        src: src_vreg,
                    });
                } else if *bit_offset == 0 {
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, src_vreg, mask);
                } else {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: src_vreg,
                        imm: *bit_offset as u8,
                    });
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                }
            } else if *width <= 64 {
                // Narrow slice from wide source
                let src_chunks = ctx.get_wide_chunks(src, block);
                let chunk_idx = *bit_offset / 64;
                let intra_bit = *bit_offset % 64;
                let main = ctx.wide_chunk_or_zero(&src_chunks, chunk_idx, block);

                if intra_bit == 0 {
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, main, mask);
                } else if intra_bit + *width <= 64 {
                    // Fits in one chunk after shift
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: main,
                        imm: intra_bit as u8,
                    });
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                } else {
                    // Crosses chunk boundary
                    let lo = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: lo,
                        src: main,
                        imm: intra_bit as u8,
                    });
                    let upper = ctx.wide_chunk_or_zero(&src_chunks, chunk_idx + 1, block);
                    let hi = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShlImm {
                        dst: hi,
                        src: upper,
                        imm: (64 - intra_bit) as u8,
                    });
                    let combined = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: combined,
                        lhs: lo,
                        rhs: hi,
                    });
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, combined, mask);
                }
            } else {
                // Wide slice: extract bits from a wide source.
                // Get source chunks, then extract the requested range.
                let src_chunks = ctx.get_wide_chunks(src, block);
                let dst_n_chunks = ISelContext::num_chunks(*width);
                let chunk_start = *bit_offset / 64;
                let intra_bit = *bit_offset % 64;

                let mut dst_chunks = Vec::with_capacity(dst_n_chunks);
                for i in 0..dst_n_chunks {
                    let src_idx = chunk_start + i;
                    let main = ctx.wide_chunk_or_zero(&src_chunks, src_idx, block);

                    if intra_bit == 0 {
                        dst_chunks.push((main, 64));
                    } else {
                        // Cross-chunk: combine bits from src[src_idx] and src[src_idx+1]
                        let lo = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: lo,
                            src: main,
                            imm: intra_bit as u8,
                        });
                        let upper = ctx.wide_chunk_or_zero(&src_chunks, src_idx + 1, block);
                        let hi = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShlImm {
                            dst: hi,
                            src: upper,
                            imm: (64 - intra_bit) as u8,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: lo,
                            rhs: hi,
                        });
                        dst_chunks.push((combined, 64));
                    }
                }

                // Mask the top chunk to the exact width
                let top_bits = *width % 64;
                if top_bits != 0 && !dst_chunks.is_empty() {
                    let last_idx = dst_chunks.len() - 1;
                    let (last_vreg, _) = dst_chunks[last_idx];
                    let masked = ctx.alloc_vreg(SpillDesc::transient());
                    ctx.emit_and_imm(block, masked, last_vreg, mask_for_width(top_bits));
                    dst_chunks[last_idx] = (masked, top_bits);
                }

                ctx.set_wide_chunks(*dst, dst_chunks);
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Wide (>64-bit) operation lowering via multi-word chunks
// ────────────────────────────────────────────────────────────────

/// Lower a binary operation on wide (>64-bit) values.
/// Supports: And, Or, Xor (chunk-wise) and Shl (multi-word shift).
fn lower_wide_binary(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
) {
    let d_width = ctx.sir_width(&dst);
    let lhs_width = ctx.sir_width(&lhs);
    // For comparisons and logic ops, the result may be narrow (1 bit)
    // but we need to process all chunks of the wider operand.
    let n_chunks = ISelContext::num_chunks(d_width.max(lhs_width));

    match op {
        // Chunk-wise operations: apply to each 64-bit chunk independently
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);

            for i in 0..n_chunks {
                let l = lhs_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let r = rhs_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let d = ctx.alloc_vreg(SpillDesc::transient());
                match op {
                    BinaryOp::And => block.push(MInst::And {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    }),
                    BinaryOp::Or => block.push(MInst::Or {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    }),
                    BinaryOp::Xor => block.push(MInst::Xor {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    }),
                    _ => unreachable!(),
                }
                dst_chunks.push((d, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide left shift by a scalar amount.
        // If the shift amount is a known constant (common after loop unrolling),
        // compute chunk assignments directly without runtime select chains.
        BinaryOp::Shl => {
            // (debug removed)
            let src_chunks = ctx.get_wide_chunks(&lhs, block);
            let n_src = src_chunks.len();

            if let Some(&amount) = ctx.consts.get(&rhs) {
                // Constant shift: compute each chunk statically
                let cs = (amount / 64) as usize; // chunk shift
                let is = (amount % 64) as u8; // intra-chunk shift

                let mut dst_chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    if i < cs {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        dst_chunks.push((z, 64));
                    } else {
                        let src_idx = i - cs;
                        let main_vreg = if src_idx < n_src {
                            src_chunks[src_idx].0
                        } else {
                            let z = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm { dst: z, value: 0 });
                            z
                        };

                        if is == 0 {
                            dst_chunks.push((main_vreg, 64));
                        } else {
                            // main_part = src[src_idx] << is
                            let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: main_shifted,
                                src: main_vreg,
                                imm: is,
                            });

                            // carry from lower chunk: src[src_idx-1] >> (64 - is)
                            if src_idx > 0 && (src_idx - 1) < n_src {
                                let carry_vreg = src_chunks[src_idx - 1].0;
                                let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm {
                                    dst: carry_shifted,
                                    src: carry_vreg,
                                    imm: 64 - is,
                                });
                                let combined = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: combined,
                                    lhs: main_shifted,
                                    rhs: carry_shifted,
                                });
                                dst_chunks.push((combined, 64));
                            } else {
                                dst_chunks.push((main_shifted, 64));
                            }
                        }
                    }
                }
                ctx.set_wide_chunks(dst, dst_chunks);
            } else {
                // Runtime left shift: select chain + carry propagation.
                lower_wide_runtime_shift(
                    ctx,
                    block,
                    dst,
                    &lhs,
                    &rhs,
                    n_chunks,
                    ShiftDir::Left,
                    false,
                );
            }
        }

        // Wide addition with carry chain
        BinaryOp::Add => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            let mut carry: Option<VReg> = None;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);

                if let Some(cin) = carry {
                    // s1 = l + r
                    let s1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s1,
                        lhs: l,
                        rhs: r,
                    });
                    // c1 = (s1 < l) unsigned
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c1,
                        lhs: s1,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    // s2 = s1 + cin
                    let s2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s2,
                        lhs: s1,
                        rhs: cin,
                    });
                    // c2 = (s2 < s1) unsigned
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c2,
                        lhs: s2,
                        rhs: s1,
                        kind: CmpKind::LtU,
                    });
                    // carry = c1 | c2
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: cout,
                        lhs: c1,
                        rhs: c2,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s2, 64));
                } else {
                    // s = l + r
                    let s = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s,
                        lhs: l,
                        rhs: r,
                    });
                    // carry = (s < l) unsigned
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: cout,
                        lhs: s,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide subtraction with borrow chain
        BinaryOp::Sub => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            let mut borrow: Option<VReg> = None;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);

                if let Some(bin) = borrow {
                    // d1 = l - r
                    let d1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub {
                        dst: d1,
                        lhs: l,
                        rhs: r,
                    });
                    // b1 = (r > l) unsigned
                    let b1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: b1,
                        lhs: r,
                        rhs: l,
                        kind: CmpKind::GtU,
                    });
                    // d2 = d1 - bin
                    let d2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub {
                        dst: d2,
                        lhs: d1,
                        rhs: bin,
                    });
                    // b2 = (bin > d1) unsigned
                    let b2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: b2,
                        lhs: bin,
                        rhs: d1,
                        kind: CmpKind::GtU,
                    });
                    // borrow = b1 | b2
                    let bout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: bout,
                        lhs: b1,
                        rhs: b2,
                    });
                    borrow = Some(bout);
                    dst_chunks.push((d2, 64));
                } else {
                    // d = l - r
                    let d = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    });
                    // borrow = (r > l) unsigned
                    let bout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: bout,
                        lhs: r,
                        rhs: l,
                        kind: CmpKind::GtU,
                    });
                    borrow = Some(bout);
                    dst_chunks.push((d, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide equality/inequality: chunk-wise AND/OR of per-chunk comparisons
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            let is_eq = matches!(op, BinaryOp::Eq | BinaryOp::EqWildcard);
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            let init = ctx.alloc_vreg(SpillDesc::remat(if is_eq { 1 } else { 0 }));
            block.push(MInst::LoadImm {
                dst: init,
                value: if is_eq { 1 } else { 0 },
            });
            let mut cond = init;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: eq,
                    lhs: l,
                    rhs: r,
                    kind: CmpKind::Eq,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                if is_eq {
                    block.push(MInst::And {
                        dst: next,
                        lhs: cond,
                        rhs: eq,
                    });
                } else {
                    // ne: accumulate OR of (chunk != chunk)
                    let neq = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: neq,
                        lhs: l,
                        rhs: r,
                        kind: CmpKind::Ne,
                    });
                    block.push(MInst::Or {
                        dst: next,
                        lhs: cond,
                        rhs: neq,
                    });
                }
                cond = next;
            }
            // Result is a 1-bit value in chunk 0, rest zero
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((cond, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide unsigned comparisons: compare from MSB chunk down
        BinaryOp::LtU | BinaryOp::LeU | BinaryOp::GtU | BinaryOp::GeU => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            // Init: Le/Ge → 1 (true when equal), Lt/Gt → 0 (false when equal)
            let init_val = if matches!(op, BinaryOp::LeU | BinaryOp::GeU) {
                1u64
            } else {
                0u64
            };
            let init = ctx.alloc_vreg(SpillDesc::remat(init_val));
            block.push(MInst::LoadImm {
                dst: init,
                value: init_val,
            });
            let mut res = init;

            let cmp_kind = match op {
                BinaryOp::LtU | BinaryOp::LeU => CmpKind::LtU,
                BinaryOp::GtU | BinaryOp::GeU => CmpKind::GtU,
                _ => unreachable!(),
            };

            // Process from LSB to MSB; each chunk: if equal keep previous, else use this chunk's cmp
            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: eq,
                    lhs: l,
                    rhs: r,
                    kind: CmpKind::Eq,
                });
                let cmp = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: cmp,
                    lhs: l,
                    rhs: r,
                    kind: cmp_kind,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: eq,
                    true_val: res,
                    false_val: cmp,
                });
                res = next;
            }

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((res, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide signed comparisons: compare MSB chunk signed, lower chunks unsigned
        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            let init_val = if matches!(op, BinaryOp::LeS | BinaryOp::GeS) {
                1u64
            } else {
                0u64
            };
            let init = ctx.alloc_vreg(SpillDesc::remat(init_val));
            block.push(MInst::LoadImm {
                dst: init,
                value: init_val,
            });
            let mut res = init;

            let unsigned_kind = match op {
                BinaryOp::LtS | BinaryOp::LeS => CmpKind::LtU,
                BinaryOp::GtS | BinaryOp::GeS => CmpKind::GtU,
                _ => unreachable!(),
            };
            let signed_kind = match op {
                BinaryOp::LtS | BinaryOp::LeS => CmpKind::LtS,
                BinaryOp::GtS | BinaryOp::GeS => CmpKind::GtS,
                _ => unreachable!(),
            };

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: eq,
                    lhs: l,
                    rhs: r,
                    kind: CmpKind::Eq,
                });
                // MSB chunk uses signed comparison, lower chunks use unsigned
                let kind = if i == n_chunks - 1 {
                    signed_kind
                } else {
                    unsigned_kind
                };
                let cmp = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: cmp,
                    lhs: l,
                    rhs: r,
                    kind,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: eq,
                    true_val: res,
                    false_val: cmp,
                });
                res = next;
            }

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((res, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide right shifts (logical and arithmetic)
        BinaryOp::Shr | BinaryOp::Sar => {
            let is_sar = matches!(op, BinaryOp::Sar);
            let src_chunks = ctx.get_wide_chunks(&lhs, block);
            let n_src = src_chunks.len();

            if let Some(&amount) = ctx.consts.get(&rhs) {
                // Constant shift
                let cs = (amount / 64) as usize; // chunk shift
                let is = (amount % 64) as u8; // intra-chunk shift

                let mut dst_chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    let src_idx = i + cs;
                    let main_vreg = if src_idx < n_src {
                        src_chunks[src_idx].0
                    } else if is_sar {
                        // SAR: fill with sign extension from MSB chunk
                        let msb = src_chunks[n_src - 1].0;
                        let sign = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::SarImm {
                            dst: sign,
                            src: msb,
                            imm: 63,
                        });
                        sign
                    } else {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        z
                    };

                    if is == 0 {
                        dst_chunks.push((main_vreg, 64));
                    } else {
                        // main_part = src[src_idx] >> is  (logical for SHR, logical here too — sign handled by carry)
                        let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
                        if is_sar && i == n_chunks - 1 {
                            // MSB chunk of SAR: arithmetic shift
                            block.push(MInst::SarImm {
                                dst: main_shifted,
                                src: main_vreg,
                                imm: is,
                            });
                        } else {
                            block.push(MInst::ShrImm {
                                dst: main_shifted,
                                src: main_vreg,
                                imm: is,
                            });
                        }

                        // carry from upper chunk: src[src_idx+1] << (64 - is)
                        let upper_idx = src_idx + 1;
                        if upper_idx < n_src {
                            let carry_vreg = src_chunks[upper_idx].0;
                            let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: carry_shifted,
                                src: carry_vreg,
                                imm: 64 - is,
                            });
                            let combined = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: combined,
                                lhs: main_shifted,
                                rhs: carry_shifted,
                            });
                            dst_chunks.push((combined, 64));
                        } else if is_sar && i < n_chunks - 1 {
                            // SAR: carry from sign-extended chunk
                            let msb = src_chunks[n_src - 1].0;
                            let sign = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::SarImm {
                                dst: sign,
                                src: msb,
                                imm: 63,
                            });
                            let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: carry_shifted,
                                src: sign,
                                imm: 64 - is,
                            });
                            let combined = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: combined,
                                lhs: main_shifted,
                                rhs: carry_shifted,
                            });
                            dst_chunks.push((combined, 64));
                        } else {
                            dst_chunks.push((main_shifted, 64));
                        }
                    }
                }
                ctx.set_wide_chunks(dst, dst_chunks);
            } else {
                // Runtime right shift: select chain + carry propagation.
                let dir = if is_sar {
                    ShiftDir::ArithRight
                } else {
                    ShiftDir::Right
                };
                lower_wide_runtime_shift(ctx, block, dst, &lhs, &rhs, n_chunks, dir, is_sar);
            }
        }

        // Wide logical operations (result is 1-bit)
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            // Reduce lhs to bool: any chunk non-zero?
            let lhs_bool = wide_reduce_or(ctx, block, &lhs_chunks, n_chunks);
            // Reduce rhs to bool
            let rhs_bool = wide_reduce_or(ctx, block, &rhs_chunks, n_chunks);

            let result = ctx.alloc_vreg(SpillDesc::transient());
            match op {
                BinaryOp::LogicAnd => block.push(MInst::And {
                    dst: result,
                    lhs: lhs_bool,
                    rhs: rhs_bool,
                }),
                BinaryOp::LogicOr => block.push(MInst::Or {
                    dst: result,
                    lhs: lhs_bool,
                    rhs: rhs_bool,
                }),
                _ => unreachable!(),
            }

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide multiplication: schoolbook O(n²) using UMulHi for 64×64→128.
        BinaryOp::Mul => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            // Accumulator: n_chunks of VRegs initialized to 0
            let mut acc: Vec<VReg> = (0..n_chunks)
                .map(|_| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                })
                .collect();

            for i in 0..n_chunks {
                let a_i = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let mut carry = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: carry,
                    value: 0,
                });

                for j in 0..n_chunks {
                    let k = i + j;
                    if k >= n_chunks {
                        break;
                    }

                    let b_j = ctx.wide_chunk_or_zero(&rhs_chunks, j, block);

                    // lo = a_i * b_j, hi = umulhi(a_i, b_j)
                    let lo = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Mul {
                        dst: lo,
                        lhs: a_i,
                        rhs: b_j,
                    });
                    let hi = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::UMulHi {
                        dst: hi,
                        lhs: a_i,
                        rhs: b_j,
                    });

                    // sum1 = acc[k] + lo
                    let sum1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: sum1,
                        lhs: acc[k],
                        rhs: lo,
                    });
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c1,
                        lhs: sum1,
                        rhs: acc[k],
                        kind: CmpKind::LtU,
                    });

                    // sum2 = sum1 + carry
                    let sum2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: sum2,
                        lhs: sum1,
                        rhs: carry,
                    });
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c2,
                        lhs: sum2,
                        rhs: sum1,
                        kind: CmpKind::LtU,
                    });

                    acc[k] = sum2;

                    // carry = hi + c1 + c2
                    let carry1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: carry1,
                        lhs: hi,
                        rhs: c1,
                    });
                    let new_carry = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: new_carry,
                        lhs: carry1,
                        rhs: c2,
                    });
                    carry = new_carry;
                }
            }

            let dst_chunks: Vec<(VReg, usize)> = acc.into_iter().map(|v| (v, 64)).collect();
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide division/remainder: bit-by-bit restoring division.
        BinaryOp::Div | BinaryOp::Rem => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let total_bits = n_chunks * 64;

            let mut q_chunks: Vec<VReg> = (0..n_chunks)
                .map(|_| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                })
                .collect();
            let mut rem_chunks: Vec<VReg> = (0..n_chunks)
                .map(|_| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                })
                .collect();

            for bit in (0..total_bits).rev() {
                let chunk_idx = bit / 64;
                let bit_idx = bit % 64;

                // remainder <<= 1
                for c in (0..n_chunks).rev() {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShlImm {
                        dst: shifted,
                        src: rem_chunks[c],
                        imm: 1,
                    });
                    if c > 0 {
                        let carry_bit = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: carry_bit,
                            src: rem_chunks[c - 1],
                            imm: 63,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: shifted,
                            rhs: carry_bit,
                        });
                        rem_chunks[c] = combined;
                    } else {
                        rem_chunks[c] = shifted;
                    }
                }

                // remainder[0] |= (dividend[chunk_idx] >> bit_idx) & 1
                let dividend_chunk = ctx.wide_chunk_or_zero(&lhs_chunks, chunk_idx, block);
                let extracted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: extracted,
                    src: dividend_chunk,
                    imm: bit_idx as u8,
                });
                let one_bit = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, one_bit, extracted, 1);
                let new_rem0 = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: new_rem0,
                    lhs: rem_chunks[0],
                    rhs: one_bit,
                });
                rem_chunks[0] = new_rem0;

                // if remainder >= divisor (chunk-wise unsigned comparison)
                let init_ge = ctx.alloc_vreg(SpillDesc::remat(1));
                block.push(MInst::LoadImm {
                    dst: init_ge,
                    value: 1,
                });
                let mut ge = init_ge;
                for (c, &rc) in rem_chunks.iter().enumerate() {
                    let dc = ctx.wide_chunk_or_zero(&rhs_chunks, c, block);
                    let eq = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: eq,
                        lhs: rc,
                        rhs: dc,
                        kind: CmpKind::Eq,
                    });
                    let gt = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: gt,
                        lhs: rc,
                        rhs: dc,
                        kind: CmpKind::GeU,
                    });
                    let next_ge = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: next_ge,
                        cond: eq,
                        true_val: ge,
                        false_val: gt,
                    });
                    ge = next_ge;
                }

                // conditional: remainder -= divisor (wide sub with borrow)
                let mut borrow: Option<VReg> = None;
                for (c, rc) in rem_chunks.iter_mut().enumerate() {
                    let old_rc = *rc;
                    let dc = ctx.wide_chunk_or_zero(&rhs_chunks, c, block);

                    let (diff, bout) = if let Some(bin) = borrow {
                        let d1 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub {
                            dst: d1,
                            lhs: old_rc,
                            rhs: dc,
                        });
                        let b1 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: b1,
                            lhs: dc,
                            rhs: old_rc,
                            kind: CmpKind::GtU,
                        });
                        let d2 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub {
                            dst: d2,
                            lhs: d1,
                            rhs: bin,
                        });
                        let b2 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: b2,
                            lhs: bin,
                            rhs: d1,
                            kind: CmpKind::GtU,
                        });
                        let bout = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: bout,
                            lhs: b1,
                            rhs: b2,
                        });
                        (d2, bout)
                    } else {
                        let d = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub {
                            dst: d,
                            lhs: old_rc,
                            rhs: dc,
                        });
                        let bout = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: bout,
                            lhs: dc,
                            rhs: old_rc,
                            kind: CmpKind::GtU,
                        });
                        (d, bout)
                    };

                    // select: if ge then subtracted else original
                    let new_rc = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: new_rc,
                        cond: ge,
                        true_val: diff,
                        false_val: old_rc,
                    });
                    *rc = new_rc;
                    borrow = Some(bout);
                }

                // quotient[chunk_idx] |= ge ? (1 << bit_idx) : 0
                let bit_mask = ctx.alloc_vreg(SpillDesc::remat(1u64 << bit_idx));
                block.push(MInst::LoadImm {
                    dst: bit_mask,
                    value: 1u64 << bit_idx,
                });
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: masked,
                    cond: ge,
                    true_val: bit_mask,
                    false_val: zero,
                });
                let new_q = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: new_q,
                    lhs: q_chunks[chunk_idx],
                    rhs: masked,
                });
                q_chunks[chunk_idx] = new_q;
            }

            let result_chunks = if matches!(op, BinaryOp::Div) {
                q_chunks
            } else {
                rem_chunks
            };
            let dst_chunks: Vec<(VReg, usize)> =
                result_chunks.into_iter().map(|v| (v, 64)).collect();
            ctx.set_wide_chunks(dst, dst_chunks);
        }
    }

    // When the result is narrow (≤64 bits, e.g. comparison result),
    // sync chunk[0] back to the scalar reg_map so scalar Store paths
    // can read it.
    if d_width <= 64 {
        if let Some(chunks) = ctx.wide_regs.get(&dst) {
            let chunk0 = chunks[0].0;
            let scalar = ctx.reg_map.get(dst);
            if chunk0 != scalar {
                block.push(MInst::Mov {
                    dst: scalar,
                    src: chunk0,
                });
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ShiftDir {
    Left,
    Right,
    ArithRight,
}

/// Runtime multi-word shift via select chain + cross-chunk carry.
///
/// For each output chunk, a select chain picks the source chunk based on
/// `shift_amt >> 6` (word offset), then applies the intra-chunk bit shift
/// with carry from the adjacent chunk.
fn lower_wide_runtime_shift(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: &RegisterId,
    rhs: &RegisterId,
    n_chunks: usize,
    dir: ShiftDir,
    _is_sar: bool,
) {
    let src_chunks = ctx.get_wide_chunks(lhs, block);
    let n_src = src_chunks.len();
    let amount_vreg = ctx.reg_map.get(*rhs);

    let chunk_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: chunk_shift,
        src: amount_vreg,
        imm: 6,
    });
    let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, bit_shift, amount_vreg, 63);
    let sixty_four = ctx.alloc_vreg(SpillDesc::remat(64));
    block.push(MInst::LoadImm {
        dst: sixty_four,
        value: 64,
    });
    let inv_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: inv_bit_shift,
        lhs: sixty_four,
        rhs: bit_shift,
    });
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let has_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_bit_shift,
        lhs: bit_shift,
        rhs: zero,
        kind: CmpKind::Ne,
    });

    // Fill value: 0 for SHL/SHR, sign-extension for SAR
    let fill = if matches!(dir, ShiftDir::ArithRight) {
        let msb = src_chunks[n_src - 1].0;
        let sf = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm {
            dst: sf,
            src: msb,
            imm: 63,
        });
        sf
    } else {
        zero
    };

    let mut dst_chunks = Vec::with_capacity(n_chunks);
    for i in 0..n_chunks {
        // Select the "main" source chunk via word_offset.
        // For SHL: src_index = i - word_offset → select where j + word_offset == i
        // For SHR/SAR: src_index = i + word_offset → select where j == i + word_offset
        let main_chunk = {
            let mut val = fill;
            for j in (0..n_src).rev() {
                // Compute the effective index this source chunk maps to
                let j_vreg = ctx.alloc_vreg(SpillDesc::remat(j as u64));
                block.push(MInst::LoadImm {
                    dst: j_vreg,
                    value: j as u64,
                });
                let eff_idx = ctx.alloc_vreg(SpillDesc::transient());
                match dir {
                    ShiftDir::Left => {
                        // src[j] goes to dst[j + word_offset]
                        block.push(MInst::Add {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        // src[j + word_offset] goes to dst[j], i.e., src[j] goes to dst[j - word_offset]
                        // Check: j >= word_offset, then eff = j - word_offset
                        // Simpler: for dst[i], source is src[i + word_offset]
                        // So we select j if j == i + word_offset
                        block.push(MInst::Sub {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                }
                let i_vreg = ctx.alloc_vreg(SpillDesc::remat(i as u64));
                block.push(MInst::LoadImm {
                    dst: i_vreg,
                    value: i as u64,
                });
                let is_match = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: is_match,
                    lhs: eff_idx,
                    rhs: i_vreg,
                    kind: CmpKind::Eq,
                });
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected,
                    cond: is_match,
                    true_val: src_chunks[j].0,
                    false_val: val,
                });
                val = selected;
            }
            val
        };

        // Select the "carry" source chunk (adjacent in shift direction)
        let carry_chunk = {
            let mut val = fill;
            for j in (0..n_src).rev() {
                let j_vreg = ctx.alloc_vreg(SpillDesc::remat(j as u64));
                block.push(MInst::LoadImm {
                    dst: j_vreg,
                    value: j as u64,
                });
                let eff_idx = ctx.alloc_vreg(SpillDesc::transient());
                let carry_i = match dir {
                    ShiftDir::Left => {
                        // carry comes from chunk below: i-1
                        if i == 0 { usize::MAX } else { i - 1 }
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        // carry comes from chunk above: i+1
                        i + 1
                    }
                };
                match dir {
                    ShiftDir::Left => {
                        block.push(MInst::Add {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        block.push(MInst::Sub {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                }
                let ci_vreg = ctx.alloc_vreg(SpillDesc::remat(carry_i as u64));
                block.push(MInst::LoadImm {
                    dst: ci_vreg,
                    value: carry_i as u64,
                });
                let is_match = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: is_match,
                    lhs: eff_idx,
                    rhs: ci_vreg,
                    kind: CmpKind::Eq,
                });
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected,
                    cond: is_match,
                    true_val: src_chunks[j].0,
                    false_val: val,
                });
                val = selected;
            }
            val
        };

        // Apply intra-chunk shift: result = (main_chunk SHIFT bit_shift) | (carry_chunk INVSHIFT inv_bit_shift)
        // (debug removed)
        let bit_shift_copy = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mov {
            dst: bit_shift_copy,
            src: bit_shift,
        });
        let inv_copy = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mov {
            dst: inv_copy,
            src: inv_bit_shift,
        });

        let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
        let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());

        match dir {
            ShiftDir::Left => {
                block.push(MInst::Shl {
                    dst: main_shifted,
                    lhs: main_chunk,
                    rhs: bit_shift_copy,
                });
                block.push(MInst::Shr {
                    dst: carry_shifted,
                    lhs: carry_chunk,
                    rhs: inv_copy,
                });
            }
            ShiftDir::Right => {
                block.push(MInst::Shr {
                    dst: main_shifted,
                    lhs: main_chunk,
                    rhs: bit_shift_copy,
                });
                block.push(MInst::Shl {
                    dst: carry_shifted,
                    lhs: carry_chunk,
                    rhs: inv_copy,
                });
            }
            ShiftDir::ArithRight => {
                if i == n_chunks - 1 {
                    block.push(MInst::Sar {
                        dst: main_shifted,
                        lhs: main_chunk,
                        rhs: bit_shift_copy,
                    });
                } else {
                    block.push(MInst::Shr {
                        dst: main_shifted,
                        lhs: main_chunk,
                        rhs: bit_shift_copy,
                    });
                }
                block.push(MInst::Shl {
                    dst: carry_shifted,
                    lhs: carry_chunk,
                    rhs: inv_copy,
                });
            }
        }

        // Combine: if has_bit_shift then (main | carry) else main
        let combined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: combined,
            lhs: main_shifted,
            rhs: carry_shifted,
        });
        let result = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: result,
            cond: has_bit_shift,
            true_val: combined,
            false_val: main_chunk,
        });

        dst_chunks.push((result, 64));
    }
    ctx.set_wide_chunks(dst, dst_chunks);
}

/// Reduce a wide value to a boolean (any chunk non-zero → 1, else 0).
fn wide_reduce_or(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[(VReg, usize)],
    n_chunks: usize,
) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut acc = zero;
    for i in 0..n_chunks {
        let c = chunks.get(i).map(|c| c.0).unwrap_or(zero);
        let next = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next,
            lhs: acc,
            rhs: c,
        });
        acc = next;
    }
    // acc != 0 → 1
    let result = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: result,
        lhs: acc,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    result
}

/// Lower a unary operation on wide (>64-bit) values.
fn lower_wide_unary(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    op: &UnaryOp,
    src: RegisterId,
) {
    let d_width = ctx.sir_width(&dst);
    let src_width = ctx.sir_width(&src);
    let n_chunks = ISelContext::num_chunks(d_width.max(src_width));

    match op {
        UnaryOp::BitNot => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                let s = src_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let d = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::BitNot { dst: d, src: s });
                dst_chunks.push((d, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }
        UnaryOp::Ident => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            // Zero-pad to n_chunks if source has fewer chunks (narrow→wide cast)
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                if i < src_chunks.len() {
                    dst_chunks.push(src_chunks[i]);
                } else {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    dst_chunks.push((z, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }
        // Wide negation: two's complement = ~x + 1
        UnaryOp::Minus => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            // First invert all bits
            let mut inv_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                let s = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let d = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::BitNot { dst: d, src: s });
                inv_chunks.push((d, 64usize));
            }
            // Then add 1 (wide add with constant 1)
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            let mut carry: Option<VReg> = None;
            for (i, &(l, _)) in inv_chunks.iter().enumerate() {
                let r = if i == 0 {
                    let one = ctx.alloc_vreg(SpillDesc::remat(1));
                    block.push(MInst::LoadImm { dst: one, value: 1 });
                    one
                } else {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                };
                let s = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Add {
                    dst: s,
                    lhs: l,
                    rhs: r,
                });
                if let Some(cin) = carry {
                    let s2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s2,
                        lhs: s,
                        rhs: cin,
                    });
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c1,
                        lhs: s,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c2,
                        lhs: s2,
                        rhs: s,
                        kind: CmpKind::LtU,
                    });
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: cout,
                        lhs: c1,
                        rhs: c2,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s2, 64));
                } else {
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: cout,
                        lhs: s,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide logical not: result = (value == 0) ? 1 : 0
        UnaryOp::LogicNot => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let is_nonzero = wide_reduce_or(ctx, block, &src_chunks, n_chunks);
            // LogicNot: invert the boolean
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let result = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: result,
                lhs: is_nonzero,
                rhs: zero,
                kind: CmpKind::Eq,
            });

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide reduction OR: result = (any bit set?) → 1
        UnaryOp::Or => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let result = wide_reduce_or(ctx, block, &src_chunks, n_chunks);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide reduction AND: result = (all bits set?) → 1
        UnaryOp::And => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: u64::MAX,
            });
            let mut acc = all_ones;
            for i in 0..n_chunks {
                let c = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::And {
                    dst: next,
                    lhs: acc,
                    rhs: c,
                });
                acc = next;
            }
            let result = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: result,
                lhs: acc,
                rhs: all_ones,
                kind: CmpKind::Eq,
            });
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide reduction XOR: result = parity of all bits
        UnaryOp::Xor => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let mut acc = zero;
            for i in 0..n_chunks {
                let c = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Xor {
                    dst: next,
                    lhs: acc,
                    rhs: c,
                });
                acc = next;
            }
            // Now acc has XOR of all chunks. Need popcount parity (odd # of 1-bits → 1)
            // Fold 64-bit value to 1 bit by cascading XOR
            let mut val = acc;
            for shift in [32u8, 16, 8, 4, 2, 1] {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: val,
                    imm: shift,
                });
                let folded = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Xor {
                    dst: folded,
                    lhs: val,
                    rhs: shifted,
                });
                val = folded;
            }
            let result = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, result, val, 1);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }
    }

    // Sync narrow results to scalar reg_map
    if d_width <= 64 {
        if let Some(chunks) = ctx.wide_regs.get(&dst) {
            let chunk0 = chunks[0].0;
            let scalar = ctx.reg_map.get(dst);
            if chunk0 != scalar {
                block.push(MInst::Mov {
                    dst: scalar,
                    src: chunk0,
                });
            }
        }
    }
}

/// Extract a ≤64-bit value from a wide (>64-bit) register by right-shifting.
fn lower_wide_extract(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    wide_src: RegisterId,
    shift_amount: RegisterId,
) {
    let dst_vreg = ctx.reg_map.get(dst);
    let d_width = ctx.sir_width(&dst);
    let src_chunks = ctx.get_wide_chunks(&wide_src, block);
    let n_src = src_chunks.len();

    if let Some(&amount) = ctx.consts.get(&shift_amount) {
        // Constant extraction: directly pick the right chunk and shift
        let ci = (amount / 64) as usize;
        let is = (amount % 64) as u8;

        let main_vreg = if ci < n_src {
            src_chunks[ci].0
        } else {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        };

        if is == 0 {
            if d_width < 64 {
                let mask = mask_for_width(d_width);
                ctx.emit_and_imm(block, dst_vreg, main_vreg, mask);
            } else {
                block.push(MInst::Mov {
                    dst: dst_vreg,
                    src: main_vreg,
                });
            }
        } else {
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted,
                src: main_vreg,
                imm: is,
            });

            // Carry from next chunk
            if (ci + 1) < n_src {
                let next_vreg = src_chunks[ci + 1].0;
                let carry = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShlImm {
                    dst: carry,
                    src: next_vreg,
                    imm: 64 - is,
                });
                let combined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: combined,
                    lhs: shifted,
                    rhs: carry,
                });
                if d_width < 64 {
                    let mask = mask_for_width(d_width);
                    ctx.emit_and_imm(block, dst_vreg, combined, mask);
                } else {
                    block.push(MInst::Mov {
                        dst: dst_vreg,
                        src: combined,
                    });
                }
            } else if d_width < 64 {
                let mask = mask_for_width(d_width);
                ctx.emit_and_imm(block, dst_vreg, shifted, mask);
            } else {
                block.push(MInst::Mov {
                    dst: dst_vreg,
                    src: shifted,
                });
            }
        }
    } else {
        // Wide Shr with non-constant amount is handled by lower_wide_binary's
        // runtime shift path. This code is only reachable if the narrow Binary
        // handler's Shr detects lhs_width > 64, which is pre-empted by the wide
        // dispatch at the top of the Binary handler.
        unreachable!(
            "wide extract with non-constant shift: should be handled by lower_wide_binary"
        );
    }
}

/// Sign-extend a pair of operands for signed comparison.
/// For widths < 64, shifts left then arithmetic-shifts right to propagate the sign bit.
fn sign_extend_pair(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    lhs_sir: &RegisterId,
    rhs_sir: &RegisterId,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
) -> (VReg, VReg) {
    let lw = ctx.sir_width(lhs_sir);
    let rw = ctx.sir_width(rhs_sir);
    let width = lw.max(rw);

    if width >= 64 {
        return (lhs_vreg, rhs_vreg);
    }

    let shift = (64 - width) as u8;

    let sign_extend_with_imm = |ctx: &mut ISelContext, block: &mut MBlock, src: VReg| -> VReg {
        let shifted_up = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShlImm {
            dst: shifted_up,
            src,
            imm: shift,
        });
        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm {
            dst: sign_extended,
            src: shifted_up,
            imm: shift,
        });
        sign_extended
    };

    let sl = sign_extend_with_imm(ctx, block, lhs_vreg);
    let sr = sign_extend_with_imm(ctx, block, rhs_vreg);
    (sl, sr)
}

fn lower_terminator(ctx: &mut ISelContext, block: &mut MBlock, term: &SIRTerminator) {
    match term {
        SIRTerminator::Jump(target, _args) => {
            // Block args are handled via phi nodes (built in a second pass).
            block.push(MInst::Jump {
                target: BlockId(target.0 as u32),
            });
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let cond_vreg = ctx.reg_map.get(*cond);
            block.push(MInst::Branch {
                cond: cond_vreg,
                true_bb: BlockId(true_block.0.0 as u32),
                false_bb: BlockId(false_block.0.0 as u32),
            });
        }
        SIRTerminator::Return => {
            block.push(MInst::Return);
        }
        SIRTerminator::Error(code) => {
            block.push(MInst::ReturnError { code: *code });
        }
    }
}

// ────────────────────────────────────────────────────────────────
// 4-state mask computation
// ────────────────────────────────────────────────────────────────

/// Compute result mask for a binary operation.
///
/// Mask formulas (from IEEE 1800 / Cranelift backend):
/// - AND: m = (lm & rm) | (lm & rv) | (rm & lv)  ... but dominant-0 cancels X
///   Simplified: any X bit where the other operand's corresponding bit is not
///   definite-0 propagates as X. If the other bit is definite-0, AND = 0 regardless.
///   Formula: res_m = (lm | rm) & ~(~lv & ~lm) & ~(~rv & ~rm)
///   equivalently: res_m = (lm & rm) | (lm & rv) | (rm & lv)
/// - OR:  dual of AND — dominant-1 cancels X
///   res_m = (lm & rm) | (lm & ~rv) | (rm & ~lv)
/// - XOR: res_m = lm | rm
/// - Shift: if shift amount has X → all-X; else shift mask normally
/// - Arithmetic (Add/Sub/Mul/Div/Rem): conservative — any X → all-X
/// - Comparison: any X → result X (1-bit mask)
/// - LogicAnd: dominant-false (v|m==0) → mask=0; else if any X → mask=all-X
/// - LogicOr: dominant-true (v&~m!=0) → mask=0; else if any X → mask=all-X
fn lower_binary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    op: &BinaryOp,
    lv: VReg,
    rv: VReg,
    lm: VReg,
    rm: VReg,
    d_width: usize,
) -> VReg {
    match op {
        BinaryOp::And => {
            // res_m = (lm & rm) | (lm & rv) | (rm & lv)
            let t1 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t1,
                lhs: lm,
                rhs: rm,
            });
            let t2 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t2,
                lhs: lm,
                rhs: rv,
            });
            let t3 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t3,
                lhs: rm,
                rhs: lv,
            });
            let t4 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: t4,
                lhs: t1,
                rhs: t2,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: res,
                lhs: t4,
                rhs: t3,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, res, mask_for_width(d_width));
                masked
            } else {
                res
            }
        }
        BinaryOp::Or => {
            // res_m = (lm & rm) | (lm & ~rv) | (rm & ~lv)
            let t1 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t1,
                lhs: lm,
                rhs: rm,
            });
            let not_rv = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_rv,
                src: rv,
            });
            let t2 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t2,
                lhs: lm,
                rhs: not_rv,
            });
            let not_lv = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_lv,
                src: lv,
            });
            let t3 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t3,
                lhs: rm,
                rhs: not_lv,
            });
            let t4 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: t4,
                lhs: t1,
                rhs: t2,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: res,
                lhs: t4,
                rhs: t3,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, res, mask_for_width(d_width));
                masked
            } else {
                res
            }
        }
        BinaryOp::Xor => {
            // res_m = lm | rm
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: res,
                lhs: lm,
                rhs: rm,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, res, mask_for_width(d_width));
                masked
            } else {
                res
            }
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            // If shift amount has X → all-X; else shift mask by same amount
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: rm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            // Shift mask by value
            let shifted_m = ctx.alloc_vreg(SpillDesc::transient());
            match op {
                BinaryOp::Shl => block.push(MInst::Shl {
                    dst: shifted_m,
                    lhs: lm,
                    rhs: rv,
                }),
                BinaryOp::Shr => block.push(MInst::Shr {
                    dst: shifted_m,
                    lhs: lm,
                    rhs: rv,
                }),
                BinaryOp::Sar => block.push(MInst::Sar {
                    dst: shifted_m,
                    lhs: lm,
                    rhs: rv,
                }),
                _ => unreachable!(),
            }
            let all_x = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
            block.push(MInst::LoadImm {
                dst: all_x,
                value: u64::MAX,
            });
            let raw = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: raw,
                cond: has_x,
                true_val: all_x,
                false_val: shifted_m,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, raw, mask_for_width(d_width));
                masked
            } else {
                raw
            }
        }
        BinaryOp::LogicAnd => {
            // Dominant false: if either operand is definite false (v|m == 0) → mask=0
            // else if any X → mask = all-X
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let l_vm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: l_vm,
                lhs: lv,
                rhs: lm,
            });
            let r_vm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: r_vm,
                lhs: rv,
                rhs: rm,
            });
            let l_def_false = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_def_false,
                lhs: l_vm,
                rhs: zero,
                kind: CmpKind::Eq,
            });
            let r_def_false = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_def_false,
                lhs: r_vm,
                rhs: zero,
                kind: CmpKind::Eq,
            });
            let either_false = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: either_false,
                lhs: l_def_false,
                rhs: r_def_false,
            });
            // any X?
            let l_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_has_x,
                lhs: lm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let r_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_has_x,
                lhs: rm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let any_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: any_x,
                lhs: l_has_x,
                rhs: r_has_x,
            });
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: mask_for_width(d_width),
            });
            let conservative = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: conservative,
                cond: any_x,
                true_val: all_ones,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: either_false,
                true_val: zero,
                false_val: conservative,
            });
            res
        }
        BinaryOp::LogicOr => {
            // Dominant true: if either operand is definite true → mask=0
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let not_lm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_lm,
                src: lm,
            });
            let l_def_v = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: l_def_v,
                lhs: lv,
                rhs: not_lm,
            });
            let not_rm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_rm,
                src: rm,
            });
            let r_def_v = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: r_def_v,
                lhs: rv,
                rhs: not_rm,
            });
            let l_def_true = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_def_true,
                lhs: l_def_v,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let r_def_true = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_def_true,
                lhs: r_def_v,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let either_true = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: either_true,
                lhs: l_def_true,
                rhs: r_def_true,
            });
            let l_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_has_x,
                lhs: lm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let r_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_has_x,
                lhs: rm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let any_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: any_x,
                lhs: l_has_x,
                rhs: r_has_x,
            });
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: mask_for_width(d_width),
            });
            let conservative = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: conservative,
                cond: any_x,
                true_val: all_ones,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: either_true,
                true_val: zero,
                false_val: conservative,
            });
            res
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            // Wildcard mask is handled inline in the Binary handler;
            // this arm should never be reached.
            unreachable!("wildcard mask is computed inline, not via lower_binary_mask")
        }
        _ => {
            // Conservative: any X in either operand → all-X result
            // Covers: Add, Sub, Mul, Div, Rem, comparisons (Eq/Ne/Lt/Le/Gt/Ge)
            conservative_mask(ctx, block, lm, rm, d_width)
        }
    }
}

/// Compute result mask for a unary operation.
fn lower_unary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    op: &UnaryOp,
    src_v: VReg,
    src_m: VReg,
    d_width: usize,
    src_width: usize,
) -> VReg {
    match op {
        UnaryOp::Ident => {
            // Mask passes through but must be zero-extended for widening.
            // When src is narrower than dst, upper bits should be 0 (definite 0).
            let effective_width = src_width.min(d_width);
            if effective_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, src_m, mask_for_width(effective_width));
                masked
            } else {
                src_m
            }
        }
        UnaryOp::BitNot => {
            // ~X = X, mask passes through
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, src_m, mask_for_width(d_width));
                masked
            } else {
                src_m
            }
        }
        UnaryOp::Minus | UnaryOp::LogicNot => {
            // Conservative: any X → all-X
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: mask_for_width(d_width),
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: has_x,
                true_val: all_ones,
                false_val: zero,
            });
            res
        }
        UnaryOp::And => {
            // Reduction AND: dominant-0. If any definite 0 → result definite (mask=0).
            // definite_zeros = ~src_v & ~src_m (bits that are definitely 0)
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let not_v = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_v,
                src: src_v,
            });
            let not_m = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_m,
                src: src_m,
            });
            let def_zeros = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: def_zeros,
                lhs: not_v,
                rhs: not_m,
            });
            // Mask to src_width
            let def_zeros_masked = if src_width < 64 {
                let m = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, m, def_zeros, mask_for_width(src_width));
                m
            } else {
                def_zeros
            };
            let has_def_zero = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_def_zero,
                lhs: def_zeros_masked,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            // If definite 0 → mask=0; else if X → mask=1; else mask=0
            let x_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: x_mask,
                cond: has_x,
                true_val: has_x,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: has_def_zero,
                true_val: zero,
                false_val: x_mask,
            });
            res
        }
        UnaryOp::Or => {
            // Reduction OR: dominant-1. If any definite 1 → result definite (mask=0).
            // definite_ones = src_v & ~src_m (bits that are definitely 1)
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let not_m = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_m,
                src: src_m,
            });
            let def_ones = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: def_ones,
                lhs: src_v,
                rhs: not_m,
            });
            let has_def_one = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_def_one,
                lhs: def_ones,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let x_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: x_mask,
                cond: has_x,
                true_val: has_x,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: has_def_one,
                true_val: zero,
                false_val: x_mask,
            });
            res
        }
        UnaryOp::Xor => {
            // Reduction XOR: any X → result X
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            has_x
        }
    }
}

/// Conservative mask: if either operand has any X bits, result is all-X.
fn conservative_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    lm: VReg,
    rm: VReg,
    d_width: usize,
) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let l_has_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: l_has_x,
        lhs: lm,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let r_has_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: r_has_x,
        lhs: rm,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let any_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: any_x,
        lhs: l_has_x,
        rhs: r_has_x,
    });
    let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
    block.push(MInst::LoadImm {
        dst: all_ones,
        value: mask_for_width(d_width),
    });
    let res = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: res,
        cond: any_x,
        true_val: all_ones,
        false_val: zero,
    });
    res
}

// ────────────────────────────────────────────────────────────────
// Wide (>64-bit) 4-state mask computation
// ────────────────────────────────────────────────────────────────

/// Helper: get wide mask chunks for a register, or create zero chunks.
fn get_wide_mask_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    reg: &RegisterId,
    n_chunks: usize,
) -> Vec<VReg> {
    if let Some(mchunks) = ctx.wide_masks.get(reg).cloned() {
        let mut result: Vec<VReg> = mchunks.iter().map(|c| c.0).collect();
        while result.len() < n_chunks {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            result.push(z);
        }
        result
    } else {
        // Use scalar mask if available
        let scalar_m = ctx.mask_map.map.get(reg.0).copied().flatten();
        let mut result = Vec::with_capacity(n_chunks);
        if let Some(m) = scalar_m {
            result.push(m);
        } else {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            result.push(z);
        }
        for _ in 1..n_chunks {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            result.push(z);
        }
        result
    }
}

/// Check if any chunk has X bits (OR-reduce all mask chunks).
fn any_chunk_has_x(ctx: &mut ISelContext, block: &mut MBlock, mask_chunks: &[VReg]) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    // OR all mask chunks together
    let mut combined = mask_chunks[0];
    for &mc in &mask_chunks[1..] {
        let t = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: t,
            lhs: combined,
            rhs: mc,
        });
        combined = t;
    }
    let has_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_x,
        lhs: combined,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    has_x
}

/// Compute wide mask for binary operations.
fn lower_wide_binary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
    d_width: usize,
) {
    let n_chunks = ISelContext::num_chunks(d_width.max(ctx.sir_width(&lhs)));
    let lm_chunks = get_wide_mask_chunks(ctx, block, &lhs, n_chunks);
    let rm_chunks = get_wide_mask_chunks(ctx, block, &rhs, n_chunks);

    match op {
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            // Per-chunk mask computation
            let lv_chunks = ctx.get_wide_chunks(&lhs, block);
            let rv_chunks = ctx.get_wide_chunks(&rhs, block);
            let n_dst = ISelContext::num_chunks(d_width);
            let mut dst_m_chunks = Vec::with_capacity(n_dst);

            for i in 0..n_dst {
                let lm = lm_chunks.get(i).copied().unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let rm = rm_chunks.get(i).copied().unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let lv = ctx.wide_chunk_or_zero(&lv_chunks, i, block);
                let rv = ctx.wide_chunk_or_zero(&rv_chunks, i, block);
                let chunk_w = if i == n_dst - 1 {
                    let r = d_width % 64;
                    if r == 0 { 64 } else { r }
                } else {
                    64
                };
                let m = lower_binary_mask(ctx, block, op, lv, rv, lm, rm, chunk_w);
                dst_m_chunks.push((m, 64));
            }
            // Set scalar mask from chunk[0]
            ctx.set_mask(dst, dst_m_chunks[0].0);
            ctx.wide_masks.insert(dst, dst_m_chunks);
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            // If shift amount has X → all-X. Otherwise, shift mask same way as value.
            // Check shift amount mask (rhs is scalar, so rm_chunks[0] is the mask)
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let shift_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: shift_has_x,
                lhs: rm_chunks[0],
                rhs: zero,
                kind: CmpKind::Ne,
            });

            let n_dst = ISelContext::num_chunks(d_width);
            // Get the result value chunks (already computed by lower_wide_binary)
            // The mask should follow the same pattern as the value.
            // For constant shifts, shift mask chunks directly.
            if let Some(&amount) = ctx.consts.get(&rhs) {
                let cs = (amount / 64) as usize;
                let is = (amount % 64) as u8;
                let mut dst_m_chunks = Vec::with_capacity(n_dst);

                match op {
                    BinaryOp::Shl => {
                        for i in 0..n_dst {
                            if i < cs {
                                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                block.push(MInst::LoadImm { dst: z, value: 0 });
                                dst_m_chunks.push((z, 64));
                            } else {
                                let src_i = i - cs;
                                let cur = lm_chunks.get(src_i).copied().unwrap_or_else(|| {
                                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                    block.push(MInst::LoadImm { dst: z, value: 0 });
                                    z
                                });
                                if is == 0 {
                                    dst_m_chunks.push((cur, 64));
                                } else {
                                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::ShlImm {
                                        dst: shifted,
                                        src: cur,
                                        imm: is,
                                    });
                                    let prev = if src_i > 0 {
                                        lm_chunks.get(src_i - 1).copied().unwrap_or_else(|| {
                                            let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                            block.push(MInst::LoadImm { dst: z, value: 0 });
                                            z
                                        })
                                    } else {
                                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                        block.push(MInst::LoadImm { dst: z, value: 0 });
                                        z
                                    };
                                    let carry = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::ShrImm {
                                        dst: carry,
                                        src: prev,
                                        imm: 64 - is,
                                    });
                                    let merged = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Or {
                                        dst: merged,
                                        lhs: shifted,
                                        rhs: carry,
                                    });
                                    dst_m_chunks.push((merged, 64));
                                }
                            }
                        }
                    }
                    BinaryOp::Shr | BinaryOp::Sar => {
                        for i in 0..n_dst {
                            let src_i = i + cs;
                            let cur = lm_chunks.get(src_i).copied().unwrap_or_else(|| {
                                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                block.push(MInst::LoadImm { dst: z, value: 0 });
                                z
                            });
                            if is == 0 {
                                dst_m_chunks.push((cur, 64));
                            } else {
                                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm {
                                    dst: shifted,
                                    src: cur,
                                    imm: is,
                                });
                                let next = lm_chunks.get(src_i + 1).copied().unwrap_or_else(|| {
                                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                    block.push(MInst::LoadImm { dst: z, value: 0 });
                                    z
                                });
                                let carry = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShlImm {
                                    dst: carry,
                                    src: next,
                                    imm: 64 - is,
                                });
                                let merged = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: merged,
                                    lhs: shifted,
                                    rhs: carry,
                                });
                                dst_m_chunks.push((merged, 64));
                            }
                        }

                        // SAR: if the sign bit is X, sign-extension produces X in upper bits.
                        // Check if bit (lhs_width-1) in the mask is set.
                        if matches!(op, BinaryOp::Sar) {
                            let lhs_w = ctx.sir_width(&lhs);
                            let sign_chunk = (lhs_w - 1) / 64;
                            let sign_bit = (lhs_w - 1) % 64;
                            let sign_mask_chunk =
                                lm_chunks.get(sign_chunk).copied().unwrap_or_else(|| {
                                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                    block.push(MInst::LoadImm { dst: z, value: 0 });
                                    z
                                });
                            // Extract sign bit from mask
                            let sign_x = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: sign_x,
                                src: sign_mask_chunk,
                                imm: sign_bit as u8,
                            });
                            let one = ctx.alloc_vreg(SpillDesc::remat(1));
                            block.push(MInst::LoadImm { dst: one, value: 1 });
                            let sign_x_bit = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::And {
                                dst: sign_x_bit,
                                lhs: sign_x,
                                rhs: one,
                            });
                            let sign_is_x = ctx.alloc_vreg(SpillDesc::transient());
                            let z_cmp = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: z_cmp,
                                value: 0,
                            });
                            block.push(MInst::Cmp {
                                dst: sign_is_x,
                                lhs: sign_x_bit,
                                rhs: z_cmp,
                                kind: CmpKind::Ne,
                            });

                            // For chunks above the shifted sign position, OR with all-X if sign is X.
                            // The sign bit after shift is at position (lhs_width - 1 - shift_amount).
                            // All bits above this position in the result are sign-extended.
                            let effective_sign_pos =
                                lhs_w.saturating_sub(1).saturating_sub(amount as usize);
                            let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                            block.push(MInst::LoadImm {
                                dst: all_ones,
                                value: u64::MAX,
                            });
                            for (i, chunk) in dst_m_chunks.iter_mut().enumerate() {
                                let chunk_start = i * 64;
                                if chunk_start >= effective_sign_pos {
                                    // Entire chunk is above sign — all X if sign is X
                                    let new_m = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Select {
                                        dst: new_m,
                                        cond: sign_is_x,
                                        true_val: all_ones,
                                        false_val: chunk.0,
                                    });
                                    chunk.0 = new_m;
                                } else if chunk_start + 64 > effective_sign_pos {
                                    // Partial: bits above effective_sign_pos in this chunk
                                    let bit_in_chunk = effective_sign_pos - chunk_start;
                                    let upper_mask_val = u64::MAX << bit_in_chunk;
                                    let upper_mask =
                                        ctx.alloc_vreg(SpillDesc::remat(upper_mask_val));
                                    block.push(MInst::LoadImm {
                                        dst: upper_mask,
                                        value: upper_mask_val,
                                    });
                                    let x_fill = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Select {
                                        dst: x_fill,
                                        cond: sign_is_x,
                                        true_val: upper_mask,
                                        false_val: z_cmp,
                                    });
                                    let new_m = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Or {
                                        dst: new_m,
                                        lhs: chunk.0,
                                        rhs: x_fill,
                                    });
                                    chunk.0 = new_m;
                                }
                            }
                        }
                    }
                    _ => unreachable!(),
                }

                // Apply X-in-shift-amount override
                let all_x = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                block.push(MInst::LoadImm {
                    dst: all_x,
                    value: u64::MAX,
                });
                let mut final_chunks = Vec::with_capacity(n_dst);
                for (i, (m, w)) in dst_m_chunks.into_iter().enumerate() {
                    let chunk_w = if i == n_dst - 1 {
                        let r = d_width % 64;
                        if r == 0 { 64 } else { r }
                    } else {
                        64
                    };
                    let x_val = if chunk_w < 64 {
                        let v = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                        block.push(MInst::LoadImm {
                            dst: v,
                            value: mask_for_width(chunk_w),
                        });
                        v
                    } else {
                        all_x
                    };
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: shift_has_x,
                        true_val: x_val,
                        false_val: m,
                    });
                    final_chunks.push((res, w));
                }
                ctx.set_mask(dst, final_chunks[0].0);
                ctx.wide_masks.insert(dst, final_chunks);
            } else {
                // Runtime shift: apply the same shift to mask chunks.
                // Temporarily inject mask chunks as wide_regs for a pseudo register,
                // call lower_wide_runtime_shift, then extract result.
                // Use pseudo RegisterIds just past the existing range
                let next_id = ctx.reg_map.map.len();
                let mask_pseudo_lhs = RegisterId(next_id);
                let mask_pseudo_dst = RegisterId(next_id + 1);
                let mask_chunks_wide: Vec<(VReg, usize)> =
                    lm_chunks.iter().map(|&v| (v, 64usize)).collect();
                ctx.wide_regs.insert(mask_pseudo_lhs, mask_chunks_wide);
                // pseudo_dst needs a scalar VReg in reg_map
                let pd_vreg = ctx.alloc_vreg(SpillDesc::transient());
                ctx.reg_map.map.resize(next_id + 2, None);
                ctx.reg_map.map[mask_pseudo_dst.0] = Some(pd_vreg);

                let dir = match op {
                    BinaryOp::Shl => ShiftDir::Left,
                    _ => ShiftDir::Right,
                };
                lower_wide_runtime_shift(
                    ctx,
                    block,
                    mask_pseudo_dst,
                    &mask_pseudo_lhs,
                    &rhs,
                    n_chunks,
                    dir,
                    false,
                );

                // Extract shifted mask chunks
                let shifted_mask_chunks =
                    ctx.wide_regs.remove(&mask_pseudo_dst).unwrap_or_default();
                ctx.wide_regs.remove(&mask_pseudo_lhs);

                // Apply shift-has-X override
                let all_x_v = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                block.push(MInst::LoadImm {
                    dst: all_x_v,
                    value: u64::MAX,
                });
                let mut final_m_chunks = Vec::with_capacity(n_dst);
                for i in 0..n_dst {
                    let chunk_w = if i == n_dst - 1 {
                        let r = d_width % 64;
                        if r == 0 { 64 } else { r }
                    } else {
                        64
                    };
                    let x_val = if chunk_w < 64 {
                        let v = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                        block.push(MInst::LoadImm {
                            dst: v,
                            value: mask_for_width(chunk_w),
                        });
                        v
                    } else {
                        all_x_v
                    };
                    let shifted_m = shifted_mask_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        z
                    });
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: shift_has_x,
                        true_val: x_val,
                        false_val: shifted_m,
                    });
                    final_m_chunks.push((res, 64));
                }
                ctx.set_mask(dst, final_m_chunks[0].0);
                ctx.wide_masks.insert(dst, final_m_chunks);
            }
        }
        _ => {
            // Conservative: any X in any chunk of either operand → all-X result
            let all_masks: Vec<VReg> = lm_chunks.iter().chain(rm_chunks.iter()).copied().collect();
            let has_x = any_chunk_has_x(ctx, block, &all_masks);

            let n_dst = ISelContext::num_chunks(d_width);
            if n_dst == 0 {
                // 1-bit result (comparison)
                ctx.set_mask(dst, has_x);
                return;
            }
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });

            if d_width <= 64 {
                // Narrow result from wide operands (comparisons)
                let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
                block.push(MInst::LoadImm {
                    dst: all_ones,
                    value: mask_for_width(d_width),
                });
                let res = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: res,
                    cond: has_x,
                    true_val: all_ones,
                    false_val: zero,
                });
                ctx.set_mask(dst, res);
            } else {
                let mut dst_m_chunks = Vec::with_capacity(n_dst);
                let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                block.push(MInst::LoadImm {
                    dst: all_ones,
                    value: u64::MAX,
                });
                for i in 0..n_dst {
                    let chunk_m = ctx.alloc_vreg(SpillDesc::transient());
                    let chunk_w = if i == n_dst - 1 {
                        let r = d_width % 64;
                        if r == 0 { 64 } else { r }
                    } else {
                        64
                    };
                    if chunk_w < 64 {
                        let mask_val = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                        block.push(MInst::LoadImm {
                            dst: mask_val,
                            value: mask_for_width(chunk_w),
                        });
                        block.push(MInst::Select {
                            dst: chunk_m,
                            cond: has_x,
                            true_val: mask_val,
                            false_val: zero,
                        });
                    } else {
                        block.push(MInst::Select {
                            dst: chunk_m,
                            cond: has_x,
                            true_val: all_ones,
                            false_val: zero,
                        });
                    }
                    dst_m_chunks.push((chunk_m, 64));
                }
                ctx.set_mask(dst, dst_m_chunks[0].0);
                ctx.wide_masks.insert(dst, dst_m_chunks);
            }
        }
    }
}

/// Compute wide mask for unary operations.
fn lower_wide_unary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    op: &UnaryOp,
    src: RegisterId,
    d_width: usize,
    src_width: usize,
) {
    let n_src = ISelContext::num_chunks(src_width);
    let sm_chunks = get_wide_mask_chunks(ctx, block, &src, n_src);

    match op {
        UnaryOp::Ident | UnaryOp::BitNot => {
            // Mask passes through (per-chunk)
            let n_dst = ISelContext::num_chunks(d_width);
            let mut dst_m_chunks = Vec::with_capacity(n_dst);
            for i in 0..n_dst {
                let m = sm_chunks.get(i).copied().unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                dst_m_chunks.push((m, 64));
            }
            ctx.set_mask(dst, dst_m_chunks[0].0);
            ctx.wide_masks.insert(dst, dst_m_chunks);
        }
        UnaryOp::Minus | UnaryOp::LogicNot => {
            // Conservative: any X → all-X
            let has_x = any_chunk_has_x(ctx, block, &sm_chunks);
            let n_dst = ISelContext::num_chunks(d_width);
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });

            if d_width <= 64 {
                let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
                block.push(MInst::LoadImm {
                    dst: all_ones,
                    value: mask_for_width(d_width),
                });
                let res = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: res,
                    cond: has_x,
                    true_val: all_ones,
                    false_val: zero,
                });
                ctx.set_mask(dst, res);
            } else {
                let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                block.push(MInst::LoadImm {
                    dst: all_ones,
                    value: u64::MAX,
                });
                let mut dst_m_chunks = Vec::with_capacity(n_dst);
                for i in 0..n_dst {
                    let chunk_m = ctx.alloc_vreg(SpillDesc::transient());
                    let chunk_w = if i == n_dst - 1 {
                        let r = d_width % 64;
                        if r == 0 { 64 } else { r }
                    } else {
                        64
                    };
                    if chunk_w < 64 {
                        let mask_val = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                        block.push(MInst::LoadImm {
                            dst: mask_val,
                            value: mask_for_width(chunk_w),
                        });
                        block.push(MInst::Select {
                            dst: chunk_m,
                            cond: has_x,
                            true_val: mask_val,
                            false_val: zero,
                        });
                    } else {
                        block.push(MInst::Select {
                            dst: chunk_m,
                            cond: has_x,
                            true_val: all_ones,
                            false_val: zero,
                        });
                    }
                    dst_m_chunks.push((chunk_m, 64));
                }
                ctx.set_mask(dst, dst_m_chunks[0].0);
                ctx.wide_masks.insert(dst, dst_m_chunks);
            }
        }
        UnaryOp::And | UnaryOp::Or | UnaryOp::Xor => {
            // Reduction ops: result is 1-bit
            // AND: dominant-0 across all chunks
            // OR: dominant-1 across all chunks
            // XOR: any X → result X
            let sv_chunks: Vec<VReg> = if let Some(chunks) = ctx.wide_regs.get(&src).cloned() {
                chunks.iter().map(|c| c.0).collect()
            } else {
                vec![ctx.reg_map.get(src)]
            };

            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = any_chunk_has_x(ctx, block, &sm_chunks);

            match op {
                UnaryOp::And => {
                    // Check for definite-0 across all chunks: ~v & ~m
                    let mut any_def_zero = zero;
                    for i in 0..n_src {
                        let not_v = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_v,
                            src: sv_chunks[i.min(sv_chunks.len() - 1)],
                        });
                        let not_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_m,
                            src: sm_chunks[i],
                        });
                        let def_z = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: def_z,
                            lhs: not_v,
                            rhs: not_m,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: any_def_zero,
                            rhs: def_z,
                        });
                        any_def_zero = combined;
                    }
                    let has_def_zero = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: has_def_zero,
                        lhs: any_def_zero,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    let x_mask = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: x_mask,
                        cond: has_x,
                        true_val: has_x,
                        false_val: zero,
                    });
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: has_def_zero,
                        true_val: zero,
                        false_val: x_mask,
                    });
                    ctx.set_mask(dst, res);
                }
                UnaryOp::Or => {
                    // Check for definite-1 across all chunks: v & ~m
                    let mut any_def_one = zero;
                    for i in 0..n_src {
                        let not_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_m,
                            src: sm_chunks[i],
                        });
                        let def_one = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: def_one,
                            lhs: sv_chunks[i.min(sv_chunks.len() - 1)],
                            rhs: not_m,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: any_def_one,
                            rhs: def_one,
                        });
                        any_def_one = combined;
                    }
                    let has_def_one = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: has_def_one,
                        lhs: any_def_one,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    let x_mask = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: x_mask,
                        cond: has_x,
                        true_val: has_x,
                        false_val: zero,
                    });
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: has_def_one,
                        true_val: zero,
                        false_val: x_mask,
                    });
                    ctx.set_mask(dst, res);
                }
                _ => {
                    // XOR: any X → result X
                    ctx.set_mask(dst, has_x);
                }
            }
        }
    }
}
