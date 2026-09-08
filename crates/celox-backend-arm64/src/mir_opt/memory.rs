//! Keep short packed-state update chains in registers. Reuse is local and
//! bounded so eliminating a load does not create a long-lived spill candidate.

use crate::HashMap;
use crate::mir::{BaseReg, MFunction, MInst, OpSize, VReg};

fn narrow_masked_loads(function: &mut MFunction) {
    let mut uses = HashMap::<VReg, usize>::default();
    for block in &function.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                *uses.entry(source).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                *uses.entry(source).or_default() += 1;
            }
        }
    }
    for block in &mut function.blocks {
        let mut loads = HashMap::default();
        for index in 0..block.insts.len() {
            let (source, lsb, width) = match block.insts[index] {
                MInst::Load {
                    dst,
                    base: BaseReg::SimState,
                    ..
                }
                | MInst::LoadIndexed {
                    dst,
                    base: BaseReg::SimState,
                    ..
                } => {
                    loads.insert(dst, index);
                    continue;
                }
                MInst::BitExtract {
                    src, lsb, width, ..
                } => (src, lsb, width),
                MInst::AndImm { src, imm, .. } => (src, 0, (64 - imm.leading_zeros()) as u8),
                MInst::AndImm32 { src, imm, .. } => (src, 0, (32 - imm.leading_zeros()) as u8),
                _ => continue,
            };
            if uses.get(&source) != Some(&1) || width == 0 {
                continue;
            }
            let Some(&load_index) = loads.get(&source) else {
                continue;
            };
            let byte_delta = lsb / 8;
            let remaining_lsb = lsb % 8;
            let Some(new_size) = [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64]
                .into_iter()
                .find(|size| remaining_lsb + width <= size.bytes() * 8)
            else {
                continue;
            };
            let (offset, size) = match &mut block.insts[load_index] {
                MInst::Load { offset, size, .. } | MInst::LoadIndexed { offset, size, .. } => {
                    (offset, size)
                }
                _ => unreachable!(),
            };
            // Rounding the field to a machine load must never read beyond
            // the original range. In particular, a shifted 56-bit field
            // cannot become an eight-byte read starting one byte later.
            if byte_delta + new_size.bytes() > size.bytes() {
                continue;
            }
            let Some(new_offset) = offset.checked_add(i32::from(byte_delta)) else {
                continue;
            };
            *offset = new_offset;
            *size = new_size;
            if let MInst::BitExtract { lsb, .. } = &mut block.insts[index] {
                *lsb = remaining_lsb;
            }
        }
    }
}

struct AvailableValue {
    start: i64,
    end: i64,
    source: VReg,
    position: usize,
    zero_extended: bool,
}

pub(super) fn run(function: &mut MFunction) {
    narrow_masked_loads(function);
    const MAX_REUSE_DISTANCE: usize = 16;
    for block in &mut function.blocks {
        let mut available = Vec::<AvailableValue>::new();
        for (position, inst) in block.insts.iter_mut().enumerate() {
            available.retain(|value| position - value.position <= MAX_REUSE_DISTANCE);
            match *inst {
                MInst::Load {
                    dst,
                    base: BaseReg::SimState,
                    offset,
                    size,
                } => {
                    let start = i64::from(offset);
                    let end = start + i64::from(size.bytes());
                    if let Some(value) = available
                        .iter()
                        .rev()
                        .find(|value| value.start <= start && end <= value.end)
                    {
                        *inst = if start == value.start
                            && (size == OpSize::S64 || (value.zero_extended && end == value.end))
                        {
                            MInst::Mov {
                                dst,
                                src: value.source,
                            }
                        } else {
                            MInst::BitExtract {
                                dst,
                                src: value.source,
                                lsb: ((start - value.start) * 8) as u8,
                                width: size.bytes() * 8,
                            }
                        };
                        // Keep the original reuse deadline, even across
                        // repeated loads of this value.
                    } else {
                        available.push(AvailableValue {
                            start,
                            end,
                            source: dst,
                            position,
                            zero_extended: true,
                        });
                    }
                }
                MInst::Store {
                    base: BaseReg::SimState,
                    offset,
                    src,
                    size,
                } => {
                    let start = i64::from(offset);
                    let end = start + i64::from(size.bytes());
                    available.retain(|value| value.end <= start || end <= value.start);
                    available.push(AvailableValue {
                        start,
                        end,
                        source: src,
                        position,
                        zero_extended: false,
                    });
                }
                MInst::KeepAlive { .. } => {}
                // All other writes (including indexed/pointer accesses and
                // bulk or sparse updates) invalidate the cache. Using def()
                // also makes future side-effecting opcodes conservative.
                _ if inst.def().is_none() => available.clear(),
                _ => {}
            }
        }
    }
}

/// Forwarding can expose writes that are completely overwritten before any
/// read. Remove only fixed state stores with a nearby, covering replacement.
pub(super) fn eliminate_overwritten_stores(function: &mut MFunction) {
    for block in &mut function.blocks {
        let mut pending = Vec::<(i64, i64, usize)>::new();
        let mut dead = vec![false; block.insts.len()];
        for (position, inst) in block.insts.iter().enumerate() {
            pending.retain(|&(_, _, store)| position - store <= 16);
            match *inst {
                MInst::Store {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => {
                    let start = i64::from(offset);
                    let end = start + i64::from(size.bytes());
                    pending.retain(|&(old_start, old_end, store)| {
                        let covered = start <= old_start && old_end <= end;
                        dead[store] |= covered;
                        !covered
                    });
                    pending.push((start, end, position));
                }
                MInst::Load {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => {
                    let start = i64::from(offset);
                    let end = start + i64::from(size.bytes());
                    pending.retain(|&(old_start, old_end, _)| old_end <= start || end <= old_start);
                }
                MInst::Load { .. }
                | MInst::LoadIndexed { .. }
                | MInst::LoadPtr { .. }
                | MInst::LoadPtrIndexed { .. }
                | MInst::PackedLaneCompare { .. } => pending.clear(),
                MInst::KeepAlive { .. } => {}
                _ if inst.def().is_none() => pending.clear(),
                _ => {}
            }
        }
        let mut index = 0;
        block.insts.retain(|_| {
            let retain = !dead[index];
            index += 1;
            retain
        });
    }
}
