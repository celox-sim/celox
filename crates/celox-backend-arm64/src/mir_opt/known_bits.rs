//! Bit facts for machine-width SSA arithmetic, including packed array offsets.

use std::collections::VecDeque;

use crate::HashMap;
use crate::mir::{CmpKind, MFunction, MInst, OpSize, SpillDesc, VReg};

#[derive(Clone, Copy)]
enum ValueDefinition {
    Phi { block: usize, phi: usize },
    Instruction { block: usize, instruction: usize },
}

fn machine_width_mask(size: OpSize) -> u64 {
    u64::MAX >> (64 - u32::from(size.bytes()) * 8)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct KnownBits {
    zero: u64,
    one: u64,
}

impl KnownBits {
    fn constant(value: u64) -> Self {
        Self {
            zero: !value,
            one: value,
        }
    }

    fn value(self) -> Option<u64> {
        (self.zero | self.one == u64::MAX).then_some(self.one)
    }

    fn truth(self) -> Option<bool> {
        if self.one != 0 {
            Some(true)
        } else {
            (self.zero == u64::MAX).then_some(false)
        }
    }

    fn compare(self, other: Self, kind: CmpKind) -> Self {
        if let (Some(lhs), Some(rhs)) = (self.value(), other.value()) {
            let value = match kind {
                CmpKind::Eq => lhs == rhs,
                CmpKind::Ne => lhs != rhs,
                CmpKind::LtU => lhs < rhs,
                CmpKind::LeU => lhs <= rhs,
                CmpKind::GtU => lhs > rhs,
                CmpKind::GeU => lhs >= rhs,
                CmpKind::LtS => (lhs as i64) < (rhs as i64),
                CmpKind::LeS => (lhs as i64) <= (rhs as i64),
                CmpKind::GtS => (lhs as i64) > (rhs as i64),
                CmpKind::GeS => (lhs as i64) >= (rhs as i64),
            };
            return Self::constant(u64::from(value));
        }
        if (self.zero & other.one) | (self.one & other.zero) != 0 {
            match kind {
                CmpKind::Eq => return Self::constant(0),
                CmpKind::Ne => return Self::constant(1),
                _ => {}
            }
        }
        let signed = matches!(
            kind,
            CmpKind::LtS | CmpKind::LeS | CmpKind::GtS | CmpKind::GeS
        );
        // Flipping the sign bit converts signed order to unsigned order,
        // including a range whose sign is not known yet.
        let (left, right) = if signed {
            let sign = Self::constant(1 << 63);
            (self.xor(sign), other.xor(sign))
        } else {
            (self, other)
        };
        let (min_left, max_left) = (left.one, !left.zero);
        let (min_right, max_right) = (right.one, !right.zero);
        let value = match kind {
            CmpKind::LtU | CmpKind::LtS if max_left < min_right => Some(true),
            CmpKind::LtU | CmpKind::LtS if min_left >= max_right => Some(false),
            CmpKind::LeU | CmpKind::LeS if max_left <= min_right => Some(true),
            CmpKind::LeU | CmpKind::LeS if min_left > max_right => Some(false),
            CmpKind::GtU | CmpKind::GtS if min_left > max_right => Some(true),
            CmpKind::GtU | CmpKind::GtS if max_left <= min_right => Some(false),
            CmpKind::GeU | CmpKind::GeS if min_left >= max_right => Some(true),
            CmpKind::GeU | CmpKind::GeS if max_left < min_right => Some(false),
            _ => None,
        };
        if let Some(value) = value {
            return Self::constant(u64::from(value));
        }
        Self::default().truncate(1)
    }

    fn truncate(self, mask: u64) -> Self {
        Self {
            zero: self.zero | !mask,
            one: self.one & mask,
        }
    }

    fn join(self, other: Self) -> Self {
        Self {
            zero: self.zero & other.zero,
            one: self.one & other.one,
        }
    }

    fn not(self) -> Self {
        Self {
            zero: self.one,
            one: self.zero,
        }
    }

    fn and(self, other: Self) -> Self {
        Self {
            zero: self.zero | other.zero,
            one: self.one & other.one,
        }
    }

    fn or(self, other: Self) -> Self {
        Self {
            zero: self.zero & other.zero,
            one: self.one | other.one,
        }
    }

    fn xor(self, other: Self) -> Self {
        Self {
            zero: (self.zero & other.zero) | (self.one & other.one),
            one: (self.zero & other.one) | (self.one & other.zero),
        }
    }

    fn add(self, other: Self) -> Self {
        // A carry is monotone in the lower input bits. Evaluate the minimum
        // and maximum inputs to find which carry-in bits are fixed, then XOR
        // those with the fixed operand bits. This also preserves u64 wrapping.
        let min_sum = self.one.wrapping_add(other.one);
        let max_sum = (!self.zero).wrapping_add(!other.zero);
        let min_carry = min_sum ^ self.one ^ other.one;
        let max_carry = max_sum ^ !self.zero ^ !other.zero;
        let known = (self.zero | self.one) & (other.zero | other.one) & !(min_carry ^ max_carry);
        Self {
            zero: !min_sum & known,
            one: min_sum & known,
        }
    }

    fn mul(self, other: Self) -> Self {
        // Split each operand into a fixed low part and an unknown multiple
        // of a power of two. All three terms involving an unknown part are
        // multiples of 2^known_low, even when multiplication wraps.
        let left_unknown = (!(self.zero | self.one)).trailing_zeros();
        let right_unknown = (!(other.zero | other.one)).trailing_zeros();
        let known_low = (left_unknown + other.one.trailing_zeros())
            .min(right_unknown + self.one.trailing_zeros())
            .min(left_unknown + right_unknown)
            .min(64);
        let low_mask = u64::MAX.checked_shr(64 - known_low).unwrap_or(0);
        let product = self.one.wrapping_mul(other.one);
        let mut zero = !product & low_mask;
        if let Some(max) = (!self.zero).checked_mul(!other.zero) {
            zero |= u64::MAX.checked_shl(64 - max.leading_zeros()).unwrap_or(0);
        }
        Self {
            zero,
            one: product & low_mask,
        }
    }

    fn shl(self, shift: u8) -> Self {
        Self {
            zero: !(!self.zero).checked_shl(u32::from(shift)).unwrap_or(0),
            one: self.one.checked_shl(u32::from(shift)).unwrap_or(0),
        }
    }

    fn shr(self, shift: u8) -> Self {
        Self {
            zero: !(!self.zero).checked_shr(u32::from(shift)).unwrap_or(0),
            one: self.one.checked_shr(u32::from(shift)).unwrap_or(0),
        }
    }
}

fn comparison_selection(inst: &MInst, bits: impl Fn(VReg) -> KnownBits) -> Option<VReg> {
    let (condition, true_val, false_val) = match *inst {
        MInst::CmpSelect {
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
            ..
        } => (
            bits(lhs).compare(bits(rhs), kind).truth(),
            true_val,
            false_val,
        ),
        MInst::CmpImmSelect {
            lhs,
            imm,
            kind,
            true_val,
            false_val,
            ..
        } => (
            bits(lhs)
                .compare(KnownBits::constant(imm as u64), kind)
                .truth(),
            true_val,
            false_val,
        ),
        MInst::GuardedCmpSelect {
            guard,
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
            ..
        } => {
            let condition = match (
                bits(guard).truth(),
                bits(lhs).compare(bits(rhs), kind).truth(),
            ) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            };
            (condition, true_val, false_val)
        }
        _ => return None,
    };
    condition.map(|condition| if condition { true_val } else { false_val })
}

