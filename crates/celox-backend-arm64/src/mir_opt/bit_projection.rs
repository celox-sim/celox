//! Collapse bit extraction and reconstruction back to one source operation.

use super::*;

#[derive(Clone, Copy)]
struct Projection {
    source: VReg,
    // output_bit = source_bit + shift, restricted to mask's output bits.
    shift: i16,
    mask: u64,
}

impl Projection {
    fn identity(source: VReg) -> Self {
        Self {
            source,
            shift: 0,
            mask: u64::MAX,
        }
    }

    fn masked(mut self, mask: u64) -> Self {
        self.mask &= mask;
        if self.mask == 0 {
            self.shift = 0;
        }
        self
    }

    fn shifted(mut self, shift: i16) -> Self {
        self.mask = if shift >= 0 {
            self.mask.checked_shl(shift as u32).unwrap_or(0)
        } else {
            self.mask
                .checked_shr(u32::from(shift.unsigned_abs()))
                .unwrap_or(0)
        };
        self.shift = if self.mask == 0 {
            0
        } else {
            self.shift + shift
        };
        self
    }

    fn union(self, other: Self) -> Option<Self> {
        if self.mask == 0 {
            return Some(other);
        }
        if other.mask == 0 {
            return Some(self);
        }
        (self.source == other.source && self.shift == other.shift).then_some(Self {
            mask: self.mask | other.mask,
            ..self
        })
    }

    fn instruction(self, dst: VReg) -> Option<MInst> {
        let src = self.source;
        if self.mask == 0 {
            return Some(MInst::LoadImm { dst, value: 0 });
        }
        if self.shift == 0 {
            return match self.mask {
                u64::MAX => Some(MInst::Mov { dst, src }),
                0xffff_ffff => Some(MInst::Mov32 { dst, src }),
                mask if mask <= u64::from(u32::MAX)
                    && crate::scalar::is_logical_immediate(mask, 32) =>
                {
                    Some(MInst::AndImm32 {
                        dst,
                        src,
                        imm: mask as u32,
                    })
                }
                mask if crate::scalar::is_logical_immediate(mask, 64) => Some(MInst::AndImm {
                    dst,
                    src,
                    imm: mask,
                }),
                _ => None,
            };
        }
        if self.shift > 0 && self.shift < 64 && self.mask == u64::MAX << self.shift {
            return Some(MInst::ShlImm {
                dst,
                src,
                imm: self.shift as u8,
            });
        }
        if self.shift < 0 && self.shift > -64 {
            let lsb = (-self.shift) as u8;
            let width = self.mask.trailing_ones() as u8;
            if width > 0 && width <= 64 - lsb && self.mask.count_ones() == u32::from(width) {
                return Some(MInst::BitExtract {
                    dst,
                    src,
                    lsb,
                    width,
                });
            }
        }
        None
    }
}

/// Keep the usual single-source case allocation-free. A partial bit insert
/// can temporarily combine several sources, then a later mask or shift can
/// isolate one of them again.
#[derive(Clone)]
enum Fragments {
    Single(Projection),
    Mixed(Vec<Projection>),
}

impl Fragments {
    fn identity(source: VReg) -> Self {
        Self::Single(Projection::identity(source))
    }

    fn compact(mut parts: Vec<Projection>) -> Self {
        parts.retain(|part| part.mask != 0);
        match parts.len() {
            0 => Self::identity(VReg(0)).masked(0),
            1 => Self::Single(parts[0]),
            _ => Self::Mixed(parts),
        }
    }

    fn map(self, transform: impl Fn(Projection) -> Projection) -> Self {
        match self {
            Self::Single(part) => Self::Single(transform(part)),
            Self::Mixed(mut parts) => {
                for part in &mut parts {
                    *part = transform(*part);
                }
                Self::compact(parts)
            }
        }
    }

    fn masked(self, mask: u64) -> Self {
        self.map(|part| part.masked(mask))
    }
    fn shifted(self, shift: i16) -> Self {
        self.map(|part| part.shifted(shift))
    }

    fn parts(self) -> Vec<Projection> {
        match self {
            Self::Single(part) => vec![part],
            Self::Mixed(parts) => parts,
        }
    }

    fn union(self, other: Self) -> Option<Self> {
        if let (Self::Single(left), Self::Single(right)) = (&self, &other)
            && let Some(part) = left.union(*right)
        {
            return Some(Self::Single(part));
        }
        let mut parts = self.parts();
        for other in other.parts() {
            if let Some(part) = parts
                .iter_mut()
                .find(|part| part.source == other.source && part.shift == other.shift)
            {
                part.mask |= other.mask;
            } else {
                parts.push(other);
            }
        }
        // Bound compile-time work for long bit-concatenation expressions.
        (parts.len() <= 8).then(|| Self::compact(parts))
    }

    fn instruction(&self, dst: VReg) -> Option<MInst> {
        match self {
            Self::Single(part) if part.source != dst || part.mask == 0 => part.instruction(dst),
            _ => None,
        }
    }
}

pub(crate) fn run(function: &mut MFunction) {
    for block in &mut function.blocks {
        let mut projections = HashMap::<VReg, Fragments>::default();
        for inst in &mut block.insts {
            let Some(dst) = inst.def() else { continue };
            let get = |value| {
                projections
                    .get(&value)
                    .cloned()
                    .unwrap_or_else(|| Fragments::identity(value))
            };
            let projection = match *inst {
                MInst::LoadImm { value: 0, .. } => Some(Fragments::identity(dst).masked(0)),
                MInst::Mov { src, .. } => Some(get(src)),
                MInst::Mov32 { src, .. } => Some(get(src).masked(u64::from(u32::MAX))),
                MInst::AndImm { src, imm, .. } => Some(get(src).masked(imm)),
                MInst::AndImm32 { src, imm, .. } => Some(get(src).masked(u64::from(imm))),
                MInst::ShrImm { src, imm, .. } => Some(get(src).shifted(-i16::from(imm))),
                MInst::ShlImm { src, imm, .. } => Some(get(src).shifted(i16::from(imm))),
                MInst::BitExtract {
                    src, lsb, width, ..
                } if width > 0 && width <= 64 => Some(
                    get(src)
                        .shifted(-i16::from(lsb))
                        .masked(u64::MAX >> (64 - width)),
                ),
                MInst::Or { lhs, rhs, .. } => get(lhs).union(get(rhs)),
                MInst::Or32 { lhs, rhs, .. } => get(lhs)
                    .union(get(rhs))
                    .map(|value| value.masked(u64::from(u32::MAX))),
                MInst::OrShifted {
                    lhs, rhs, shift, ..
                } => get(lhs).union(get(rhs).shifted(i16::from(shift))),
                MInst::BitInsert {
                    base,
                    src,
                    lsb,
                    width,
                    ..
                } if width > 0 && u16::from(lsb) + u16::from(width) <= 64 => {
                    let mask = u64::MAX >> (64 - width);
                    get(base)
                        .masked(!(mask << lsb))
                        .union(get(src).masked(mask).shifted(i16::from(lsb)))
                }
                _ => None,
            };
            if let Some(projection) = projection {
                if let Some(replacement) = projection.instruction(dst) {
                    *inst = replacement;
                }
                projections.insert(dst, projection);
            }
        }
    }
}
