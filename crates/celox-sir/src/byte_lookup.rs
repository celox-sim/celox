//! Two-state reads of an aligned byte from a bounded packed constant.
use std::borrow::Cow;

use super::*;

/// The low `width` bits of the byte at `offset / 8`. The SIR shift count is a
/// bit offset, with three known zero low bits. Its entire domain fits the table.
pub struct ByteLookup<'a> {
    pub table: RegisterId,
    pub shifted: RegisterId,
    pub offset: RegisterId,
    pub offset_width: usize,
    pub width: usize,
    payload: &'a BigUint,
}

impl ByteLookup<'_> {
    pub fn bytes(&self) -> Vec<u8> {
        let mut bytes = self.payload.to_bytes_le();
        bytes.resize(1 << (self.offset_width - 3), 0);
        let mask = u8::MAX >> (8 - self.width);
        for byte in &mut bytes {
            *byte &= mask;
        }
        bytes
    }
}

/// Find exact tables on paths where the caller has selected two-state evaluation.
pub fn collect<A>(unit: &ExecutionUnit<A>) -> HashMap<RegisterId, ByteLookup<'_>> {
    let mut found = HashMap::default();
    if !unit
        .register_map
        .values()
        .any(|ty| (128..=8192).contains(&ty.width()) && ty.width().is_power_of_two())
    {
        return found;
    }
    let mut defs = HashMap::default();
    for block in unit.blocks.values() {
        for &param in &block.params {
            defs.insert(param, None);
        }
        for inst in &block.instructions {
            if let Some(dst) = inst.defined_register() {
                defs.entry(dst)
                    .and_modify(|d| *d = None)
                    .or_insert(Some(inst));
            }
        }
    }
    let definition = |reg: &RegisterId| defs.get(reg).copied().flatten();
    for (&dst, &instruction) in &defs {
        let Some(SIRInstruction::Slice(_, shifted, 0, width @ 1..=8)) = instruction else {
            continue;
        };
        let Some(SIRInstruction::Binary(_, table, BinaryOp::Shr, offset)) = definition(shifted)
        else {
            continue;
        };
        let Some(SIRInstruction::Imm(_, bits)) = definition(table) else {
            continue;
        };
        let Some(SIRInstruction::Concat(_, parts)) = definition(offset) else {
            continue;
        };
        let Some(last) = parts.last() else { continue };
        let offset_width = unit.register_map[offset].width();
        if !(7..=13).contains(&offset_width)
            || unit.register_map[table].width() != 1 << offset_width
            || unit.register_map[shifted].width() != 1 << offset_width
            || unit.register_map[&dst].width() != *width
            || !bits.mask.is_zero()
            || unit.register_map[last].width() != 3
            || !matches!(definition(last), Some(SIRInstruction::Imm(_,zero)) if zero.mask.is_zero() && zero.payload.is_zero())
        {
            continue;
        }
        found.insert(
            dst,
            ByteLookup {
                table: *table,
                shifted: *shifted,
                offset: *offset,
                offset_width,
                width: *width,
                payload: &bits.payload,
            },
        );
    }
    found
}

// Each 64-entry page supplies one machine word per output bit. The low six
// address bits select a bit in the word. The remaining bits select a page.
fn emit_scalar<A>(lookup: &ByteLookup<'_>, dst: RegisterId, b: &mut SIRBuilder<A>) {
    let index_width = lookup.offset_width - 3;
    let low_width = index_width.min(6);
    let low = b.alloc_bit(low_width, false);
    b.emit(SIRInstruction::Slice(low, lookup.offset, 3, low_width));
    let mut selectors = Vec::new();
    for bit in low_width..index_width {
        let select = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Slice(select, lookup.offset, 3 + bit, 1));
        selectors.push(select);
    }
    let bytes = lookup.bytes();
    let mut constants = HashMap::default();
    let mut bits = Vec::new();
    for bit in (0..lookup.width).rev() {
        let mut pages = Vec::new();
        for page in bytes.chunks(1 << low_width) {
            let word = page
                .iter()
                .enumerate()
                .fold(0u64, |word, (i, v)| word | (u64::from((v >> bit) & 1) << i));
            pages.push(*constants.entry(word).or_insert_with(|| {
                let imm = b.alloc_bit(64, false);
                b.emit(SIRInstruction::Imm(imm, SIRValue::new(word)));
                imm
            }));
        }
        for &selector in &selectors {
            pages = pages
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| {
                    if p[0] == p[1] {
                        return p[0];
                    }
                    let next = b.alloc_bit(64, false);
                    b.emit(SIRInstruction::Mux(next, selector, p[1], p[0]));
                    next
                })
                .collect();
        }
        let shifted = b.alloc_bit(64, false);
        b.emit(SIRInstruction::Binary(
            shifted,
            pages[0],
            BinaryOp::Shr,
            low,
        ));
        let bit = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Slice(bit, shifted, 0, 1));
        bits.push(bit);
    }
    b.emit(SIRInstruction::Concat(dst, bits));
}