fn instruction_bits(inst: &MInst, facts: &[KnownBits]) -> KnownBits {
    let bits = |value: VReg| facts[value.0 as usize];
    let low32 = u64::from(u32::MAX);
    match inst {
        MInst::LoadImm { value, .. } => KnownBits::constant(*value),
        MInst::Load { size, .. }
        | MInst::LoadIndexed { size, .. }
        | MInst::LoadPtr { size, .. }
        | MInst::LoadPtrIndexed { size, .. } => {
            KnownBits::default().truncate(machine_width_mask(*size))
        }
        MInst::Mov { src, .. } => bits(*src),
        MInst::Mov32 { src, .. } => bits(*src).truncate(low32),
        MInst::BitExtract {
            src, lsb, width, ..
        } => bits(*src).shr(*lsb).truncate(u64::MAX >> (64 - width)),
        MInst::BitInsert {
            base,
            src,
            lsb,
            width,
            ..
        } => {
            let low_mask = u64::MAX >> (64 - width);
            bits(*base)
                .and(KnownBits::constant(!(low_mask << lsb)))
                .or(bits(*src).truncate(low_mask).shl(*lsb))
        }
        MInst::OrShifted {
            lhs, rhs, shift, ..
        } => bits(*lhs).or(bits(*rhs).shl(*shift)),
        MInst::BitNot { src, .. } => bits(*src).not(),
        MInst::And { lhs, rhs, .. } => bits(*lhs).and(bits(*rhs)),
        MInst::And32 { lhs, rhs, .. } => bits(*lhs).and(bits(*rhs)).truncate(low32),
        MInst::AndImm { src, imm, .. } => bits(*src).and(KnownBits::constant(*imm)),
        MInst::AndImm32 { src, imm, .. } => bits(*src).and(KnownBits::constant(u64::from(*imm))),
        MInst::Or { lhs, rhs, .. } => bits(*lhs).or(bits(*rhs)),
        MInst::Or32 { lhs, rhs, .. } => bits(*lhs).or(bits(*rhs)).truncate(low32),
        MInst::OrImm { src, imm, .. } => bits(*src).or(KnownBits::constant(*imm)),
        MInst::Xor { lhs, rhs, .. } => bits(*lhs).xor(bits(*rhs)),
        MInst::Xor32 { lhs, rhs, .. } => bits(*lhs).xor(bits(*rhs)).truncate(low32),
        MInst::Add { lhs, rhs, .. } => bits(*lhs).add(bits(*rhs)),
        MInst::Add32 { lhs, rhs, .. } => bits(*lhs).add(bits(*rhs)).truncate(low32),
        MInst::AddImm { src, imm, .. } => bits(*src).add(KnownBits::constant(*imm as u64)),
        MInst::Sub { lhs, rhs, .. } => bits(*lhs).add(bits(*rhs).not()).add(KnownBits::constant(1)),
        MInst::Sub32 { lhs, rhs, .. } => bits(*lhs)
            .add(bits(*rhs).not())
            .add(KnownBits::constant(1))
            .truncate(low32),
        MInst::SubImm { src, imm, .. } => {
            bits(*src).add(KnownBits::constant((*imm as u64).wrapping_neg()))
        }
        MInst::Mul { lhs, rhs, .. } => bits(*lhs).mul(bits(*rhs)),
        MInst::Mul32 { lhs, rhs, .. } => bits(*lhs).mul(bits(*rhs)).truncate(low32),
        MInst::ShrImm { src, imm, .. } => bits(*src).shr(*imm),
        MInst::ShlImm { src, imm, .. } => bits(*src).shl(*imm),
        MInst::Cmp { lhs, rhs, kind, .. } => bits(*lhs).compare(bits(*rhs), *kind),
        MInst::CmpImm { lhs, imm, kind, .. } => {
            bits(*lhs).compare(KnownBits::constant(*imm as u64), *kind)
        }
        MInst::Shl { lhs, rhs, .. } => {
            if bits(*lhs).value() == Some(0) {
                return KnownBits::constant(0);
            }
            bits(*rhs).value().map_or_else(KnownBits::default, |shift| {
                bits(*lhs).shl(shift.min(64) as u8)
            })
        }
        MInst::Shr { lhs, rhs, .. } => {
            if bits(*lhs).value() == Some(0) {
                return KnownBits::constant(0);
            }
            bits(*rhs).value().map_or_else(KnownBits::default, |shift| {
                bits(*lhs).shr(shift.min(64) as u8)
            })
        }
        MInst::Popcnt { .. } => KnownBits::default().truncate(0x7f),
        MInst::Bsf { .. } => KnownBits::default().truncate(0x7f),
        MInst::Select {
            cond,
            true_val,
            false_val,
            ..
        } => match bits(*cond).truth() {
            Some(true) => bits(*true_val),
            Some(false) => bits(*false_val),
            None => bits(*true_val).join(bits(*false_val)),
        },
        MInst::CmpSelect {
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
        } => comparison_selection(inst, bits)
            .map_or_else(|| bits(*true_val).join(bits(*false_val)), bits),
        _ => KnownBits::default(),
    }
}

