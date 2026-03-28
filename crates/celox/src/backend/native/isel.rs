//! Instruction Selection: lowers SIR (bit-level SSA) to MIR (word-level SSA).
//!
//! Current scope: 2-state, ≤64-bit values only. 4-state and multi-word
//! support will be added incrementally.

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
        self.map[reg.0]
            .unwrap_or_else(|| panic!("SIR register r{} not yet defined", reg.0))
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

    let mut ctx = ISelContext {
        vregs: &mut func.vregs,
        spill_descs: &mut func.spill_descs,
        reg_map: &mut reg_map,
        register_types: &eu.register_map,
        layout,
        wide_regs: WideRegMap::default(),
        reg_addrs: crate::HashMap::default(),
        consts: ConstMap::default(),
    };

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
                ctx.reg_addrs.insert(*dst, addr.clone());
            }
        }

        // Lower instructions
        for inst in &sir_block.instructions {
            lower_instruction(&mut ctx, &mut mblock, inst);
        }

        // Lower terminator
        lower_terminator(&mut ctx, &mut mblock, &sir_block.terminator);

        func.blocks.push(mblock);
    }

    // Build phi nodes from SIR block params and predecessor terminators.
    // For each SIR block with params, find all predecessors that pass args.
    {
        use std::collections::HashMap;
        // Collect phi sources: target_block → [(pred_block, param_idx, arg_vreg)]
        let mut phi_sources: HashMap<BlockId, Vec<(BlockId, usize, VReg)>> = HashMap::new();
        for &sir_block_id in &block_ids {
            let sir_block = &eu.blocks[&sir_block_id];
            let pred_mir_id = BlockId(sir_block_id.0 as u32);
            // Collect (target_sir_id, args) from terminator
            let edges: Vec<(crate::ir::BlockId, &[RegisterId])> = match &sir_block.terminator {
                SIRTerminator::Jump(target, args) => vec![(*target, args.as_slice())],
                SIRTerminator::Branch { true_block, false_block, .. } => vec![
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
                        mblock.phis.push(PhiNode { dst, sources: phi_srcs });
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
    if width >= 64 { u64::MAX } else { (1u64 << width) - 1 }
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

    /// Number of 64-bit chunks needed for a given bit width.
    fn num_chunks(width_bits: usize) -> usize {
        (width_bits + 63) / 64
    }

    /// Get or create wide chunks for a SIR register.
    /// If the register is already tracked as wide, returns existing chunks.
    /// If it's a scalar (≤64-bit), promotes it to a wide value with zero-extended chunks.
    fn get_wide_chunks(
        &mut self,
        reg: &RegisterId,
        block: &mut MBlock,
    ) -> Vec<(VReg, usize)> {
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
            block.push(MInst::LoadImm { dst: zero, value: 0 });
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
            block.push(MInst::LoadImm { dst: vreg, value: imm_val });
            // Track constant value for later folding
            ctx.consts.insert(*dst, imm_val);

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
                        block.push(MInst::LoadImm { dst: cv, value: chunk_val });
                        chunks.push((cv, 64));
                    }
                }
                ctx.set_wide_chunks(*dst, chunks);
            }
        }

        SIRInstruction::Load(dst, addr, offset, width_bits) => {
            ctx.reg_addrs.insert(*dst, addr.clone());
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
                                addr.clone(), bit_pos, chunk_bits, false,
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
                        block.push(MInst::Mov { dst: vreg, src: chunks[0].0 });
                        ctx.wide_regs.insert(*dst, chunks);
                        return;
                    }

                    let byte_off = ctx.byte_offset(addr, *bit_off);
                    let intra_byte = bit_off % 8;
                    let op_size = ISelContext::op_size_for_width(*width_bits);

                    // Update spill desc
                    ctx.spill_descs[vreg.0 as usize] =
                        SpillDesc::sim_state(addr.clone(), *bit_off, *width_bits, false);

                    if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
                        // Word-aligned, native size: single load.
                        // If the load is wider than the variable (SIR optimizer widening),
                        // mask the result to the variable's actual width.
                        let var_width = ctx.layout.widths.get(&addr.absolute_addr()).copied().unwrap_or(*width_bits);
                        if var_width < *width_bits && var_width < 64 {
                            let raw = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: raw,
                                base: BaseReg::SimState,
                                offset: byte_off,
                                size: op_size,
                            });
                            block.push(MInst::AndImm {
                                dst: vreg,
                                src: raw,
                                imm: mask_for_width(var_width),
                            });
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
                            block.push(MInst::AndImm {
                                dst: vreg,
                                src: shifted,
                                imm: mask,
                            });
                        } else {
                            // Byte-aligned but non-native width: just mask
                            let mask = mask_for_width(*width_bits);
                            block.push(MInst::AndImm {
                                dst: vreg,
                                src: tmp,
                                imm: mask,
                            });
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
                    block.push(MInst::AndImm {
                        dst: bit_shift,
                        src: offset_vreg,
                        imm: 7,
                    });

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
                        block.push(MInst::AndImm {
                            dst: vreg,
                            src: shifted,
                            imm: mask,
                        });
                    } else {
                        block.push(MInst::Mov {
                            dst: vreg,
                            src: shifted,
                        });
                    }
                }
            }
        }

        SIRInstruction::Store(addr, offset, width_bits, src_reg, _triggers) => {
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
                                    let val_copy = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Mov { dst: val_copy, src: *chunk_vreg });
                                    let new = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::BitFieldInsert {
                                        dst: new,
                                        base_word: old,
                                        val: val_copy,
                                        shift: intra as u8,
                                        mask,
                                    });
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
                                let advance = (chunk_bits + 7) / 8;
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
                            // Copy val to fresh VReg so BFI emit can clobber it freely.
                            let val_copy = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Mov { dst: val_copy, src: src_vreg });
                            let new_word = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::BitFieldInsert {
                                dst: new_word,
                                base_word: old_word,
                                val: val_copy,
                                shift: intra_byte as u8,
                                mask,
                            });

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
                    block.push(MInst::AndImm {
                        dst: bit_shift,
                        src: offset_vreg,
                        imm: 7,
                    });

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
                        block.push(MInst::AndImm {
                            dst: masked_src,
                            src: src_vreg,
                            imm: mask_val,
                        });
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
                            let advance = (chunk_bits + 7) / 8;
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
                    let phys_bytes = (*width_bits + 7) / 8;
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
        }

        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let d_width = ctx.sir_width(dst);
            let lhs_width = ctx.sir_width(lhs);

            // Wide (>64-bit) binary operations: dispatch to multi-word handler.
            // For comparisons/logic, the result may be narrow (1 bit) but the
            // operands can be wide — dispatch based on operand width too.
            if d_width > 64 || lhs_width > 64 {
                lower_wide_binary(ctx, block, *dst, *lhs, op, *rhs);
                return;
            }
            // Also check if operands have wide chunks (may have been loaded wide
            // even if sir_width reports ≤64 due to optimizer width changes).
            if ctx.wide_regs.contains_key(lhs) || ctx.wide_regs.contains_key(rhs) {
                lower_wide_binary(ctx, block, *dst, *lhs, op, *rhs);
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
                    block.push(MInst::LoadImm { dst: dst_vreg, value: val });
                    ctx.consts.insert(*dst, val);
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
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        tmp
                    } else {
                        dst_vreg
                    };
                    match op {
                        BinaryOp::Add => block.push(MInst::Add { dst: raw, lhs: lhs_vreg, rhs: rhs_vreg }),
                        BinaryOp::Sub => block.push(MInst::Sub { dst: raw, lhs: lhs_vreg, rhs: rhs_vreg }),
                        BinaryOp::Mul => block.push(MInst::Mul { dst: raw, lhs: lhs_vreg, rhs: rhs_vreg }),
                        _ => unreachable!(),
                    }
                    if d_width < 64 {
                        block.push(MInst::AndImm { dst: dst_vreg, src: raw, imm: mask_for_width(d_width) });
                    }
                }
                BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
                    // Operands may be wider than d_width (SIR optimizer widens
                    // Loads). Mask result to d_width to prevent upper-bit garbage
                    // from propagating to comparisons and stores.
                    let raw = if d_width < 64 {
                        ctx.alloc_vreg(SpillDesc::transient())
                    } else {
                        dst_vreg
                    };
                    match op {
                        BinaryOp::And => block.push(MInst::And { dst: raw, lhs: lhs_vreg, rhs: rhs_vreg }),
                        BinaryOp::Or => block.push(MInst::Or { dst: raw, lhs: lhs_vreg, rhs: rhs_vreg }),
                        BinaryOp::Xor => block.push(MInst::Xor { dst: raw, lhs: lhs_vreg, rhs: rhs_vreg }),
                        _ => unreachable!(),
                    }
                    if d_width < 64 {
                        block.push(MInst::AndImm { dst: dst_vreg, src: raw, imm: mask_for_width(d_width) });
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
                    // Use ShrImm if shift amount is a known constant (avoids CL clobber issues)
                    if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        block.push(MInst::ShrImm {
                            dst: shifted,
                            src: lhs_vreg,
                            imm: shift_amt as u8,
                        });
                    } else {
                        // Copy rhs to fresh VReg so assignment can place it in RCX
                        // without clobbering the original (which may be live).
                        let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Mov { dst: rhs_copy, src: rhs_vreg });
                        block.push(MInst::Shr {
                            dst: shifted,
                            lhs: lhs_vreg,
                            rhs: rhs_copy,
                        });
                    }
                    // Mask to destination width
                    if d_width < 64 {
                        let mask = mask_for_width(d_width);
                        block.push(MInst::AndImm {
                            dst: dst_vreg,
                            src: shifted,
                            imm: mask,
                        });
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
                        block.push(MInst::Mov { dst: rhs_copy, src: rhs_vreg });
                        block.push(MInst::Shl {
                            dst: shifted,
                            lhs: lhs_vreg,
                            rhs: rhs_copy,
                        });
                    }
                    if d_width < 64 {
                        let mask = mask_for_width(d_width);
                        block.push(MInst::AndImm {
                            dst: dst_vreg,
                            src: shifted,
                            imm: mask,
                        });
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
                        block.push(MInst::ShlImm { dst: shifted_up, src: lhs_vreg, imm: sext_shift });
                        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::SarImm { dst: sign_extended, src: shifted_up, imm: sext_shift });
                        // Now do the actual shift
                        let sar_result = ctx.alloc_vreg(SpillDesc::transient());
                        if let Some(&shift_amt) = ctx.consts.get(rhs) {
                            block.push(MInst::SarImm { dst: sar_result, src: sign_extended, imm: shift_amt as u8 });
                        } else {
                            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Mov { dst: rhs_copy, src: rhs_vreg });
                            block.push(MInst::Sar { dst: sar_result, lhs: sign_extended, rhs: rhs_copy });
                        }
                        // Mask to output width
                        let mask = mask_for_width(width);
                        block.push(MInst::AndImm { dst: dst_vreg, src: sar_result, imm: mask });
                    } else {
                        if let Some(&shift_amt) = ctx.consts.get(rhs) {
                            block.push(MInst::SarImm { dst: dst_vreg, src: lhs_vreg, imm: shift_amt as u8 });
                        } else {
                            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Mov { dst: rhs_copy, src: rhs_vreg });
                            block.push(MInst::Sar { dst: dst_vreg, lhs: lhs_vreg, rhs: rhs_copy });
                        }
                    }
                }
                BinaryOp::Eq => block.push(MInst::Cmp {
                    dst: dst_vreg, lhs: lhs_vreg, rhs: rhs_vreg, kind: CmpKind::Eq,
                }),
                BinaryOp::Ne => block.push(MInst::Cmp {
                    dst: dst_vreg, lhs: lhs_vreg, rhs: rhs_vreg, kind: CmpKind::Ne,
                }),
                BinaryOp::LtU => block.push(MInst::Cmp {
                    dst: dst_vreg, lhs: lhs_vreg, rhs: rhs_vreg, kind: CmpKind::LtU,
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
                    dst: dst_vreg, lhs: lhs_vreg, rhs: rhs_vreg, kind: CmpKind::LeU,
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
                    dst: dst_vreg, lhs: lhs_vreg, rhs: rhs_vreg, kind: CmpKind::GtU,
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
                    dst: dst_vreg, lhs: lhs_vreg, rhs: rhs_vreg, kind: CmpKind::GeU,
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
                    // Regalloc handles RAX/RDX clobber via clobbers() in assignment.
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: zero, value: 0 });
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
                    if matches!(op, BinaryOp::Div) {
                        block.push(MInst::UDiv {
                            dst: dst_vreg,
                            lhs: lhs_vreg,
                            rhs: safe_rhs,
                        });
                    } else {
                        block.push(MInst::URem {
                            dst: dst_vreg,
                            lhs: lhs_vreg,
                            rhs: safe_rhs,
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

        SIRInstruction::Unary(dst, op, src) => {
            let d_width = ctx.sir_width(dst);
            let src_width = ctx.sir_width(src);
            if d_width > 64 || src_width > 64
                || ctx.wide_regs.contains_key(src)
            {
                lower_wide_unary(ctx, block, *dst, op, *src);
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
                        block.push(MInst::BitNot { dst: tmp, src: src_vreg });
                        block.push(MInst::AndImm { dst: dst_vreg, src: tmp, imm: mask_for_width(width) });
                    } else {
                        block.push(MInst::BitNot { dst: dst_vreg, src: src_vreg });
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
                    // XOR-fold the value down to 1 bit:
                    // val ^= val >> 32; val ^= val >> 16; val ^= val >> 8;
                    // val ^= val >> 4; val ^= val >> 2; val ^= val >> 1; val & 1
                    let width = ctx.sir_width(src);
                    let mut cur = src_vreg;

                    // Fold progressively: for width ≤ N, skip shifts ≥ N
                    for shift in [32u8, 16, 8, 4, 2, 1] {
                        if width as u8 > shift {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: shifted,
                                src: cur,
                                imm: shift,
                            });
                            let folded = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Xor {
                                dst: folded,
                                lhs: cur,
                                rhs: shifted,
                            });
                            cur = folded;
                        }
                    }
                    // Extract bit 0
                    block.push(MInst::AndImm {
                        dst: dst_vreg,
                        src: cur,
                        imm: 1,
                    });
                }
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
            } else {
                // Wide concat (>64 bits): record chunk vregs for use by Store.
                // args are [MSB, ..., LSB]. Store in LSB-first order.
                // If an arg is itself wider than 64 bits, expand its chunks.
                let mut chunks = Vec::new();
                for arg in args.iter().rev() {
                    let arg_width = ctx.sir_width(arg);
                    if arg_width > 64 {
                        let arg_chunks = ctx.get_wide_chunks(arg, block);
                        for ch in arg_chunks {
                            chunks.push(ch);
                        }
                    } else {
                        let arg_vreg = ctx.reg_map.get(*arg);
                        chunks.push((arg_vreg, arg_width));
                    }
                }
                ctx.wide_regs.insert(*dst, chunks);
                // No MIR instructions emitted; the value is consumed by Store.
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
                            dst: tmp, base: BaseReg::SimState,
                            offset: byte_off, size: load_size,
                        });
                        if intra_byte > 0 {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm { dst: shifted, src: tmp, imm: intra_byte as u8 });
                            let mask = mask_for_width(*width);
                            block.push(MInst::AndImm { dst: dst_vreg, src: shifted, imm: mask });
                        } else {
                            let mask = mask_for_width(*width);
                            block.push(MInst::AndImm { dst: dst_vreg, src: tmp, imm: mask });
                        }
                    }
                    return;
                }
            }

            if *width <= 64 && src_width <= 64 {
                let src_vreg = ctx.reg_map.get(*src);
                if *bit_offset == 0 && *width == src_width {
                    block.push(MInst::Mov { dst: dst_vreg, src: src_vreg });
                } else if *bit_offset == 0 {
                    let mask = mask_for_width(*width);
                    block.push(MInst::AndImm { dst: dst_vreg, src: src_vreg, imm: mask });
                } else {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm { dst: shifted, src: src_vreg, imm: *bit_offset as u8 });
                    let mask = mask_for_width(*width);
                    block.push(MInst::AndImm { dst: dst_vreg, src: shifted, imm: mask });
                }
            } else if *width <= 64 {
                // Narrow slice from wide source
                let src_chunks = ctx.get_wide_chunks(src, block);
                let chunk_idx = *bit_offset / 64;
                let intra_bit = *bit_offset % 64;
                let main = ctx.wide_chunk_or_zero(&src_chunks, chunk_idx, block);

                if intra_bit == 0 {
                    let mask = mask_for_width(*width);
                    block.push(MInst::AndImm { dst: dst_vreg, src: main, imm: mask });
                } else if intra_bit + *width <= 64 {
                    // Fits in one chunk after shift
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm { dst: shifted, src: main, imm: intra_bit as u8 });
                    let mask = mask_for_width(*width);
                    block.push(MInst::AndImm { dst: dst_vreg, src: shifted, imm: mask });
                } else {
                    // Crosses chunk boundary
                    let lo = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm { dst: lo, src: main, imm: intra_bit as u8 });
                    let upper = ctx.wide_chunk_or_zero(&src_chunks, chunk_idx + 1, block);
                    let hi = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShlImm { dst: hi, src: upper, imm: (64 - intra_bit) as u8 });
                    let combined = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or { dst: combined, lhs: lo, rhs: hi });
                    let mask = mask_for_width(*width);
                    block.push(MInst::AndImm { dst: dst_vreg, src: combined, imm: mask });
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
                        block.push(MInst::ShrImm { dst: lo, src: main, imm: intra_bit as u8 });
                        let upper = ctx.wide_chunk_or_zero(&src_chunks, src_idx + 1, block);
                        let hi = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShlImm { dst: hi, src: upper, imm: (64 - intra_bit) as u8 });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or { dst: combined, lhs: lo, rhs: hi });
                        dst_chunks.push((combined, 64));
                    }
                }

                // Mask the top chunk to the exact width
                let top_bits = *width % 64;
                if top_bits != 0 && !dst_chunks.is_empty() {
                    let last_idx = dst_chunks.len() - 1;
                    let (last_vreg, _) = dst_chunks[last_idx];
                    let masked = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::AndImm { dst: masked, src: last_vreg, imm: mask_for_width(top_bits) });
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
                    BinaryOp::And => block.push(MInst::And { dst: d, lhs: l, rhs: r }),
                    BinaryOp::Or => block.push(MInst::Or { dst: d, lhs: l, rhs: r }),
                    BinaryOp::Xor => block.push(MInst::Xor { dst: d, lhs: l, rhs: r }),
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
                let cs = (amount / 64) as usize;    // chunk shift
                let is = (amount % 64) as u8;        // intra-chunk shift

                let mut dst_chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    if i < cs {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        dst_chunks.push((z, 64));
                    } else {
                        let src_idx = i - cs;
                        let main_vreg = if src_idx < n_src { src_chunks[src_idx].0 } else {
                            let z = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm { dst: z, value: 0 });
                            z
                        };

                        if is == 0 {
                            dst_chunks.push((main_vreg, 64));
                        } else {
                            // main_part = src[src_idx] << is
                            let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm { dst: main_shifted, src: main_vreg, imm: is });

                            // carry from lower chunk: src[src_idx-1] >> (64 - is)
                            if src_idx > 0 && (src_idx - 1) < n_src {
                                let carry_vreg = src_chunks[src_idx - 1].0;
                                let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm { dst: carry_shifted, src: carry_vreg, imm: 64 - is });
                                let combined = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or { dst: combined, lhs: main_shifted, rhs: carry_shifted });
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
                lower_wide_runtime_shift(ctx, block, dst, &lhs, &rhs, n_chunks, ShiftDir::Left, false);
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
                    block.push(MInst::Add { dst: s1, lhs: l, rhs: r });
                    // c1 = (s1 < l) unsigned
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: c1, lhs: s1, rhs: l, kind: CmpKind::LtU });
                    // s2 = s1 + cin
                    let s2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add { dst: s2, lhs: s1, rhs: cin });
                    // c2 = (s2 < s1) unsigned
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: c2, lhs: s2, rhs: s1, kind: CmpKind::LtU });
                    // carry = c1 | c2
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or { dst: cout, lhs: c1, rhs: c2 });
                    carry = Some(cout);
                    dst_chunks.push((s2, 64));
                } else {
                    // s = l + r
                    let s = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add { dst: s, lhs: l, rhs: r });
                    // carry = (s < l) unsigned
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: cout, lhs: s, rhs: l, kind: CmpKind::LtU });
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
                    block.push(MInst::Sub { dst: d1, lhs: l, rhs: r });
                    // b1 = (r > l) unsigned
                    let b1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: b1, lhs: r, rhs: l, kind: CmpKind::GtU });
                    // d2 = d1 - bin
                    let d2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub { dst: d2, lhs: d1, rhs: bin });
                    // b2 = (bin > d1) unsigned
                    let b2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: b2, lhs: bin, rhs: d1, kind: CmpKind::GtU });
                    // borrow = b1 | b2
                    let bout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or { dst: bout, lhs: b1, rhs: b2 });
                    borrow = Some(bout);
                    dst_chunks.push((d2, 64));
                } else {
                    // d = l - r
                    let d = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub { dst: d, lhs: l, rhs: r });
                    // borrow = (r > l) unsigned
                    let bout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: bout, lhs: r, rhs: l, kind: CmpKind::GtU });
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
            block.push(MInst::LoadImm { dst: init, value: if is_eq { 1 } else { 0 } });
            let mut cond = init;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp { dst: eq, lhs: l, rhs: r, kind: CmpKind::Eq });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                if is_eq {
                    block.push(MInst::And { dst: next, lhs: cond, rhs: eq });
                } else {
                    // ne: accumulate OR of (chunk != chunk)
                    let neq = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: neq, lhs: l, rhs: r, kind: CmpKind::Ne });
                    block.push(MInst::Or { dst: next, lhs: cond, rhs: neq });
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
            let init_val = if matches!(op, BinaryOp::LeU | BinaryOp::GeU) { 1u64 } else { 0u64 };
            let init = ctx.alloc_vreg(SpillDesc::remat(init_val));
            block.push(MInst::LoadImm { dst: init, value: init_val });
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
                block.push(MInst::Cmp { dst: eq, lhs: l, rhs: r, kind: CmpKind::Eq });
                let cmp = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp { dst: cmp, lhs: l, rhs: r, kind: cmp_kind });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select { dst: next, cond: eq, true_val: res, false_val: cmp });
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

            let init_val = if matches!(op, BinaryOp::LeS | BinaryOp::GeS) { 1u64 } else { 0u64 };
            let init = ctx.alloc_vreg(SpillDesc::remat(init_val));
            block.push(MInst::LoadImm { dst: init, value: init_val });
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
                block.push(MInst::Cmp { dst: eq, lhs: l, rhs: r, kind: CmpKind::Eq });
                // MSB chunk uses signed comparison, lower chunks use unsigned
                let kind = if i == n_chunks - 1 { signed_kind } else { unsigned_kind };
                let cmp = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp { dst: cmp, lhs: l, rhs: r, kind: kind });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select { dst: next, cond: eq, true_val: res, false_val: cmp });
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
                let cs = (amount / 64) as usize;  // chunk shift
                let is = (amount % 64) as u8;      // intra-chunk shift

                let mut dst_chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    let src_idx = i + cs;
                    let main_vreg = if src_idx < n_src {
                        src_chunks[src_idx].0
                    } else if is_sar {
                        // SAR: fill with sign extension from MSB chunk
                        let msb = src_chunks[n_src - 1].0;
                        let sign = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::SarImm { dst: sign, src: msb, imm: 63 });
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
                            block.push(MInst::SarImm { dst: main_shifted, src: main_vreg, imm: is });
                        } else {
                            block.push(MInst::ShrImm { dst: main_shifted, src: main_vreg, imm: is });
                        }

                        // carry from upper chunk: src[src_idx+1] << (64 - is)
                        let upper_idx = src_idx + 1;
                        if upper_idx < n_src {
                            let carry_vreg = src_chunks[upper_idx].0;
                            let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm { dst: carry_shifted, src: carry_vreg, imm: 64 - is });
                            let combined = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or { dst: combined, lhs: main_shifted, rhs: carry_shifted });
                            dst_chunks.push((combined, 64));
                        } else if is_sar && i < n_chunks - 1 {
                            // SAR: carry from sign-extended chunk
                            let msb = src_chunks[n_src - 1].0;
                            let sign = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::SarImm { dst: sign, src: msb, imm: 63 });
                            let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm { dst: carry_shifted, src: sign, imm: 64 - is });
                            let combined = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or { dst: combined, lhs: main_shifted, rhs: carry_shifted });
                            dst_chunks.push((combined, 64));
                        } else {
                            dst_chunks.push((main_shifted, 64));
                        }
                    }
                }
                ctx.set_wide_chunks(dst, dst_chunks);
            } else {
                // Runtime right shift: select chain + carry propagation.
                let dir = if is_sar { ShiftDir::ArithRight } else { ShiftDir::Right };
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
                BinaryOp::LogicAnd => block.push(MInst::And { dst: result, lhs: lhs_bool, rhs: rhs_bool }),
                BinaryOp::LogicOr => block.push(MInst::Or { dst: result, lhs: lhs_bool, rhs: rhs_bool }),
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
            let mut acc: Vec<VReg> = (0..n_chunks).map(|_| {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                z
            }).collect();

            for i in 0..n_chunks {
                let a_i = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let mut carry = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: carry, value: 0 });

                for j in 0..n_chunks {
                    let k = i + j;
                    if k >= n_chunks { break; }

                    let b_j = ctx.wide_chunk_or_zero(&rhs_chunks, j, block);

                    // lo = a_i * b_j, hi = umulhi(a_i, b_j)
                    let lo = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Mul { dst: lo, lhs: a_i, rhs: b_j });
                    let hi = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::UMulHi { dst: hi, lhs: a_i, rhs: b_j });

                    // sum1 = acc[k] + lo
                    let sum1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add { dst: sum1, lhs: acc[k], rhs: lo });
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: c1, lhs: sum1, rhs: acc[k], kind: CmpKind::LtU });

                    // sum2 = sum1 + carry
                    let sum2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add { dst: sum2, lhs: sum1, rhs: carry });
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: c2, lhs: sum2, rhs: sum1, kind: CmpKind::LtU });

                    acc[k] = sum2;

                    // carry = hi + c1 + c2
                    let carry1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add { dst: carry1, lhs: hi, rhs: c1 });
                    let new_carry = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add { dst: new_carry, lhs: carry1, rhs: c2 });
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

            let mut q_chunks: Vec<VReg> = (0..n_chunks).map(|_| {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                z
            }).collect();
            let mut rem_chunks: Vec<VReg> = (0..n_chunks).map(|_| {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                z
            }).collect();

            for bit in (0..total_bits).rev() {
                let chunk_idx = bit / 64;
                let bit_idx = bit % 64;

                // remainder <<= 1
                for c in (0..n_chunks).rev() {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShlImm { dst: shifted, src: rem_chunks[c], imm: 1 });
                    if c > 0 {
                        let carry_bit = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm { dst: carry_bit, src: rem_chunks[c - 1], imm: 63 });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or { dst: combined, lhs: shifted, rhs: carry_bit });
                        rem_chunks[c] = combined;
                    } else {
                        rem_chunks[c] = shifted;
                    }
                }

                // remainder[0] |= (dividend[chunk_idx] >> bit_idx) & 1
                let dividend_chunk = ctx.wide_chunk_or_zero(&lhs_chunks, chunk_idx, block);
                let extracted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm { dst: extracted, src: dividend_chunk, imm: bit_idx as u8 });
                let one_bit = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::AndImm { dst: one_bit, src: extracted, imm: 1 });
                let new_rem0 = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or { dst: new_rem0, lhs: rem_chunks[0], rhs: one_bit });
                rem_chunks[0] = new_rem0;

                // if remainder >= divisor (chunk-wise unsigned comparison)
                let init_ge = ctx.alloc_vreg(SpillDesc::remat(1));
                block.push(MInst::LoadImm { dst: init_ge, value: 1 });
                let mut ge = init_ge;
                for c in 0..n_chunks {
                    let rc = rem_chunks[c];
                    let dc = ctx.wide_chunk_or_zero(&rhs_chunks, c, block);
                    let eq = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: eq, lhs: rc, rhs: dc, kind: CmpKind::Eq });
                    let gt = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: gt, lhs: rc, rhs: dc, kind: CmpKind::GeU });
                    let next_ge = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select { dst: next_ge, cond: eq, true_val: ge, false_val: gt });
                    ge = next_ge;
                }

                // conditional: remainder -= divisor (wide sub with borrow)
                let mut borrow: Option<VReg> = None;
                for c in 0..n_chunks {
                    let rc = rem_chunks[c];
                    let dc = ctx.wide_chunk_or_zero(&rhs_chunks, c, block);

                    let (diff, bout) = if let Some(bin) = borrow {
                        let d1 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub { dst: d1, lhs: rc, rhs: dc });
                        let b1 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp { dst: b1, lhs: dc, rhs: rc, kind: CmpKind::GtU });
                        let d2 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub { dst: d2, lhs: d1, rhs: bin });
                        let b2 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp { dst: b2, lhs: bin, rhs: d1, kind: CmpKind::GtU });
                        let bout = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or { dst: bout, lhs: b1, rhs: b2 });
                        (d2, bout)
                    } else {
                        let d = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub { dst: d, lhs: rc, rhs: dc });
                        let bout = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp { dst: bout, lhs: dc, rhs: rc, kind: CmpKind::GtU });
                        (d, bout)
                    };

                    // select: if ge then subtracted else original
                    let new_rc = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select { dst: new_rc, cond: ge, true_val: diff, false_val: rc });
                    rem_chunks[c] = new_rc;
                    borrow = Some(bout);
                }

                // quotient[chunk_idx] |= ge ? (1 << bit_idx) : 0
                let bit_mask = ctx.alloc_vreg(SpillDesc::remat(1u64 << bit_idx));
                block.push(MInst::LoadImm { dst: bit_mask, value: 1u64 << bit_idx });
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: zero, value: 0 });
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select { dst: masked, cond: ge, true_val: bit_mask, false_val: zero });
                let new_q = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or { dst: new_q, lhs: q_chunks[chunk_idx], rhs: masked });
                q_chunks[chunk_idx] = new_q;
            }

            let result_chunks = if matches!(op, BinaryOp::Div) { q_chunks } else { rem_chunks };
            let dst_chunks: Vec<(VReg, usize)> = result_chunks.into_iter().map(|v| (v, 64)).collect();
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
                block.push(MInst::Mov { dst: scalar, src: chunk0 });
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ShiftDir { Left, Right, ArithRight }

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
    block.push(MInst::ShrImm { dst: chunk_shift, src: amount_vreg, imm: 6 });
    let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::AndImm { dst: bit_shift, src: amount_vreg, imm: 63 });
    let sixty_four = ctx.alloc_vreg(SpillDesc::remat(64));
    block.push(MInst::LoadImm { dst: sixty_four, value: 64 });
    let inv_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub { dst: inv_bit_shift, lhs: sixty_four, rhs: bit_shift });
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm { dst: zero, value: 0 });
    let has_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp { dst: has_bit_shift, lhs: bit_shift, rhs: zero, kind: CmpKind::Ne });

    // Fill value: 0 for SHL/SHR, sign-extension for SAR
    let fill = if matches!(dir, ShiftDir::ArithRight) {
        let msb = src_chunks[n_src - 1].0;
        let sf = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm { dst: sf, src: msb, imm: 63 });
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
                block.push(MInst::LoadImm { dst: j_vreg, value: j as u64 });
                let eff_idx = ctx.alloc_vreg(SpillDesc::transient());
                match dir {
                    ShiftDir::Left => {
                        // src[j] goes to dst[j + word_offset]
                        block.push(MInst::Add { dst: eff_idx, lhs: j_vreg, rhs: chunk_shift });
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        // src[j + word_offset] goes to dst[j], i.e., src[j] goes to dst[j - word_offset]
                        // Check: j >= word_offset, then eff = j - word_offset
                        // Simpler: for dst[i], source is src[i + word_offset]
                        // So we select j if j == i + word_offset
                        block.push(MInst::Sub { dst: eff_idx, lhs: j_vreg, rhs: chunk_shift });
                    }
                }
                let i_vreg = ctx.alloc_vreg(SpillDesc::remat(i as u64));
                block.push(MInst::LoadImm { dst: i_vreg, value: i as u64 });
                let is_match = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp { dst: is_match, lhs: eff_idx, rhs: i_vreg, kind: CmpKind::Eq });
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select { dst: selected, cond: is_match, true_val: src_chunks[j].0, false_val: val });
                val = selected;
            }
            val
        };

        // Select the "carry" source chunk (adjacent in shift direction)
        let carry_chunk = {
            let mut val = fill;
            for j in (0..n_src).rev() {
                let j_vreg = ctx.alloc_vreg(SpillDesc::remat(j as u64));
                block.push(MInst::LoadImm { dst: j_vreg, value: j as u64 });
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
                        block.push(MInst::Add { dst: eff_idx, lhs: j_vreg, rhs: chunk_shift });
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        block.push(MInst::Sub { dst: eff_idx, lhs: j_vreg, rhs: chunk_shift });
                    }
                }
                let ci_vreg = ctx.alloc_vreg(SpillDesc::remat(carry_i as u64));
                block.push(MInst::LoadImm { dst: ci_vreg, value: carry_i as u64 });
                let is_match = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp { dst: is_match, lhs: eff_idx, rhs: ci_vreg, kind: CmpKind::Eq });
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select { dst: selected, cond: is_match, true_val: src_chunks[j].0, false_val: val });
                val = selected;
            }
            val
        };

        // Apply intra-chunk shift: result = (main_chunk SHIFT bit_shift) | (carry_chunk INVSHIFT inv_bit_shift)
        // (debug removed)
        let bit_shift_copy = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mov { dst: bit_shift_copy, src: bit_shift });
        let inv_copy = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mov { dst: inv_copy, src: inv_bit_shift });

        let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
        let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());

        match dir {
            ShiftDir::Left => {
                block.push(MInst::Shl { dst: main_shifted, lhs: main_chunk, rhs: bit_shift_copy });
                block.push(MInst::Shr { dst: carry_shifted, lhs: carry_chunk, rhs: inv_copy });
            }
            ShiftDir::Right => {
                block.push(MInst::Shr { dst: main_shifted, lhs: main_chunk, rhs: bit_shift_copy });
                block.push(MInst::Shl { dst: carry_shifted, lhs: carry_chunk, rhs: inv_copy });
            }
            ShiftDir::ArithRight => {
                if i == n_chunks - 1 {
                    block.push(MInst::Sar { dst: main_shifted, lhs: main_chunk, rhs: bit_shift_copy });
                } else {
                    block.push(MInst::Shr { dst: main_shifted, lhs: main_chunk, rhs: bit_shift_copy });
                }
                block.push(MInst::Shl { dst: carry_shifted, lhs: carry_chunk, rhs: inv_copy });
            }
        }

        // Combine: if has_bit_shift then (main | carry) else main
        let combined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or { dst: combined, lhs: main_shifted, rhs: carry_shifted });
        let result = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select { dst: result, cond: has_bit_shift, true_val: combined, false_val: main_chunk });

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
    block.push(MInst::LoadImm { dst: zero, value: 0 });
    let mut acc = zero;
    for i in 0..n_chunks {
        let c = chunks.get(i).map(|c| c.0).unwrap_or(zero);
        let next = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or { dst: next, lhs: acc, rhs: c });
        acc = next;
    }
    // acc != 0 → 1
    let result = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp { dst: result, lhs: acc, rhs: zero, kind: CmpKind::Ne });
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
            for i in 0..n_chunks {
                let l = inv_chunks[i].0;
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
                block.push(MInst::Add { dst: s, lhs: l, rhs: r });
                if let Some(cin) = carry {
                    let s2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add { dst: s2, lhs: s, rhs: cin });
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: c1, lhs: s, rhs: l, kind: CmpKind::LtU });
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: c2, lhs: s2, rhs: s, kind: CmpKind::LtU });
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or { dst: cout, lhs: c1, rhs: c2 });
                    carry = Some(cout);
                    dst_chunks.push((s2, 64));
                } else {
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp { dst: cout, lhs: s, rhs: l, kind: CmpKind::LtU });
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
            block.push(MInst::LoadImm { dst: zero, value: 0 });
            let result = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp { dst: result, lhs: is_nonzero, rhs: zero, kind: CmpKind::Eq });

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
            block.push(MInst::LoadImm { dst: all_ones, value: u64::MAX });
            let mut acc = all_ones;
            for i in 0..n_chunks {
                let c = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::And { dst: next, lhs: acc, rhs: c });
                acc = next;
            }
            let result = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp { dst: result, lhs: acc, rhs: all_ones, kind: CmpKind::Eq });
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
            block.push(MInst::LoadImm { dst: zero, value: 0 });
            let mut acc = zero;
            for i in 0..n_chunks {
                let c = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Xor { dst: next, lhs: acc, rhs: c });
                acc = next;
            }
            // Now acc has XOR of all chunks. Need popcount parity (odd # of 1-bits → 1)
            // Fold 64-bit value to 1 bit by cascading XOR
            let mut val = acc;
            for shift in [32u8, 16, 8, 4, 2, 1] {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm { dst: shifted, src: val, imm: shift });
                let folded = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Xor { dst: folded, lhs: val, rhs: shifted });
                val = folded;
            }
            let result = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::AndImm { dst: result, src: val, imm: 1 });
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
                block.push(MInst::Mov { dst: scalar, src: chunk0 });
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

        let main_vreg = if ci < n_src { src_chunks[ci].0 } else {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        };

        if is == 0 {
            if d_width < 64 {
                let mask = mask_for_width(d_width);
                block.push(MInst::AndImm { dst: dst_vreg, src: main_vreg, imm: mask });
            } else {
                block.push(MInst::Mov { dst: dst_vreg, src: main_vreg });
            }
        } else {
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm { dst: shifted, src: main_vreg, imm: is });

            // Carry from next chunk
            if (ci + 1) < n_src {
                let next_vreg = src_chunks[ci + 1].0;
                let carry = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShlImm { dst: carry, src: next_vreg, imm: 64 - is });
                let combined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or { dst: combined, lhs: shifted, rhs: carry });
                if d_width < 64 {
                    let mask = mask_for_width(d_width);
                    block.push(MInst::AndImm { dst: dst_vreg, src: combined, imm: mask });
                } else {
                    block.push(MInst::Mov { dst: dst_vreg, src: combined });
                }
            } else if d_width < 64 {
                let mask = mask_for_width(d_width);
                block.push(MInst::AndImm { dst: dst_vreg, src: shifted, imm: mask });
            } else {
                block.push(MInst::Mov { dst: dst_vreg, src: shifted });
            }
        }
    } else {
        // Non-constant: fall back to loading 0 (lossy but no crash)
        // TODO: implement runtime extraction when spilling is robust
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm { dst: zero, value: 0 });
        block.push(MInst::Mov { dst: dst_vreg, src: zero });
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
        block.push(MInst::ShlImm { dst: shifted_up, src, imm: shift });
        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm { dst: sign_extended, src: shifted_up, imm: shift });
        sign_extended
    };

    let sl = sign_extend_with_imm(ctx, block, lhs_vreg);
    let sr = sign_extend_with_imm(ctx, block, rhs_vreg);
    (sl, sr)
}

fn lower_terminator(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    term: &SIRTerminator,
) {
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
                true_bb: BlockId(true_block.0 .0 as u32),
                false_bb: BlockId(false_block.0 .0 as u32),
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