/// Legalize byte lookups for targets without a constant-pool load. No operation
/// in the replacement is wider than 64 bits, even for a 1024-entry table.
pub fn scalarize<A: Clone>(unit: &ExecutionUnit<A>, four_state: bool) -> Cow<'_, ExecutionUnit<A>> {
    if four_state {
        return Cow::Borrowed(unit);
    }
    let lookups = collect(unit);
    if lookups.is_empty() {
        return Cow::Borrowed(unit);
    }
    let uses = analysis::collect_use_sites(unit);
    let all_users_in = |reg: RegisterId, removed: &HashSet<RegisterId>| {
        uses[&reg].iter().all(|site| {
            site.inst_idx
                .and_then(|i| unit.blocks[&site.block].instructions[i].defined_register())
                .is_some_and(|user| removed.contains(&user))
        })
    };
    let replaced: HashSet<_> = lookups.keys().copied().collect();
    let mut dead: HashSet<_> = lookups
        .values()
        .map(|l| l.shifted)
        .filter(|&shifted| all_users_in(shifted, &replaced))
        .collect();
    let dead_tables: Vec<_> = lookups
        .values()
        .map(|l| l.table)
        .filter(|&table| all_users_in(table, &dead))
        .collect();
    dead.extend(dead_tables);
    let mut out = unit.clone();
    out.register_map.retain(|r, _| !dead.contains(r));
    let mut next = unit.register_map.keys().map(|r| r.0).max().unwrap() + 1;
    for block in out.blocks.values_mut() {
        for inst in std::mem::take(&mut block.instructions) {
            if let Some(dst) = inst.defined_register() {
                if dead.contains(&dst) {
                    continue;
                }
                if let Some(lookup) = lookups.get(&dst) {
                    let mut b = SIRBuilder::new_with(next);
                    emit_scalar(lookup, dst, &mut b);
                    let id = b.seal_block(SIRTerminator::Return);
                    let (mut blocks, regs, end) = b.drain();
                    block
                        .instructions
                        .append(&mut blocks.get_mut(&id).unwrap().instructions);
                    out.register_map.extend(regs);
                    next = end;
                    continue;
                }
            }
            block.instructions.push(inst);
        }
    }
    Cow::Owned(out)
}

/// Prepare a group before Cranelift estimates its size or splits it into chunks.
pub fn scalarize_units<A: Clone>(
    units: &[ExecutionUnit<A>],
    four_state: bool,
) -> Cow<'_, [ExecutionUnit<A>]> {
    let mut result = Cow::Borrowed(units);
    for (i, unit) in units.iter().enumerate() {
        if let Cow::Owned(lowered) = scalarize(unit, four_state) {
            result.to_mut()[i] = lowered;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalarization_preserves_other_consumers_and_requires_an_exact_aligned_table() {
        let mut b = SIRBuilder::new();
        let index = b.alloc_bit(4, false);
        let zero = b.alloc_bit(3, false);
        let offset = b.alloc_bit(7, false);
        let table = b.alloc_bit(128, false);
        let shifted = b.alloc_bit(128, false);
        let result = b.alloc_bit(4, false);
        let other = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Load(index, 0u32, SIROffset::Static(0), 4));
        b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        b.emit(SIRInstruction::Concat(offset, vec![index, zero]));
        b.emit(SIRInstruction::Imm(table, SIRValue::new(0x4321u16)));
        b.emit(SIRInstruction::Binary(
            shifted,
            table,
            BinaryOp::Shr,
            offset,
        ));
        b.emit(SIRInstruction::Slice(result, shifted, 0, 4));
        b.emit(SIRInstruction::Slice(other, shifted, 4, 8));
        let entry_block_id = b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        let unit = ExecutionUnit {
            entry_block_id,
            blocks,
            register_map,
        };
        let lowered = scalarize(&unit, false);
        assert!(matches!(lowered, Cow::Owned(_)));
        assert!(lowered.register_map.contains_key(&table));
        assert!(lowered.register_map.contains_key(&shifted));
        lowered.verify();
        assert!(matches!(scalarize(&unit, true), Cow::Borrowed(_)));
        for (reg, value) in [
            (zero, SIRValue::new(1u8)),
            (table, SIRValue::new_four_state(7u8, 1u8)),
        ] {
            let mut unit = unit.clone();
            let i = unit
                .blocks
                .get_mut(&entry_block_id)
                .unwrap()
                .instructions
                .iter_mut()
                .find(|i| i.defined_register() == Some(reg))
                .unwrap();
            *i = SIRInstruction::Imm(reg, value);
            assert!(collect(&unit).is_empty());
        }
    }
}