/// Start with unknown bits and refine along SSA uses, revisiting a loop only
/// when one of its inputs improves. Phi joins retain facts shared by every
/// incoming edge; they never assume a constant from just the entry edge.
pub(super) fn known_zeros(func: &MFunction) -> Vec<u64> {
    analyze(func).into_iter().map(|bits| bits.zero).collect()
}

fn analyze(func: &MFunction) -> Vec<KnownBits> {
    let value_count = func
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .phis
                .iter()
                .flat_map(|phi| {
                    std::iter::once(phi.dst).chain(phi.sources.iter().map(|&(_, src)| src))
                })
                .chain(
                    block
                        .insts
                        .iter()
                        .flat_map(|inst| inst.def().into_iter().chain(inst.uses())),
                )
        })
        .map(|value| value.0 as usize + 1)
        .max()
        .unwrap_or(0)
        .max(func.vregs.count() as usize);
    let mut definitions = vec![None::<ValueDefinition>; value_count];
    let mut users = vec![Vec::<VReg>::new(); value_count];
    let mut queue = VecDeque::new();
    let mut queued = vec![false; value_count];
    for (block, body) in func.blocks.iter().enumerate() {
        for (phi, row) in body.phis.iter().enumerate() {
            definitions[row.dst.0 as usize] = Some(ValueDefinition::Phi { block, phi });
            queue.push_back(row.dst);
            queued[row.dst.0 as usize] = true;
            for &(_, source) in &row.sources {
                users[source.0 as usize].push(row.dst);
            }
        }
        for (instruction, inst) in body.insts.iter().enumerate() {
            let Some(dst) = inst.def() else { continue };
            definitions[dst.0 as usize] = Some(ValueDefinition::Instruction { block, instruction });
            queue.push_back(dst);
            queued[dst.0 as usize] = true;
            for source in inst.uses() {
                users[source.0 as usize].push(dst);
            }
        }
    }
    let mut facts = vec![KnownBits::default(); value_count];
    while let Some(value) = queue.pop_front() {
        let index = value.0 as usize;
        queued[index] = false;
        let computed = match definitions[index] {
            Some(ValueDefinition::Phi { block, phi }) => func.blocks[block].phis[phi]
                .sources
                .iter()
                .map(|(_, source)| facts[source.0 as usize])
                .reduce(KnownBits::join)
                .unwrap_or_default(),
            Some(ValueDefinition::Instruction { block, instruction }) => {
                instruction_bits(&func.blocks[block].insts[instruction], &facts)
            }
            None => continue,
        };
        let improved = KnownBits {
            zero: facts[index].zero | computed.zero,
            one: facts[index].one | computed.one,
        };
        debug_assert_eq!(improved.zero & improved.one, 0);
        if facts[index] == improved {
            continue;
        }
        facts[index] = improved;
        for &user in &users[index] {
            if !queued[user.0 as usize] {
                queued[user.0 as usize] = true;
                queue.push_back(user);
            }
        }
    }
    facts
}

