//! Bit facts for machine-width SSA arithmetic, including packed array offsets.

use super::*;

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
        MInst::BsrOr { zero_value, .. } => {
            KnownBits::default().truncate(0x3f | u64::from(*zero_value))
        }
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
    let value_count = func.vregs.count() as usize;
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
    for (index, zero) in counted_loop::index_zeros(func) {
        facts[index.0 as usize].zero = zero;
    }
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
                    if func.target_features.bmi1() && is_boolean(condition) {
                        block.push(MInst::BitNot {
                            dst: inverted,
                            src: cond,
                        });
                    } else {
                        block.push(MInst::CmpImm {
                            dst: inverted,
                            lhs: cond,
                            imm: 0,
                            kind: CmpKind::Eq,
                        });
                    }
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

/// Propagate the bits observed by stores and predicates backwards through SSA.
/// Packed struct reconstruction often computes a whole word only to extract
/// one field. Preserve every demanded bit, but allow dead fields to disappear
/// before they acquire registers and spill homes.
fn demand_masks(func: &MFunction, facts: &[KnownBits]) -> Vec<u64> {
    let mut definitions = vec![None; facts.len()];
    let mut demanded = vec![0u64; facts.len()];
    let mut work = VecDeque::new();
    let add = |value: VReg, mask: u64, demanded: &mut [u64], work: &mut VecDeque<VReg>| {
        let old = demanded[value.0 as usize];
        if mask & !old != 0 {
            demanded[value.0 as usize] |= mask;
            work.push_back(value);
        }
    };
    for (block, body) in func.blocks.iter().enumerate() {
        for (phi, row) in body.phis.iter().enumerate() {
            definitions[row.dst.0 as usize] = Some(ValueDefinition::Phi { block, phi });
        }
        for (instruction, inst) in body.insts.iter().enumerate() {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] =
                    Some(ValueDefinition::Instruction { block, instruction });
            } else {
                for source in inst.uses() {
                    let mask = match inst {
                        MInst::Store { src, size, .. } if source == *src => {
                            machine_width_mask(*size)
                        }
                        _ => u64::MAX,
                    };
                    add(source, mask, &mut demanded, &mut work);
                }
            }
        }
    }
    while let Some(value) = work.pop_front() {
        let mask = demanded[value.0 as usize];
        let mut use_bits = |source, bits| add(source, bits, &mut demanded, &mut work);
        let Some(definition) = definitions[value.0 as usize] else {
            continue;
        };
        let inst = match definition {
            ValueDefinition::Phi { block, phi } => {
                for &(_, source) in &func.blocks[block].phis[phi].sources {
                    use_bits(source, mask);
                }
                continue;
            }
            ValueDefinition::Instruction { block, instruction } => {
                &func.blocks[block].insts[instruction]
            }
        };
        match *inst {
            MInst::Mov { src, .. } | MInst::BitNot { src, .. } => use_bits(src, mask),
            MInst::OrImm { src, imm, .. } => use_bits(src, mask & !imm),
            MInst::Mov32 { src, .. } => use_bits(src, mask & u64::from(u32::MAX)),
            MInst::AndImm { src, imm, .. } => use_bits(src, mask & imm),
            MInst::AndImm32 { src, imm, .. } => use_bits(src, mask & u64::from(imm)),
            MInst::ShlImm { src, imm, .. } => {
                use_bits(src, mask.checked_shr(u32::from(imm)).unwrap_or(0))
            }
            MInst::ShrImm { src, imm, .. } => {
                use_bits(src, mask.checked_shl(u32::from(imm)).unwrap_or(0))
            }
            MInst::And { lhs, rhs, .. }
            | MInst::And32 { lhs, rhs, .. }
            | MInst::Or { lhs, rhs, .. }
            | MInst::Or32 { lhs, rhs, .. }
            | MInst::Xor { lhs, rhs, .. }
            | MInst::Xor32 { lhs, rhs, .. } => {
                let mask = if matches!(
                    inst,
                    MInst::And32 { .. } | MInst::Or32 { .. } | MInst::Xor32 { .. }
                ) {
                    mask & u64::from(u32::MAX)
                } else {
                    mask
                };
                let (left, right) = match inst {
                    MInst::And { .. } | MInst::And32 { .. } => {
                        (facts[lhs.0 as usize].zero, facts[rhs.0 as usize].zero)
                    }
                    MInst::Or { .. } | MInst::Or32 { .. } => {
                        (facts[lhs.0 as usize].one, facts[rhs.0 as usize].one)
                    }
                    _ => (0, 0),
                };
                // A controlling zero (AND) or one (OR) makes the opposite
                // input irrelevant. If both sides control a bit, retain the
                // right side: dropping both would let later rewrites change
                // the originally constant output bit.
                use_bits(lhs, mask & !right);
                use_bits(rhs, mask & (!left | right));
            }
            MInst::Add { lhs, rhs, .. }
            | MInst::Sub { lhs, rhs, .. }
            | MInst::Mul { lhs, rhs, .. }
            | MInst::Add32 { lhs, rhs, .. }
            | MInst::Sub32 { lhs, rhs, .. }
            | MInst::Mul32 { lhs, rhs, .. } => {
                let mask = if matches!(
                    inst,
                    MInst::Add32 { .. } | MInst::Sub32 { .. } | MInst::Mul32 { .. }
                ) {
                    mask & u64::from(u32::MAX)
                } else {
                    mask
                };
                // Every lower bit can contribute a carry, borrow, or product
                // term to the highest observed result bit.
                let inputs = u64::MAX.checked_shr(mask.leading_zeros()).unwrap_or(0);
                use_bits(lhs, inputs);
                use_bits(rhs, inputs);
            }
            MInst::Select {
                cond,
                true_val,
                false_val,
                ..
            } => {
                use_bits(cond, u64::MAX);
                use_bits(true_val, mask);
                use_bits(false_val, mask);
            }
            MInst::CmpSelect {
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => {
                use_bits(lhs, u64::MAX);
                use_bits(rhs, u64::MAX);
                use_bits(true_val, mask);
                use_bits(false_val, mask);
            }
            MInst::CmpImmSelect {
                lhs,
                true_val,
                false_val,
                ..
            } => {
                use_bits(lhs, u64::MAX);
                use_bits(true_val, mask);
                use_bits(false_val, mask);
            }
            MInst::GuardedCmpSelect {
                guard,
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => {
                use_bits(guard, u64::MAX);
                use_bits(lhs, u64::MAX);
                use_bits(rhs, u64::MAX);
                use_bits(true_val, mask);
                use_bits(false_val, mask);
            }
            _ => {
                for source in inst.uses() {
                    use_bits(source, u64::MAX);
                }
            }
        }
    }
    demanded
}

/// Read a bounded packed bitmap as one word, rather than computing a byte
/// address and a second shift count for every tested bit. Only the observed
/// low bit is equivalent; no wider consumer may see the new high bits.
pub(super) fn fold_packed_bit_loads(func: &mut MFunction) {
    let facts = analyze(func);
    let demanded = demand_masks(func, &facts);
    let mut definitions = vec![None; facts.len()];
    let mut uses = vec![0usize; facts.len()];
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                uses[source.0 as usize] += 1;
            }
        }
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst);
            }
            for source in inst.uses() {
                uses[source.0 as usize] += 1;
            }
        }
    }
    let mut replacements = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            let MInst::Shr { dst, lhs, rhs } = *inst else {
                continue;
            };
            if demanded[dst.0 as usize] != 1 || uses[lhs.0 as usize] != 1 {
                continue;
            }
            let Some(&MInst::LoadIndexed {
                base,
                offset,
                index,
                scale: 1,
                size: OpSize::S8,
                alias_range: Some(envelope),
                ..
            }) = definitions[lhs.0 as usize]
            else {
                continue;
            };
            let Some(&MInst::ShrImm {
                src: bit, imm: 3, ..
            }) = definitions[index.0 as usize]
            else {
                continue;
            };
            if !matches!(definitions[rhs.0 as usize], Some(MInst::AndImm { src, imm: 7, .. } | MInst::AndImm32 { src, imm: 7, .. }) if *src == bit)
            {
                continue;
            }
            let maximum = !facts[bit.0 as usize].zero;
            let size = match maximum {
                8..=15 => OpSize::S16,
                16..=31 => OpSize::S32,
                32..=63 => OpSize::S64,
                _ => continue,
            };
            if offset != envelope.offset() || size.bytes() as usize > envelope.byte_len() {
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

pub(super) fn fold_demanded(func: &mut MFunction) {
    let facts = analyze(func);
    let demanded = demand_masks(func, &facts);
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let Some(dst) = inst.def() else { continue };
            let mask = demanded[dst.0 as usize];
            let fact = facts[dst.0 as usize];
            if mask != 0 && mask & !(fact.zero | fact.one) == 0 {
                *inst = MInst::LoadImm {
                    dst,
                    value: fact.one & mask,
                };
                continue;
            }
            let copy = match *inst {
                MInst::AndImm { src, imm, .. } if mask & !imm == 0 => Some(src),
                MInst::AndImm32 { src, imm, .. } if mask & !u64::from(imm) == 0 => Some(src),
                MInst::Mov32 { src, .. } if mask & !u64::from(u32::MAX) == 0 => Some(src),
                MInst::OrImm { src, imm, .. } if mask & imm == 0 => Some(src),
                MInst::Or { lhs, rhs, .. }
                | MInst::Xor { lhs, rhs, .. }
                | MInst::Or32 { lhs, rhs, .. }
                | MInst::Xor32 { lhs, rhs, .. } => {
                    let word_mask = if matches!(inst, MInst::Or32 { .. } | MInst::Xor32 { .. }) {
                        u64::from(u32::MAX)
                    } else {
                        u64::MAX
                    };
                    if mask & !word_mask != 0 {
                        None
                    } else if mask & !facts[lhs.0 as usize].zero == 0 {
                        Some(rhs)
                    } else if mask & !facts[rhs.0 as usize].zero == 0 {
                        Some(lhs)
                    } else {
                        None
                    }
                }
                MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. } => {
                    if matches!(inst, MInst::And32 { .. }) && mask & !u64::from(u32::MAX) != 0 {
                        None
                    } else if mask & !facts[lhs.0 as usize].one == 0 {
                        Some(rhs)
                    } else if mask & !facts[rhs.0 as usize].one == 0 {
                        Some(lhs)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(src) = copy {
                *inst = MInst::Mov { dst, src };
            } else if mask & !u64::from(u32::MAX) == 0
                || fact.zero & !u64::from(u32::MAX) == !u64::from(u32::MAX)
            {
                *inst = match *inst {
                    MInst::And { dst, lhs, rhs } => MInst::And32 { dst, lhs, rhs },
                    MInst::Or { dst, lhs, rhs } => MInst::Or32 { dst, lhs, rhs },
                    MInst::Xor { dst, lhs, rhs } => MInst::Xor32 { dst, lhs, rhs },
                    MInst::Add { dst, lhs, rhs } => MInst::Add32 { dst, lhs, rhs },
                    MInst::Sub { dst, lhs, rhs } => MInst::Sub32 { dst, lhs, rhs },
                    MInst::Mul { dst, lhs, rhs } => MInst::Mul32 { dst, lhs, rhs },
                    MInst::AndImm { dst, src, imm } => MInst::AndImm32 {
                        dst,
                        src,
                        imm: imm as u32,
                    },
                    _ => continue,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boolean_selects_preserve_nonzero_truth_and_full_width_arms() {
        use crate::native::{
            emit, features::X86Features, jit_mem::JitCode, mir_legalize, regalloc,
        };
        fn compile(mut function: MFunction) -> (JitCode, usize) {
            mir_legalize::legalize(&mut function);
            let allocation = regalloc::run_regalloc(&mut function).unwrap();
            let emitted = emit::emit(
                &function,
                &allocation.assignment,
                allocation.spill_frame_size,
            )
            .unwrap();
            (
                JitCode::new(&emitted.code).unwrap(),
                emitted.required_state_size.max(128) as usize,
            )
        }
        for normalized in [false, true] {
            for bmi1 in [false, true] {
                if bmi1 && !std::arch::is_x86_feature_detected!("bmi1") {
                    continue;
                }
                let mut vregs = VRegAllocator::new();
                for _ in 0..24 {
                    vregs.alloc();
                }
                let mut original = MFunction::new(vregs, vec![SpillDesc::transient(); 24]);
                original.target_features = X86Features::for_test(false).with_bmi1(bmi1);
                let mut block = MBlock::new(BlockId(0));
                for index in 0..3 {
                    block.push(MInst::Load {
                        dst: VReg(index),
                        base: BaseReg::SimState,
                        offset: index as i32 * 8,
                        size: OpSize::S64,
                    });
                }
                block.push(MInst::LoadImm {
                    dst: VReg(3),
                    value: 0,
                });
                block.push(MInst::LoadImm {
                    dst: VReg(4),
                    value: 1,
                });
                block.push(if normalized {
                    MInst::AndImm {
                        dst: VReg(5),
                        src: VReg(0),
                        imm: 1,
                    }
                } else {
                    MInst::Mov {
                        dst: VReg(5),
                        src: VReg(0),
                    }
                });
                block.push(MInst::AndImm {
                    dst: VReg(6),
                    src: VReg(1),
                    imm: 1,
                });
                block.push(MInst::AndImm {
                    dst: VReg(7),
                    src: VReg(2),
                    imm: 1,
                });
                let arms = [
                    (3, 4),
                    (4, 3),
                    (6, 3),
                    (4, 7),
                    (3, 7),
                    (6, 7),
                    (1, 3),
                    (4, 2),
                    (3, 2),
                ];
                for (index, (a, b)) in arms.into_iter().enumerate() {
                    let dst = VReg(8 + index as u32);
                    block.push(MInst::Select {
                        dst,
                        cond: VReg(5),
                        true_val: VReg(a),
                        false_val: VReg(b),
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 32 + index as i32 * 8,
                        src: dst,
                        size: OpSize::S64,
                    });
                }
                block.push(MInst::Shl {
                    dst: VReg(20),
                    lhs: VReg(3),
                    rhs: VReg(0),
                });
                block.push(MInst::Shr {
                    dst: VReg(21),
                    lhs: VReg(3),
                    rhs: VReg(0),
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 104,
                    src: VReg(20),
                    size: OpSize::S64,
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 112,
                    src: VReg(21),
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
                original.push_block(block);
                let mut optimized = original.clone();
                fold(&mut optimized);
                copy_propagate(&mut optimized);
                dead_code_eliminate(&mut optimized);
                optimized.verify();
                let (before, before_size) = compile(original);
                let (after, after_size) = compile(optimized);
                let values = [
                    0u64,
                    1,
                    2,
                    3,
                    31,
                    32,
                    63,
                    64,
                    65,
                    1 << 32,
                    1 << 63,
                    u64::MAX,
                ];
                for condition in values {
                    for a in values {
                        for b in values {
                            let mut left = vec![0u8; before_size.max(after_size)];
                            for (index, value) in [condition, a, b].into_iter().enumerate() {
                                left[index * 8..index * 8 + 8]
                                    .copy_from_slice(&value.to_le_bytes());
                            }
                            let mut right = left.clone();
                            assert_eq!(unsafe { before.call(&mut left) }, 0);
                            assert_eq!(unsafe { after.call(&mut right) }, 0);
                            assert_eq!(
                                &left[32..120],
                                &right[32..120],
                                "normalized={normalized}, bmi1={bmi1}, condition={condition:#x}, a={a:#x}, b={b:#x}"
                            );
                        }
                    }
                }
            }
        }
    }

    fn contains(facts: KnownBits, value: u64) -> bool {
        facts.zero & value == 0 && facts.one & value == facts.one
    }

    #[test]
    fn demanded_bits_keep_a_controlling_operand_when_both_sides_agree() {
        use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};
        let masks = [0, 1, 0xff00, 0x8000_0001, 0xffff_0000_ffff_0000, u64::MAX];
        for is_or in [false, true] {
            for word32 in [false, true] {
                for left_mask in masks {
                    for right_mask in masks {
                        let mut vregs = VRegAllocator::new();
                        for _ in 0..7 {
                            vregs.alloc();
                        }
                        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);
                        let mut block = MBlock::new(BlockId(0));
                        for index in 0..2 {
                            block.push(MInst::Load {
                                dst: VReg(index),
                                base: BaseReg::SimState,
                                offset: index as i32 * 8,
                                size: OpSize::S64,
                            });
                        }
                        for (index, value) in [(2, left_mask), (3, right_mask)] {
                            block.push(MInst::LoadImm {
                                dst: VReg(index),
                                value,
                            });
                        }
                        for (dst, lhs, rhs) in [
                            (VReg(4), VReg(0), VReg(2)),
                            (VReg(5), VReg(1), VReg(3)),
                            (VReg(6), VReg(4), VReg(5)),
                        ] {
                            block.push(match (is_or, word32) {
                                (false, false) => MInst::And { dst, lhs, rhs },
                                (false, true) => MInst::And32 { dst, lhs, rhs },
                                (true, false) => MInst::Or { dst, lhs, rhs },
                                (true, true) => MInst::Or32 { dst, lhs, rhs },
                            });
                        }
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: 16,
                            src: VReg(6),
                            size: OpSize::S64,
                        });
                        block.push(MInst::Return);
                        function.push_block(block);
                        for _ in 0..2 {
                            fold_demanded(&mut function);
                            copy_propagate(&mut function);
                            dead_code_eliminate(&mut function);
                        }
                        function.verify();
                        mir_legalize::legalize(&mut function);
                        let allocation = regalloc::run_regalloc(&mut function).unwrap();
                        let emitted = emit::emit(
                            &function,
                            &allocation.assignment,
                            allocation.spill_frame_size,
                        )
                        .unwrap();
                        let jit = JitCode::new(&emitted.code).unwrap();
                        for bit in 0..64 {
                            for inverted in [false, true] {
                                let left = if inverted {
                                    !(1u64 << bit)
                                } else {
                                    1u64 << bit
                                };
                                let right = left.rotate_left(17);
                                let mut expected = if is_or {
                                    left | left_mask | right | right_mask
                                } else {
                                    left & left_mask & right & right_mask
                                };
                                if word32 {
                                    expected &= u64::from(u32::MAX);
                                }
                                let mut state =
                                    vec![0u8; emitted.required_state_size.max(24) as usize];
                                state[..8].copy_from_slice(&left.to_le_bytes());
                                state[8..16].copy_from_slice(&right.to_le_bytes());
                                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                                assert_eq!(
                                    u64::from_le_bytes(state[16..24].try_into().unwrap()),
                                    expected,
                                    "or={is_or} word32={word32} left_mask={left_mask:#x} right_mask={right_mask:#x} bit={bit} inverted={inverted}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn demanded_bits_preserve_generated_word_and_field_extractions() {
        use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};
        fn compile(mut function: MFunction, simplify: bool) -> JitCode {
            mir_legalize::legalize(&mut function);
            if simplify {
                for _ in 0..2 {
                    fold_demanded(&mut function);
                    copy_propagate(&mut function);
                    dead_code_eliminate(&mut function);
                }
                refresh_constant_spill_descs(&mut function);
            }
            function.verify();
            let allocation = regalloc::run_regalloc(&mut function).unwrap();
            let emitted = emit::emit(
                &function,
                &allocation.assignment,
                allocation.spill_frame_size,
            )
            .unwrap();
            JitCode::new(&emitted.code).unwrap()
        }
        fn random(seed: &mut u64) -> u64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            *seed
        }
        for case in 1..=24 {
            let mut seed = case;
            let mut vregs = VRegAllocator::new();
            for _ in 0..40 {
                vregs.alloc();
            }
            let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 40]);
            let mut block = MBlock::new(BlockId(0));
            for index in 0..3 {
                block.push(MInst::Load {
                    dst: VReg(index),
                    base: BaseReg::SimState,
                    offset: index as i32 * 8,
                    size: OpSize::S64,
                });
            }
            block.push(MInst::LoadImm {
                dst: VReg(3),
                value: u64::MAX,
            });
            for index in 4..32 {
                let dst = VReg(index);
                let lhs = VReg((random(&mut seed) % u64::from(index)) as u32);
                let rhs = VReg((random(&mut seed) % u64::from(index)) as u32);
                let shift = [0, 1, 7, 8, 15, 31, 32, 62, 63][(random(&mut seed) % 9) as usize];
                block.push(match index % 13 {
                    0 => MInst::And { dst, lhs, rhs },
                    1 => MInst::And32 { dst, lhs, rhs },
                    2 => MInst::Or { dst, lhs, rhs },
                    3 => MInst::Or32 { dst, lhs, rhs },
                    4 => MInst::Xor { dst, lhs, rhs },
                    5 => MInst::Xor32 { dst, lhs, rhs },
                    6 => MInst::ShlImm {
                        dst,
                        src: lhs,
                        imm: shift,
                    },
                    7 => MInst::ShrImm {
                        dst,
                        src: lhs,
                        imm: shift,
                    },
                    8 => MInst::AndImm32 {
                        dst,
                        src: lhs,
                        imm: 0xff00_ff00,
                    },
                    9 => MInst::Mov32 { dst, src: lhs },
                    10 => MInst::BitNot { dst, src: lhs },
                    11 => MInst::Select {
                        dst,
                        cond: VReg(2),
                        true_val: lhs,
                        false_val: rhs,
                    },
                    _ => MInst::Add { dst, lhs, rhs },
                });
            }
            // Observe different fields of shared graphs, and retain a complete
            // 64-bit use so narrowing a second use cannot discard its upper bits.
            for index in 0..8 {
                block.push(MInst::ShrImm {
                    dst: VReg(32 + index),
                    src: VReg(31 - index / 2),
                    imm: (index * 8) as u8,
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24 + index as i32 * 8,
                    src: VReg(32 + index),
                    size: [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64][index as usize % 4],
                });
            }
            block.push(MInst::Return);
            function.push_block(block);
            let reference = compile(function.clone(), false);
            let optimized = compile(function, true);
            for sample in 0..128 {
                let mut expected = [0xa5u8; 88];
                for lane in 0..3 {
                    let value = if sample < 64 {
                        1u64 << sample
                    } else {
                        random(&mut seed)
                    };
                    expected[lane * 8..lane * 8 + 8].copy_from_slice(&value.to_le_bytes());
                }
                if sample % 2 == 0 {
                    expected[16..24].fill(0);
                }
                let mut actual = expected;
                assert_eq!(unsafe { reference.call(&mut expected) }, 0);
                assert_eq!(unsafe { optimized.call(&mut actual) }, 0);
                assert_eq!(actual, expected, "case={case}, sample={sample}");
            }
        }
    }

    #[test]
    fn demanded_bits_remove_unused_packed_fields() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..6 {
            vregs.alloc();
        }
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
        let mut block = MBlock::new(BlockId(0));
        for index in 0..2 {
            block.push(MInst::Load {
                dst: VReg(index),
                base: BaseReg::SimState,
                offset: index as i32 * 8,
                size: OpSize::S8,
            });
        }
        block.push(MInst::ShlImm {
            dst: VReg(2),
            src: VReg(1),
            imm: 8,
        });
        block.push(MInst::Or {
            dst: VReg(3),
            lhs: VReg(0),
            rhs: VReg(2),
        });
        block.push(MInst::ShlImm {
            dst: VReg(4),
            src: VReg(3),
            imm: 16,
        });
        block.push(MInst::ShrImm {
            dst: VReg(5),
            src: VReg(4),
            imm: 16,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(5),
            size: OpSize::S8,
        });
        block.push(MInst::Return);
        function.push_block(block);
        fold_demanded(&mut function);
        copy_propagate(&mut function);
        dead_code_eliminate(&mut function);
        function.verify();
        assert!(
            !function.blocks[0]
                .insts
                .iter()
                .any(|inst| matches!(inst, MInst::Load { offset: 8, .. } | MInst::Or { .. }))
        );
    }

    #[test]
    fn demanded_bits_reach_a_fixed_point_through_loop_phis() {
        use crate::native::{emit, jit_mem::JitCode, regalloc};
        let mut vregs = VRegAllocator::new();
        for _ in 0..9 {
            vregs.alloc();
        }
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 9]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S8,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.phis.push(PhiNode {
            dst: VReg(2),
            sources: vec![(BlockId(0), VReg(0)), (BlockId(2), VReg(7))],
        });
        header.phis.push(PhiNode {
            dst: VReg(3),
            sources: vec![(BlockId(0), VReg(1)), (BlockId(2), VReg(8))],
        });
        header.push(MInst::Branch {
            cond: VReg(3),
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut body = MBlock::new(BlockId(2));
        body.push(MInst::ShlImm {
            dst: VReg(5),
            src: VReg(2),
            imm: 8,
        });
        body.push(MInst::ShrImm {
            dst: VReg(6),
            src: VReg(2),
            imm: 56,
        });
        body.push(MInst::Or {
            dst: VReg(7),
            lhs: VReg(5),
            rhs: VReg(6),
        });
        body.push(MInst::SubImm {
            dst: VReg(8),
            src: VReg(3),
            imm: 1,
        });
        body.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 9,
            src: VReg(2),
            size: OpSize::S8,
        });
        exit.push(MInst::Return);
        for block in [entry, header, exit, body] {
            function.push_block(block);
        }
        fold_demanded(&mut function);
        copy_propagate(&mut function);
        dead_code_eliminate(&mut function);
        function.verify();
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let input = 0x0123_4567_89ab_cdefu64;
        for rotations in 0..=16 {
            let mut state = [0u8; 16];
            state[..8].copy_from_slice(&input.to_le_bytes());
            state[8] = rotations;
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(state[9], input.rotate_left(u32::from(rotations) * 8) as u8);
        }
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

    #[test]
    fn immediate_shifts_preserve_fixed_bits_at_machine_boundaries() {
        for value in [0, 1, 0x8000_0000_ffff_ffff, u64::MAX] {
            for shift in [0, 1, 31, 32, 63, 64, 255] {
                let facts = KnownBits::constant(value);
                assert_eq!(
                    facts.shl(shift).value(),
                    Some(value.checked_shl(u32::from(shift)).unwrap_or(0))
                );
                assert_eq!(
                    facts.shr(shift).value(),
                    Some(value.checked_shr(u32::from(shift)).unwrap_or(0))
                );
            }
        }
    }

    #[test]
    fn late_address_bits_fold_comparisons_and_selects_without_losing_wrapping() {
        use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

        let mut vregs = VRegAllocator::new();
        for _ in 0..10 {
            vregs.alloc();
        }
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 10]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 288,
        });
        entry.push(MInst::Mul {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        });
        entry.push(MInst::AddImm {
            dst: VReg(3),
            src: VReg(2),
            imm: 163,
        });
        entry.push(MInst::AndImm {
            dst: VReg(4),
            src: VReg(3),
            imm: 7,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut body = MBlock::new(BlockId(1));
        body.push(MInst::CmpImm {
            dst: VReg(5),
            lhs: VReg(4),
            imm: 3,
            kind: CmpKind::Eq,
        });
        body.push(MInst::Load {
            dst: VReg(6),
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S64,
        });
        body.push(MInst::LoadImm {
            dst: VReg(7),
            value: 0xdead_beef,
        });
        body.push(MInst::Select {
            dst: VReg(8),
            cond: VReg(5),
            true_val: VReg(6),
            false_val: VReg(7),
        });
        body.push(MInst::Shl {
            dst: VReg(9),
            lhs: VReg(6),
            rhs: VReg(4),
        });
        body.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(8),
            size: OpSize::S64,
        });
        body.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 24,
            src: VReg(9),
            size: OpSize::S64,
        });
        body.push(MInst::Return);
        func.blocks = vec![entry, body];
        fold(&mut func);
        assert!(
            !func
                .blocks
                .iter()
                .flat_map(|b| &b.insts)
                .any(|inst| matches!(inst, MInst::CmpImm { .. } | MInst::Select { .. }))
        );
        mir_legalize::legalize(&mut func);
        optimize(&mut func);
        let ra = regalloc::run_regalloc(&mut func).unwrap();
        post_regalloc_peephole(&mut func, &ra.assignment);
        let emitted = emit::emit(&func, &ra.assignment, ra.spill_frame_size).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        for input in [0u64, 1, u64::MAX / 288, 1 << 63, u64::MAX] {
            for value in [0u64, 7, 0xdead_beef, u64::MAX] {
                let mut state = vec![0u8; emitted.required_state_size.max(32) as usize];
                state[..8].copy_from_slice(&input.to_le_bytes());
                state[8..16].copy_from_slice(&value.to_le_bytes());
                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                assert_eq!(u64::from_le_bytes(state[16..24].try_into().unwrap()), value);
                assert_eq!(
                    u64::from_le_bytes(state[24..32].try_into().unwrap()),
                    value << 3
                );
            }
        }
    }

    #[test]
    fn comparison_bounds_preserve_unknown_signs_and_overlapping_ranges() {
        let mut cases = Vec::new();
        for free in [1u64, 7, 0xff, 0xf000_0000, 1 << 63, (1 << 63) | 7] {
            for fixed in [0u64, u64::MAX, 1 << 31, 1 << 63] {
                let fixed = fixed & !free;
                let facts = KnownBits {
                    zero: !(fixed | free),
                    one: fixed,
                };
                let mut values = vec![fixed, fixed | free];
                for bit in 0..64 {
                    if free & (1 << bit) != 0 {
                        values.push(fixed | (1 << bit));
                        values.push(fixed | (free & !(1 << bit)));
                    }
                }
                cases.push((facts, values));
            }
        }
        for (left, lhs_values) in &cases {
            for (right, rhs_values) in &cases {
                for kind in [
                    CmpKind::Eq,
                    CmpKind::Ne,
                    CmpKind::LtU,
                    CmpKind::LeU,
                    CmpKind::GtU,
                    CmpKind::GeU,
                    CmpKind::LtS,
                    CmpKind::LeS,
                    CmpKind::GtS,
                    CmpKind::GeS,
                ] {
                    let result = left.compare(*right, kind);
                    for &lhs in lhs_values {
                        for &rhs in rhs_values {
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
                            assert!(
                                contains(result, u64::from(value)),
                                "kind={kind:?} lhs={lhs:#x} rhs={rhs:#x}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn comparison_facts_preserve_signedness_and_unknown_bits() {
        for lhs in [0u64, 1, 1 << 31, 1 << 63, u64::MAX] {
            for rhs in [0u64, 1, 1 << 31, 1 << 63, u64::MAX] {
                for (kind, expected) in [
                    (CmpKind::Eq, lhs == rhs),
                    (CmpKind::Ne, lhs != rhs),
                    (CmpKind::LtU, lhs < rhs),
                    (CmpKind::LeU, lhs <= rhs),
                    (CmpKind::GtU, lhs > rhs),
                    (CmpKind::GeU, lhs >= rhs),
                    (CmpKind::LtS, (lhs as i64) < (rhs as i64)),
                    (CmpKind::LeS, (lhs as i64) <= (rhs as i64)),
                    (CmpKind::GtS, (lhs as i64) > (rhs as i64)),
                    (CmpKind::GeS, (lhs as i64) >= (rhs as i64)),
                ] {
                    assert_eq!(
                        KnownBits::constant(lhs)
                            .compare(KnownBits::constant(rhs), kind)
                            .value(),
                        Some(u64::from(expected))
                    );
                }
            }
        }
        let low_clear = KnownBits { zero: 1, one: 0 };
        let low_set = KnownBits { zero: 0, one: 1 };
        assert_eq!(low_clear.compare(low_set, CmpKind::Eq).value(), Some(0));
        assert_eq!(low_clear.compare(low_set, CmpKind::Ne).value(), Some(1));
        assert_eq!(low_clear.compare(low_set, CmpKind::LtU).value(), None);
        assert_eq!(low_clear.truth(), None);
        assert_eq!(low_set.truth(), Some(true));
    }

    #[test]
    fn optimized_array_address_arithmetic_executes_with_wrapping_and_narrow_inputs() {
        use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

        for size in [OpSize::S32, OpSize::S64] {
            for factor in [0u64, 7, 8, 32, 288, 1 << 63] {
                for addend in [0i32, 9, 163, -9] {
                    let mut vregs = VRegAllocator::new();
                    for _ in 0..8 {
                        vregs.alloc();
                    }
                    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 8]);
                    let mut block = MBlock::new(BlockId(0));
                    block.push(MInst::Load {
                        dst: VReg(0),
                        base: BaseReg::SimState,
                        offset: 0,
                        size,
                    });
                    block.push(MInst::LoadImm {
                        dst: VReg(1),
                        value: factor,
                    });
                    block.push(MInst::Mul {
                        dst: VReg(2),
                        lhs: VReg(0),
                        rhs: VReg(1),
                    });
                    block.push(MInst::AddImm {
                        dst: VReg(3),
                        src: VReg(2),
                        imm: addend,
                    });
                    block.push(MInst::ShrImm {
                        dst: VReg(4),
                        src: VReg(3),
                        imm: 3,
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 8,
                        src: VReg(4),
                        size: OpSize::S64,
                    });
                    block.push(MInst::AndImm {
                        dst: VReg(5),
                        src: VReg(3),
                        imm: 7,
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 16,
                        src: VReg(5),
                        size: OpSize::S64,
                    });
                    block.push(MInst::LoadImm {
                        dst: VReg(6),
                        value: 0xfedc_ba98_7654_3210,
                    });
                    block.push(MInst::Shr {
                        dst: VReg(7),
                        lhs: VReg(6),
                        rhs: VReg(5),
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 24,
                        src: VReg(7),
                        size: OpSize::S64,
                    });
                    block.push(MInst::Return);
                    func.push_block(block);
                    mir_legalize::legalize(&mut func);
                    optimize(&mut func);
                    let allocation = regalloc::run_regalloc(&mut func).unwrap();
                    let emitted =
                        emit::emit(&func, &allocation.assignment, allocation.spill_frame_size)
                            .unwrap();
                    let jit = JitCode::new(&emitted.code).unwrap();
                    for input in [0u64, 1, 15, u32::MAX as u64, u64::MAX / 32, u64::MAX] {
                        let mut state = [0u8; 32];
                        state[..8].copy_from_slice(&input.to_le_bytes());
                        let source = input & machine_width_mask(size);
                        let address = source.wrapping_mul(factor).wrapping_add(addend as u64);
                        assert_eq!(unsafe { jit.call(&mut state) }, 0);
                        for (offset, expected) in [
                            (8, address >> 3),
                            (16, address & 7),
                            (24, 0xfedc_ba98_7654_3210 >> (address & 7)),
                        ] {
                            assert_eq!(
                                u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap()),
                                expected,
                                "{size:?}: ({input:#x} * {factor:#x} + {addend}), output {offset}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn folds_fixed_offset_bits_across_blocks_but_keeps_distinct_phi_inputs() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..8 {
            vregs.alloc();
        }
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 8]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 288,
        });
        entry.push(MInst::Mul {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        });
        entry.push(MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::AddImm {
            dst: VReg(3),
            src: VReg(2),
            imm: 163,
        });
        left.push(MInst::AndImm {
            dst: VReg(4),
            src: VReg(3),
            imm: 7,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::LoadImm {
            dst: VReg(5),
            value: 5,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut exit = MBlock::new(BlockId(3));
        exit.phis.push(PhiNode {
            dst: VReg(6),
            sources: vec![(BlockId(1), VReg(4)), (BlockId(2), VReg(5))],
        });
        exit.push(MInst::AndImm {
            dst: VReg(7),
            src: VReg(6),
            imm: 1,
        });
        exit.push(MInst::Return);
        // Deliberately visit the consumer before the defining predecessor.
        for block in [entry, exit, left, right] {
            func.push_block(block);
        }
        let facts = analyze(&func);
        assert_eq!(facts[4].value(), Some(3));
        assert_eq!(facts[6].value(), None);
        assert_eq!(facts[7].value(), Some(1));
        fold(&mut func);
        assert!(matches!(
            func.blocks[2].insts[1],
            MInst::LoadImm {
                dst: VReg(4),
                value: 3
            }
        ));
    }
}
