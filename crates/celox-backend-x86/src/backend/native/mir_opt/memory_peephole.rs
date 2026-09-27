//! Contiguous memory access folding.

use super::*;

#[derive(Clone, Copy)]
struct ContiguousLoadPack {
    base: BaseReg,
    memory_bytes: [Option<i64>; 8],
    earliest_instruction: usize,
}

impl ContiguousLoadPack {
    fn from_load(base: BaseReg, offset: i32, size: OpSize, instruction: usize) -> Self {
        let mut memory_bytes = [None; 8];
        for (byte, slot) in memory_bytes
            .iter_mut()
            .enumerate()
            .take(size.bytes() as usize)
        {
            *slot = Some(i64::from(offset) + byte as i64);
        }
        Self {
            base,
            memory_bytes,
            earliest_instruction: instruction,
        }
    }

    fn shifted_left(self, bits: u8) -> Option<Self> {
        if !bits.is_multiple_of(8) || bits >= 64 {
            return None;
        }
        let bytes = usize::from(bits / 8);
        if self.memory_bytes[8 - bytes..].iter().any(Option::is_some) {
            return None;
        }
        let mut memory_bytes = [None; 8];
        memory_bytes[bytes..].copy_from_slice(&self.memory_bytes[..8 - bytes]);
        Some(Self {
            memory_bytes,
            ..self
        })
    }

    fn merged(self, other: Self) -> Option<Self> {
        if self.base != other.base {
            return None;
        }
        let mut memory_bytes = self.memory_bytes;
        for (destination, source) in memory_bytes.iter_mut().zip(other.memory_bytes) {
            if destination.is_some() && source.is_some() {
                return None;
            }
            if destination.is_none() {
                *destination = source;
            }
        }
        Some(Self {
            base: self.base,
            memory_bytes,
            earliest_instruction: self.earliest_instruction.min(other.earliest_instruction),
        })
    }

    fn native_load(self) -> Option<(i32, OpSize)> {
        for (byte_len, size) in [
            (8usize, OpSize::S64),
            (4usize, OpSize::S32),
            (2usize, OpSize::S16),
        ] {
            if self.memory_bytes[byte_len..].iter().any(Option::is_some) {
                continue;
            }
            let start = self.memory_bytes[0]?;
            if self.memory_bytes[..byte_len]
                .iter()
                .enumerate()
                .all(|(byte, address)| *address == Some(start + byte as i64))
            {
                return Some((i32::try_from(start).ok()?, size));
            }
        }
        None
    }
}

/// Replace little-endian packs of adjacent scalar loads with one native load.
///
/// Frontend Concat lowering commonly produces:
///
/// ```text
/// load.i8 [p+0] | (load.i8 [p+1] << 8) | ...
/// ```
///
/// Keeping those as separate SSA values creates seven unnecessary ALU
/// definitions and eight memory operations for one 64-bit value.  Summaries
/// are propagated forward once per instruction, so discovery is linear.  A
/// pack is materialized only if no memory write of any kind occurred between
/// its earliest scalar load and the root; this preserves the common memory
/// version without moving a read across an effect.
pub(super) fn fold_contiguous_load_packs(func: &mut MFunction) {
    for block in &mut func.blocks {
        let mut summaries = HashMap::<VReg, ContiguousLoadPack>::default();
        let mut last_write = None::<usize>;

        for instruction_index in 0..block.insts.len() {
            let original = block.insts[instruction_index].clone();
            let summary = match original {
                MInst::Load {
                    base, offset, size, ..
                } => Some(ContiguousLoadPack::from_load(
                    base,
                    offset,
                    size,
                    instruction_index,
                )),
                MInst::ShlImm { src, imm, .. } => summaries
                    .get(&src)
                    .copied()
                    .and_then(|summary| summary.shifted_left(imm)),
                MInst::Or { lhs, rhs, .. } => summaries
                    .get(&lhs)
                    .copied()
                    .zip(summaries.get(&rhs).copied())
                    .and_then(|(lhs, rhs)| lhs.merged(rhs)),
                MInst::Mov { src, .. } => summaries.get(&src).copied(),
                MInst::Mov32 { src, .. } => summaries
                    .get(&src)
                    .copied()
                    .filter(|summary| summary.memory_bytes[4..].iter().all(Option::is_none)),
                MInst::OrImm { src, imm: 0, .. } => summaries.get(&src).copied(),
                _ => None,
            };

            if let (Some(dst), Some(summary)) = (original.def(), summary) {
                if matches!(original, MInst::Or { .. })
                    && last_write.is_none_or(|write| write < summary.earliest_instruction)
                    && let Some((offset, size)) = summary.native_load()
                {
                    block.insts[instruction_index] = MInst::Load {
                        dst,
                        base: summary.base,
                        offset,
                        size,
                    };
                }
                summaries.insert(dst, summary);
            }

            if memory_effect::writes(&original).has_effect() {
                last_write = Some(instruction_index);
            }
        }
    }
}

