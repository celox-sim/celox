//! Recover bulk copies before their scalar temporaries reach allocation.

use std::collections::BTreeMap;

use crate::mir::{BaseReg, MFunction, MInst, OpSize, VReg};

pub(super) fn fold(function: &mut MFunction) {
    let uses = super::use_counts(function);
    for block in &mut function.blocks {
        let original = std::mem::take(&mut block.insts);
        let mut cursor = 0;
        while cursor < original.len() {
            if let Some((count, copy)) = copy_run(&original[cursor..], &uses) {
                block.insts.push(copy);
                cursor += count;
            } else {
                block.insts.push(original[cursor].clone());
                cursor += 1;
            }
        }
    }
}

fn load(inst: &MInst, uses: &BTreeMap<VReg, usize>) -> Option<(VReg, i32, OpSize)> {
    match *inst {
        MInst::Load {
            dst,
            base: BaseReg::SimState,
            offset,
            size,
        } if uses.get(&dst) == Some(&1) => Some((dst, offset, size)),
        _ => None,
    }
}

fn store(inst: &MInst, value: VReg, size: OpSize) -> Option<i32> {
    match *inst {
        MInst::Store {
            base: BaseReg::SimState,
            offset,
            src,
            size: stored_size,
        } if src == value && stored_size == size => Some(offset),
        _ => None,
    }
}

fn copy_run(insts: &[MInst], uses: &BTreeMap<VReg, usize>) -> Option<(usize, MInst)> {
    let (first, src_offset, size) = load(insts.first()?, uses)?;
    let chunk = i32::from(size.bytes());
    // Selection produces either interleaved load/store pairs or a complete
    // snapshot followed by its stores. Do not move other operations across
    // either form, and retain any loaded value that has another consumer.
    let interleaved = store(insts.get(1)?, first, size).is_some();
    let stride = if interleaved { 2 } else { 1 };
    let mut count = 1usize;
    // This is the emitter's unrolled vector-copy range. Bounding each scan
    // also avoids quadratic searches through large, nonmatching load runs.
    while count < 256 / usize::from(size.bytes())
        && let Some((_, offset, next_size)) =
            insts.get(count * stride).and_then(|inst| load(inst, uses))
    {
        let delta = i32::try_from(count).ok()?.checked_mul(chunk)?;
        if next_size != size || src_offset.checked_add(delta) != Some(offset) {
            break;
        }
        if interleaved {
            let (value, _, _) = load(&insts[count * 2], uses)?;
            let previous = store(&insts[1], first, size)?;
            if insts
                .get(count * 2 + 1)
                .and_then(|inst| store(inst, value, size))
                != previous.checked_add(delta)
            {
                break;
            }
        }
        count += 1;
    }
    let dst_offset = store(insts.get(if interleaved { 1 } else { count })?, first, size)?;
    if !interleaved {
        for index in 1..count {
            let (value, _, _) = load(&insts[index], uses)?;
            let delta = i32::try_from(index).ok()?.checked_mul(chunk)?;
            if store(insts.get(count + index)?, value, size) != dst_offset.checked_add(delta) {
                return None;
            }
        }
    }
    let byte_len = count.checked_mul(usize::from(size.bytes()))?;
    if byte_len < 16 {
        return None;
    }
    let src_end = i64::from(src_offset).checked_add(i64::try_from(byte_len).ok()?)?;
    let dst_end = i64::from(dst_offset).checked_add(i64::try_from(byte_len).ok()?)?;
    // An interleaved overlapping copy can intentionally propagate a prior
    // store into a later load. Only a full snapshot has memmove semantics.
    if interleaved && src_end > i64::from(dst_offset) && dst_end > i64::from(src_offset) {
        return None;
    }
    Some((
        count * 2,
        MInst::MemCopy {
            src_offset,
            dst_offset,
            byte_len,
        },
    ))
}