/// Resolve fixed portions of dynamic bit addresses before allocation. For
/// example, `(index * 288 + 163) & 7` is always 3, so it needs neither an
/// address-dependent mask nor a variable shift in the generated code. Move
/// byte-address arithmetic ahead of a right shift only when the bit facts
/// prove that the original multiply/add/left shift cannot wrap.
pub(super) fn fold(func: &mut MFunction) {
    let facts = analyze(func);
    while (func.vregs.count() as usize) < facts.len() {
        func.vregs.alloc();
    }
    func.spill_descs
        .resize_with(facts.len(), SpillDesc::transient);
    let definitions = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match inst {
            MInst::ShlImm { .. } | MInst::AddImm { .. } | MInst::Mul { .. } => {
                Some((inst.def().unwrap(), inst.clone()))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    for block in &mut func.blocks {
        let original = std::mem::take(&mut block.insts);
        for mut inst in original {
            if let Some(dst) = inst.def()
                && let Some(value) = facts[dst.0 as usize].value()
            {
                inst = MInst::LoadImm { dst, value };
            } else if let Some(src) = comparison_selection(&inst, |value| facts[value.0 as usize]) {
                inst = MInst::Mov {
                    dst: inst.def().unwrap(),
                    src,
                };
            } else if let MInst::BitExtract {
                dst,
                src,
                lsb: 0,
                width,
            } = inst
                && !facts[src.0 as usize].zero & !(u64::MAX >> (64 - width)) == 0
            {
                inst = MInst::Mov { dst, src };
            } else if let MInst::CmpImm {
                dst,
                lhs,
                imm,
                kind,
            } = inst
                && ((kind == CmpKind::Ne && imm == 0) || (kind == CmpKind::Eq && imm == 1))
                && facts[lhs.0 as usize].zero & !1 == !1
            {
                inst = MInst::Mov { dst, src: lhs };
            } else if let MInst::Select {
                dst,
                cond,
                true_val,
                false_val,
            } = inst
            {
                let condition = facts[cond.0 as usize];
                let true_bits = facts[true_val.0 as usize];
                let false_bits = facts[false_val.0 as usize];
                let is_boolean = |bits: KnownBits| bits.zero & !1 == !1;
                if let Some(condition) = condition.truth() {
                    inst = MInst::Mov {
                        dst,
                        src: if condition { true_val } else { false_val },
                    };
                } else if true_bits.value() == Some(0) && false_bits.value() == Some(1) {
                    inst = MInst::CmpImm {
                        dst,
                        lhs: cond,
                        imm: 0,
                        kind: CmpKind::Eq,
                    };
                } else if true_bits.value() == Some(1) && false_bits.value() == Some(0) {
                    inst = if is_boolean(condition) {
                        MInst::Mov { dst, src: cond }
                    } else {
                        MInst::CmpImm {
                            dst,
                            lhs: cond,
                            imm: 0,
                            kind: CmpKind::Ne,
                        }
                    };
                } else if false_bits.value() == Some(0)
                    && is_boolean(condition)
                    && is_boolean(true_bits)
                {
                    inst = MInst::And32 {
                        dst,
                        lhs: cond,
                        rhs: true_val,
                    };
                } else if true_bits.value() == Some(1)
                    && is_boolean(condition)
                    && is_boolean(false_bits)
                {
                    inst = MInst::Or32 {
                        dst,
                        lhs: cond,
                        rhs: false_val,
                    };
                } else if true_bits.value() == Some(0) && is_boolean(false_bits) {
                    let inverted = func.vregs.alloc();
                    func.spill_descs.push(SpillDesc::transient());
                    block.push(MInst::CmpImm {
                        dst: inverted,
                        lhs: cond,
                        imm: 0,
                        kind: CmpKind::Eq,
                    });
                    inst = MInst::And32 {
                        dst,
                        lhs: inverted,
                        rhs: false_val,
                    };
                }
            } else if let MInst::ShrImm {
                dst,
                src,
                imm: shift @ 1..=63,
            } = inst
            {
                match definitions.get(&src) {
                    Some(MInst::ShlImm { src, imm, .. })
                        if *imm < 64 && !facts[src.0 as usize].zero <= (u64::MAX >> imm) =>
                    {
                        inst = if *imm >= shift {
                            MInst::ShlImm {
                                dst,
                                src: *src,
                                imm: *imm - shift,
                            }
                        } else {
                            MInst::ShrImm {
                                dst,
                                src: *src,
                                imm: shift - *imm,
                            }
                        };
                    }
                    Some(MInst::AddImm { src, imm, .. })
                        if *imm >= 0
                            && facts[src.0 as usize].zero.trailing_ones() >= u32::from(shift)
                            && (!facts[src.0 as usize].zero)
                                .checked_add(*imm as u64)
                                .is_some() =>
                    {
                        let shifted = func.vregs.alloc();
                        func.spill_descs.push(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: shifted,
                            src: *src,
                            imm: shift,
                        });
                        inst = MInst::AddImm {
                            dst,
                            src: shifted,
                            imm: ((*imm as u64) >> shift) as i32,
                        };
                    }
                    Some(MInst::Mul { lhs, rhs, .. }) => {
                        for (source, factor) in [(*lhs, *rhs), (*rhs, *lhs)] {
                            if let Some(factor) = facts[factor.0 as usize].value()
                                && factor.trailing_zeros() >= u32::from(shift)
                                && (!facts[source.0 as usize].zero)
                                    .checked_mul(factor)
                                    .is_some()
                            {
                                let scale = func.vregs.alloc();
                                func.spill_descs.push(SpillDesc::remat(factor >> shift));
                                block.push(MInst::LoadImm {
                                    dst: scale,
                                    value: factor >> shift,
                                });
                                inst = MInst::Mul {
                                    dst,
                                    lhs: source,
                                    rhs: scale,
                                };
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            block.push(inst);
        }
    }
}

/// A bounded bit lookup in a small packed bitmap needs one word load and one
/// shift. Only fold a result observed through an exact one-bit mask; the high
/// bits of the widened load are not equivalent to those of the byte load.
pub(super) fn fold_packed_bit_loads(func: &mut MFunction) {
    let facts = analyze(func);
    let uses = super::use_counts(func);
    let definitions = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| inst.def().map(|dst| (dst, inst)))
        .collect::<HashMap<_, _>>();
    let mut replacements = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            let (MInst::AndImm {
                src: shifted,
                imm: 1,
                ..
            }
            | MInst::AndImm32 {
                src: shifted,
                imm: 1,
                ..
            }) = *inst
            else {
                continue;
            };
            if uses.get(&shifted) != Some(&1) {
                continue;
            }
            let Some(&&MInst::Shr { dst, lhs, rhs }) = definitions.get(&shifted) else {
                continue;
            };
            if uses.get(&lhs) != Some(&1) {
                continue;
            }
            let Some(&&MInst::LoadIndexed {
                base,
                offset,
                index,
                scale: 1,
                size: OpSize::S8,
                alias_range: Some(envelope),
                ..
            }) = definitions.get(&lhs)
            else {
                continue;
            };
            let Some(&&MInst::ShrImm {
                src: bit, imm: 3, ..
            }) = definitions.get(&index)
            else {
                continue;
            };
            if !matches!(definitions.get(&rhs), Some(MInst::AndImm { src, imm: 7, .. } | MInst::AndImm32 { src, imm: 7, .. }) if *src == bit)
            {
                continue;
            }
            let size = match !facts[bit.0 as usize].zero {
                8..=15 => OpSize::S16,
                16..=31 => OpSize::S32,
                32..=63 => OpSize::S64,
                _ => continue,
            };
            if offset != envelope.offset() || usize::from(size.bytes()) > envelope.byte_len() {
                continue;
            }
            replacements.insert(
                lhs,
                MInst::Load {
                    dst: lhs,
                    base,
                    offset,
                    size,
                },
            );
            replacements.insert(dst, MInst::Shr { dst, lhs, rhs: bit });
        }
    }
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            if let Some(replacement) = inst.def().and_then(|dst| replacements.remove(&dst)) {
                if let MInst::Load { dst, .. } = replacement {
                    func.spill_descs[dst.0 as usize] = SpillDesc::transient();
                }
                *inst = replacement;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(bits: KnownBits, value: u64) -> bool {
        value & bits.zero == 0 && value & bits.one == bits.one
    }
    #[test]
    fn arithmetic_facts_cover_all_concrete_inputs_including_wrapping() {
        // Exhaust every four-bit abstract input (zero, one, or unknown per
        // bit). Move it across the 32/64-bit boundaries and exercise carries
        // into and out of the fixed surrounding bits as well.
        for shift in [0, 30, 60] {
            for surrounding in [0, 0xaaaa_aaaa_aaaa_aaaa, u64::MAX] {
                let inputs = (0..81)
                    .map(|mut pattern| {
                        let mut facts = KnownBits::constant(surrounding & !(15u64 << shift));
                        for bit in shift..shift + 4 {
                            match pattern % 3 {
                                0 => facts.zero &= !(1 << bit),
                                1 => {}
                                2 => {
                                    facts.zero &= !(1 << bit);
                                    facts.one |= 1 << bit;
                                }
                                _ => unreachable!(),
                            }
                            pattern /= 3;
                        }
                        let values = (0..16)
                            .map(|bits| (surrounding & !(15u64 << shift)) | (bits << shift))
                            .filter(|&value| contains(facts, value))
                            .collect::<Vec<_>>();
                        (facts, values)
                    })
                    .collect::<Vec<_>>();
                for (left, left_values) in &inputs {
                    for (right, right_values) in &inputs {
                        let sum = left.add(*right);
                        let difference = left.add(right.not()).add(KnownBits::constant(1));
                        let product = left.mul(*right);
                        for &a in left_values {
                            for &b in right_values {
                                for (facts, value) in [
                                    (sum, a.wrapping_add(b)),
                                    (difference, a.wrapping_sub(b)),
                                    (product, a.wrapping_mul(b)),
                                    (left.and(*right), a & b),
                                    (left.or(*right), a | b),
                                    (left.xor(*right), a ^ b),
                                ] {
                                    assert_eq!(facts.zero & facts.one, 0);
                                    assert!(
                                        contains(facts, value),
                                        "{left:?} {right:?} => {facts:?}, actual {value:#x}"
                                    );
                                    assert!(contains(
                                        facts.truncate(u64::from(u32::MAX)),
                                        value & u64::from(u32::MAX)
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_high_bits_do_not_hide_fixed_array_offset_bits() {
        let index = KnownBits::default();
        for stride in [8, 32, 288, 512, 1u64 << 63] {
            for offset in [0, 9, 18, 163, u64::MAX] {
                let address = index
                    .mul(KnownBits::constant(stride))
                    .add(KnownBits::constant(offset));
                assert_eq!(
                    address.and(KnownBits::constant(7)).value(),
                    Some(offset & 7)
                );
                for index in [0, 1, 15, u64::MAX, u64::MAX / stride] {
                    assert!(contains(
                        address,
                        index.wrapping_mul(stride).wrapping_add(offset)
                    ));
                }
            }
        }
        // An odd stride can change every bit of the address.
        assert_eq!(
            index
                .mul(KnownBits::constant(7))
                .add(KnownBits::constant(163)),
            KnownBits::default()
        );
    }
}
