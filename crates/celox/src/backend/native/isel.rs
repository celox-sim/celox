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

    // Update spill_descs to match final vreg count
    while func.spill_descs.len() < func.vregs.count() as usize {
        func.spill_descs.push(SpillDesc::transient());
    }

    func
}

struct ISelContext<'a> {
    vregs: &'a mut VRegAllocator,
    spill_descs: &'a mut Vec<SpillDesc>,
    reg_map: &'a mut RegMap,
    register_types: &'a crate::HashMap<RegisterId, RegisterType>,
    layout: &'a MemoryLayout,
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
}

fn lower_instruction(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) {
    match inst {
        SIRInstruction::Imm(dst, val) => {
            let vreg = ctx.reg_map.get(*dst);
            // Extract u64 from BigUint (2-state only)
            let digits = val.payload.to_u64_digits();
            let imm_val = digits.first().copied().unwrap_or(0);

            // Update spill desc to rematerializable
            ctx.spill_descs[vreg.0 as usize] = SpillDesc::remat(imm_val);

            block.push(MInst::LoadImm {
                dst: vreg,
                value: imm_val,
            });
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
                        // Word-aligned, native size: single load
                        block.push(MInst::Load {
                            dst: vreg,
                            base: BaseReg::SimState,
                            offset: byte_off,
                            size: op_size,
                        });
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
                            let mask = (1u64 << width_bits) - 1;
                            block.push(MInst::AndImm {
                                dst: vreg,
                                src: shifted,
                                imm: mask,
                            });
                        } else {
                            // Byte-aligned but non-native width: just mask
                            let mask = (1u64 << width_bits) - 1;
                            block.push(MInst::AndImm {
                                dst: vreg,
                                src: tmp,
                                imm: mask,
                            });
                        }
                    }
                }
                SIROffset::Dynamic(_offset_reg) => {
                    // TODO: dynamic offset support
                    unimplemented!("dynamic offset load not yet supported in native backend");
                }
            }
        }

        SIRInstruction::Store(addr, offset, width_bits, src_reg, _triggers) => {
            let src_vreg = ctx.reg_map.get(*src_reg);

            // Mark the source as store-back-only if it was loaded from the same place
            // (optimization: done in a later pass or can be detected here)

            match offset {
                SIROffset::Static(bit_off) => {
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
                        let containing_byte_off = ctx.byte_offset(addr, 0) + (bit_off / 8) as i32;
                        let load_size = ISelContext::op_size_for_width(*width_bits + intra_byte);

                        let old_word = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: old_word,
                            base: BaseReg::SimState,
                            offset: containing_byte_off,
                            size: load_size,
                        });

                        let mask = (1u64 << width_bits) - 1;
                        let new_word = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitFieldInsert {
                            dst: new_word,
                            base_word: old_word,
                            val: src_vreg,
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
                SIROffset::Dynamic(_) => {
                    unimplemented!("dynamic offset store not yet supported in native backend");
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
                SIROffset::Dynamic(_) => {
                    unimplemented!("dynamic offset commit not yet supported");
                }
            }
        }

        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let dst_vreg = ctx.reg_map.get(*dst);
            let lhs_vreg = ctx.reg_map.get(*lhs);
            let rhs_vreg = ctx.reg_map.get(*rhs);
            let d_width = ctx.sir_width(dst);

            match op {
                BinaryOp::Add => block.push(MInst::Add {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                }),
                BinaryOp::Sub => block.push(MInst::Sub {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                }),
                BinaryOp::Mul => block.push(MInst::Mul {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                }),
                BinaryOp::And => block.push(MInst::And {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                }),
                BinaryOp::Or => block.push(MInst::Or {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                }),
                BinaryOp::Xor => block.push(MInst::Xor {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                }),
                BinaryOp::Shr => {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Shr {
                        dst: shifted,
                        lhs: lhs_vreg,
                        rhs: rhs_vreg,
                    });
                    // Mask to destination width
                    if d_width < 64 {
                        let mask = (1u64 << d_width) - 1;
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
                        let mask = (1u64 << d_width) - 1;
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
                BinaryOp::Sar => block.push(MInst::Sar {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                }),
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
                BinaryOp::LtS => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::LtS,
                }),
                BinaryOp::LeU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::LeU,
                }),
                BinaryOp::LeS => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::LeS,
                }),
                BinaryOp::GtU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GtU,
                }),
                BinaryOp::GtS => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GtS,
                }),
                BinaryOp::GeU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GeU,
                }),
                BinaryOp::GeS => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GeS,
                }),
                BinaryOp::Div => {
                    // div with zero guard: dst = rhs == 0 ? 0 : lhs / rhs
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    let one = ctx.alloc_vreg(SpillDesc::remat(1));
                    block.push(MInst::LoadImm {
                        dst: one,
                        value: 1,
                    });
                    let is_zero = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: is_zero,
                        lhs: rhs_vreg,
                        rhs: zero,
                        kind: CmpKind::Eq,
                    });
                    // Use 1 as safe divisor when rhs is 0
                    let safe_rhs = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: safe_rhs,
                        cond: is_zero,
                        true_val: one,
                        false_val: rhs_vreg,
                    });
                    // TODO: Div MIR instruction not yet defined, using Sub as placeholder
                    // This needs a proper UDiv instruction in MInst
                    let _ = (dst_vreg, lhs_vreg, safe_rhs);
                    unimplemented!("div instruction not yet in MInst");
                }
                BinaryOp::Rem => {
                    unimplemented!("rem instruction not yet in MInst");
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
                    block.push(MInst::BitNot {
                        dst: dst_vreg,
                        src: src_vreg,
                    });
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
                        (1u64 << width) - 1
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
                    // TODO: proper popcount. For now, use a simple XOR fold?
                    // This is correct but not optimal for wide values.
                    // For ≤64 bit, we can use popcnt instruction later.
                    unimplemented!("reduction XOR not yet supported in native backend");
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
                // Wide concat (>64 bits): emit chunk-by-chunk stores
                // This is for cases like Concat → Store(320 bits)
                // Defer to Store lowering which should handle Concat sources
                // For now, not supported standalone
                unimplemented!("wide concat (>{} bits) not yet supported standalone", 64);
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
                    let mask = (1u64 << width) - 1;
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
                    let mask = (1u64 << width) - 1;
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

fn lower_terminator(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    term: &SIRTerminator,
) {
    match term {
        SIRTerminator::Jump(target, _args) => {
            // Block arguments are handled via phi-like register mapping
            // For now, ignore block args (they become mov's or are already handled)
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
        SIRTerminator::Error(_code) => {
            // TODO: error handling
            block.push(MInst::Return);
        }
    }
}
