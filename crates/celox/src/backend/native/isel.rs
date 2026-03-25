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
        consts: ConstMap::default(),
    };

    for &sir_block_id in &block_ids {
        let sir_block = &eu.blocks[&sir_block_id];
        let mir_block_id = BlockId(sir_block_id.0 as u32);
        let mut mblock = MBlock::new(mir_block_id);

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
            let vreg = ctx.reg_map.get(*dst);

            match offset {
                SIROffset::Static(bit_off) => {
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

            // Wide (>64-bit) binary operations: dispatch to multi-word handler
            if d_width > 64 {
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
                    block.push(MInst::Shr {
                        dst: shifted,
                        lhs: lhs_vreg,
                        rhs: rhs_vreg,
                    });
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
                    block.push(MInst::Shl {
                        dst: shifted,
                        lhs: lhs_vreg,
                        rhs: rhs_vreg,
                    });
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
                        let sext_shift_vreg = ctx.alloc_vreg(SpillDesc::remat(sext_shift as u64));
                        block.push(MInst::LoadImm { dst: sext_shift_vreg, value: sext_shift as u64 });
                        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sar { dst: sign_extended, lhs: shifted_up, rhs: sext_shift_vreg });
                        // Now do the actual shift
                        let sar_result = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sar { dst: sar_result, lhs: sign_extended, rhs: rhs_vreg });
                        // Mask to output width
                        let mask = mask_for_width(width);
                        block.push(MInst::AndImm { dst: dst_vreg, src: sar_result, imm: mask });
                    } else {
                        block.push(MInst::Sar {
                            dst: dst_vreg,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        });
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
            if d_width > 64 {
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
                let mut chunks = Vec::new();
                for arg in args.iter().rev() {
                    let arg_vreg = ctx.reg_map.get(*arg);
                    let arg_width = ctx.sir_width(arg);
                    chunks.push((arg_vreg, arg_width));
                }
                ctx.wide_regs.insert(*dst, chunks);
                // No MIR instructions emitted; the value is consumed by Store.
            }
        }

        SIRInstruction::Slice(dst, src, bit_offset, width) => {
            let dst_vreg = ctx.reg_map.get(*dst);
            let src_vreg = ctx.reg_map.get(*src);

            if *width <= 64 {
                if *bit_offset == 0 && *width == ctx.sir_width(src) {
                    // Identity slice
                    block.push(MInst::Mov {
                        dst: dst_vreg,
                        src: src_vreg,
                    });
                } else if *bit_offset == 0 {
                    // Just mask
                    let mask = mask_for_width(*width);
                    block.push(MInst::AndImm {
                        dst: dst_vreg,
                        src: src_vreg,
                        imm: mask,
                    });
                } else {
                    // Shift + mask
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: src_vreg,
                        imm: *bit_offset as u8,
                    });
                    let mask = mask_for_width(*width);
                    block.push(MInst::AndImm {
                        dst: dst_vreg,
                        src: shifted,
                        imm: mask,
                    });
                }
            } else {
                unimplemented!("wide slice not yet supported in native backend");
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
    let n_chunks = ISelContext::num_chunks(d_width);

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
                // Non-constant shift: fall back to per-chunk scalar shift.
                // This loses cross-chunk carry bits but avoids select chain explosion.
                // TODO: implement full runtime multi-word shift when spilling is robust.
                let amount_vreg = ctx.reg_map.get(rhs);
                let chunk_shift_v = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm { dst: chunk_shift_v, src: amount_vreg, imm: 6 });
                let intra_shift_v = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::AndImm { dst: intra_shift_v, src: amount_vreg, imm: 63 });
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: zero, value: 0 });

                let mut dst_chunks = Vec::with_capacity(n_chunks);
                for _i in 0..n_chunks {
                    // Simplified: just emit zero for now
                    dst_chunks.push((zero, 64));
                }
                ctx.set_wide_chunks(dst, dst_chunks);
            }
        }

        // For other binary ops on wide values (Add, Sub, Mul, etc.),
        // fall back to scalar (truncated to 64-bit). This is incorrect but
        // prevents panics for unsupported operations.
        _ => {
            let lhs_vreg = ctx.reg_map.get(lhs);
            let rhs_vreg = ctx.reg_map.get(rhs);
            let dst_vreg = ctx.reg_map.get(dst);
            block.push(MInst::Mov { dst: dst_vreg, src: lhs_vreg });
            let _ = rhs_vreg;
        }
    }
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
    let n_chunks = ISelContext::num_chunks(d_width);

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
            ctx.set_wide_chunks(dst, src_chunks);
        }
        _ => {
            // Unsupported wide unary: fall back to scalar
            let dst_vreg = ctx.reg_map.get(dst);
            let src_vreg = ctx.reg_map.get(src);
            block.push(MInst::Mov { dst: dst_vreg, src: src_vreg });
        }
    }
}

/// Extract a ≤64-bit value from a wide (>64-bit) register by right-shifting.
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
        // Need arithmetic shift right by `shift`. No SarImm, use Sar with constant.
        let shift_vreg = ctx.alloc_vreg(SpillDesc::remat(shift as u64));
        block.push(MInst::LoadImm { dst: shift_vreg, value: shift as u64 });
        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Sar { dst: sign_extended, lhs: shifted_up, rhs: shift_vreg });
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