/// Recover fixed-size memory copies from scalarized direct load/store pairs.
///
/// The source and destination ranges must be disjoint, each loaded SSA value
/// must be consumed only by its matching Store, and every pair must be
/// adjacent. These conditions make the replacement independent of alias and
/// scheduling speculation while removing one allocation value per chunk.
pub(super) fn fold_contiguous_memory_copies(func: &mut MFunction) {
    let mut use_counts = HashMap::<VReg, usize>::default();
    for block in &func.blocks {
        for phi in &block.phis {
            for (_, source) in &phi.sources {
                *use_counts.entry(*source).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                *use_counts.entry(source).or_default() += 1;
            }
        }
    }

    for block in &mut func.blocks {
        let original = std::mem::take(&mut block.insts);
        let mut rewritten = Vec::with_capacity(original.len());
        let mut cursor = 0usize;
        while cursor < original.len() {
            let Some((src_offset, dst_offset, size)) =
                direct_copy_pair(&original, cursor, &use_counts)
            else {
                rewritten.push(original[cursor].clone());
                cursor += 1;
                continue;
            };
            let chunk_bytes = size.bytes() as usize;
            let mut pairs = 1usize;
            while let Some((next_src, next_dst, next_size)) =
                direct_copy_pair(&original, cursor + pairs * 2, &use_counts)
            {
                let Some(delta) = pairs
                    .checked_mul(chunk_bytes)
                    .and_then(|delta| i32::try_from(delta).ok())
                else {
                    break;
                };
                if next_size != size
                    || src_offset.checked_add(delta) != Some(next_src)
                    || dst_offset.checked_add(delta) != Some(next_dst)
                {
                    break;
                }
                pairs += 1;
            }
            let byte_len = pairs * chunk_bytes;
            let src_end = i64::from(src_offset) + byte_len as i64;
            let dst_end = i64::from(dst_offset) + byte_len as i64;
            let disjoint = src_end <= i64::from(dst_offset) || dst_end <= i64::from(src_offset);
            if byte_len >= 16 && disjoint {
                rewritten.push(MInst::MemCopy {
                    src_offset,
                    dst_offset,
                    byte_len,
                });
                cursor += pairs * 2;
            } else {
                rewritten.push(original[cursor].clone());
                cursor += 1;
            }
        }
        block.insts = rewritten;
    }
}

fn direct_copy_pair(
    instructions: &[MInst],
    index: usize,
    use_counts: &HashMap<VReg, usize>,
) -> Option<(i32, i32, OpSize)> {
    let [
        MInst::Load {
            dst,
            base: BaseReg::SimState,
            offset: src_offset,
            size,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: dst_offset,
            src,
            size: store_size,
        },
    ] = instructions.get(index..index.checked_add(2)?)?
    else {
        return None;
    };
    (*src == *dst && *store_size == *size && use_counts.get(dst).copied() == Some(1)).then_some((
        *src_offset,
        *dst_offset,
        *size,
    ))
}
