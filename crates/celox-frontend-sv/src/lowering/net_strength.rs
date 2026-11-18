//! The resolution of drivers of different strengths (IEEE 1800-2023
//! 28.11–28.12).
//!
//! A signal of a bit is a range of strength levels on the continuum
//! `Su0 St0 Pu0 La0 We0 Me0 Sm0 HiZ0 HiZ1 Sm1 Me1 We1 La1 Pu1 St1 Su1`
//! (Figure 28-5), numbered 0 to 15 here: a level `q` of the 0 side has
//! strength `7 - q`, one of the 1 side strength `q - 8`. A driver with
//! strengths `s0` and `s1` drives 0 as the level `7 - s0`, 1 as `8 + s1`,
//! x as the range between them and z as the range `HiZ0..HiZ1`.
//!
//! Two signals combine into the hull of the results of combining each pair
//! of their levels as signals of one level: the stronger level, or, of equal
//! strengths and opposite values, the x of that strength with all weaker
//! levels (28.12.1–28.12.3). A level of one signal is in that hull exactly
//! when it is at least as strong as the weakest level of the other signal.
//! The combination is commutative and associative, so the drivers fold in
//! any order.
//!
//! Each end of a range is four bit planes, one bit of the level of each bit
//! of the net, so a vector net resolves bit-parallel. `15 - q` is the
//! complement of the planes.
use super::procedural::slt_error;
use super::*;
use num_bigint::BigUint;
use sv::ir::DriveStrength;

/// A level of each bit: four planes, the binary digits of the level.
#[derive(Clone, Copy)]
struct Level([NodeId; 4]);

struct Planes<'a> {
    arena: &'a mut SLTNodeArena<SourceVarId>,
    width: usize,
}

impl Planes<'_> {
    fn node(&mut self, node: SLTNode<SourceVarId>) -> Result<NodeId, sv::AnalyzerError> {
        self.arena.alloc(node).map_err(slt_error)
    }

    fn binary(
        &mut self,
        left: NodeId,
        op: BinaryOp,
        right: NodeId,
    ) -> Result<NodeId, sv::AnalyzerError> {
        self.node(SLTNode::Binary(left, op, right))
    }

    fn and(&mut self, left: NodeId, right: NodeId) -> Result<NodeId, sv::AnalyzerError> {
        self.binary(left, BinaryOp::And, right)
    }

    fn or(&mut self, left: NodeId, right: NodeId) -> Result<NodeId, sv::AnalyzerError> {
        self.binary(left, BinaryOp::Or, right)
    }

    fn not(&mut self, node: NodeId) -> Result<NodeId, sv::AnalyzerError> {
        self.node(SLTNode::Unary(UnaryOp::BitNot, node))
    }

    /// A plane with every bit `bit`.
    fn filled(&mut self, bit: bool) -> Result<NodeId, sv::AnalyzerError> {
        let value = if bit {
            (BigUint::from(1u8) << self.width) - 1u8
        } else {
            BigUint::default()
        };
        self.node(SLTNode::Constant(
            value,
            BigUint::default(),
            self.width,
            false,
        ))
    }

    /// The level `q` on every bit.
    fn constant(&mut self, q: u8) -> Result<Level, sv::AnalyzerError> {
        let mut planes = [NodeId(0); 4];
        for (digit, plane) in planes.iter_mut().enumerate() {
            *plane = self.filled(q >> digit & 1 == 1)?;
        }
        Ok(Level(planes))
    }

    /// The level `q` where `select` is set, and `other` elsewhere.
    fn choose(&mut self, select: NodeId, q: u8, other: Level) -> Result<Level, sv::AnalyzerError> {
        let level = self.constant(q)?;
        self.mux(select, level, other)
    }

    /// `then` on the bits where `select` is set, `other` elsewhere.
    fn mux(
        &mut self,
        select: NodeId,
        then: Level,
        other: Level,
    ) -> Result<Level, sv::AnalyzerError> {
        let unselected = self.not(select)?;
        let mut planes = [NodeId(0); 4];
        for (digit, plane) in planes.iter_mut().enumerate() {
            let taken = self.and(select, then.0[digit])?;
            let kept = self.and(unselected, other.0[digit])?;
            *plane = self.or(taken, kept)?;
        }
        Ok(Level(planes))
    }

    /// `15 - q`: the level of the same strength on the other side.
    fn mirror(&mut self, level: Level) -> Result<Level, sv::AnalyzerError> {
        let mut planes = [NodeId(0); 4];
        for (digit, plane) in planes.iter_mut().enumerate() {
            *plane = self.not(level.0[digit])?;
        }
        Ok(Level(planes))
    }

    /// The bits where `left <= right`.
    fn at_most(&mut self, left: Level, right: Level) -> Result<NodeId, sv::AnalyzerError> {
        // From the least significant digit up: `left <= right` holds when
        // the higher digit of `left` is smaller, or equal and the lower
        // digits compare so.
        let mut result = self.filled(true)?;
        for digit in 0..4 {
            let (l, r) = (left.0[digit], right.0[digit]);
            let not_l = self.not(l)?;
            let smaller = self.and(not_l, r)?;
            let differ = self.binary(l, BinaryOp::Xor, r)?;
            let same = self.not(differ)?;
            let kept = self.and(same, result)?;
            result = self.or(smaller, kept)?;
        }
        Ok(result)
    }

    fn min(&mut self, left: Level, right: Level) -> Result<Level, sv::AnalyzerError> {
        let select = self.at_most(left, right)?;
        self.mux(select, left, right)
    }

    fn max(&mut self, left: Level, right: Level) -> Result<Level, sv::AnalyzerError> {
        let select = self.at_most(left, right)?;
        self.mux(select, right, left)
    }

    /// The weakest level's distance from the strongest end of its side:
    /// 7 for a range that spans both sides, the high end of a range of 0
    /// levels, and the mirror of the low end of a range of 1 levels.
    fn weakest(&mut self, (low, high): (Level, Level)) -> Result<Level, sv::AnalyzerError> {
        let mirrored = self.mirror(low)?;
        let seven = self.constant(7)?;
        let one_side = self.mux(low.0[3], mirrored, seven)?;
        let zero_side = self.not(high.0[3])?;
        self.mux(zero_side, high, one_side)
    }

    /// The hull of the levels of `(low, high)` at least as strong as
    /// `weakest`, where the other signal's weakest level is `weakest` from
    /// its side's strongest end; `(15, 0)` where none is.
    fn survivors(
        &mut self,
        (low, high): (Level, Level),
        weakest: Level,
    ) -> Result<(Level, Level), sv::AnalyzerError> {
        let mirrored = self.mirror(weakest)?;
        // Levels of the 0 side at most `weakest`, of the 1 side at least
        // its mirror.
        let zeros = self.at_most(low, weakest)?;
        let ones = self.at_most(mirrored, high)?;
        let fifteen = self.constant(15)?;
        let zero = self.constant(0)?;
        let low_one = self.max(low, mirrored)?;
        let low_one = self.mux(ones, low_one, fifteen)?;
        let survivor_low = self.mux(zeros, low, low_one)?;
        let high_zero = self.min(high, weakest)?;
        let high_zero = self.mux(zeros, high_zero, zero)?;
        let survivor_high = self.mux(ones, high, high_zero)?;
        Ok((survivor_low, survivor_high))
    }

    fn combine(
        &mut self,
        left: (Level, Level),
        right: (Level, Level),
    ) -> Result<(Level, Level), sv::AnalyzerError> {
        let weakest_left = self.weakest(left)?;
        let weakest_right = self.weakest(right)?;
        let left = self.survivors(left, weakest_right)?;
        let right = self.survivors(right, weakest_left)?;
        Ok((self.min(left.0, right.0)?, self.max(left.1, right.1)?))
    }

    /// The range a driver of `strength` drives: `value` and `mask` are its
    /// planes, `mask` `None` for a two-state value.
    fn driven(
        &mut self,
        value: NodeId,
        mask: Option<NodeId>,
        strength: DriveStrength,
    ) -> Result<(Level, Level), sv::AnalyzerError> {
        let zero = 7 - strength.zero;
        let one = 8 + strength.one;
        let zero_level = self.constant(zero)?;
        let one_level = self.constant(one)?;
        // 1 starts at the 1 level, 0 and x at the 0 level.
        let mut low = self.mux(value, one_level, zero_level)?;
        let mut high = self.mux(value, one_level, zero_level)?;
        if let Some(mask) = mask {
            let not_value = self.not(value)?;
            let z = self.and(mask, not_value)?;
            let x = self.and(mask, value)?;
            low = self.mux(x, zero_level, low)?;
            high = self.mux(x, one_level, high)?;
            low = self.choose(z, 7, low)?;
            high = self.choose(z, 8, high)?;
        }
        Ok((low, high))
    }
}

/// The value of a net whose drivers' `values` drive it with `strengths`,
/// each `width` bits: with `planes`, as a four-state value, else as a
/// two-state one. A range of levels of both values, or of a value and high
/// impedance (L, H), is x; one of only high impedance is z.
pub(super) fn resolve(
    arena: &mut SLTNodeArena<SourceVarId>,
    values: &[NodeId],
    strengths: &[DriveStrength],
    width: usize,
    planes: bool,
) -> Result<NodeId, sv::AnalyzerError> {
    let padded = if planes {
        width.next_multiple_of(64)
    } else {
        width
    };
    let mut builder = Planes {
        arena,
        width: padded,
    };
    let mut resolved = None;
    for (&value, &strength) in values.iter().zip(strengths) {
        let value =
            coerce_node_width(builder.arena, value, Some(padded), false).map_err(slt_error)?;
        let (value, mask) = if planes {
            let both = builder.node(SLTNode::Unary(UnaryOp::ToPlanes, value))?;
            let value = builder.node(SLTNode::Slice {
                expr: both,
                access: BitAccess::new(0, padded - 1),
            })?;
            let mask = builder.node(SLTNode::Slice {
                expr: both,
                access: BitAccess::new(padded, 2 * padded - 1),
            })?;
            (value, Some(mask))
        } else {
            (value, None)
        };
        let driven = builder.driven(value, mask, strength)?;
        resolved = Some(match resolved {
            Some(previous) => builder.combine(previous, driven)?,
            None => driven,
        });
    }
    let (low, high) = resolved.expect("a driver");
    let six = builder.constant(6)?;
    let nine = builder.constant(9)?;
    let zero = builder.at_most(high, six)?;
    let one = builder.at_most(nine, low)?;
    if !planes {
        // A two-state net stores x and z as 0 (6.11.2).
        return Ok(one);
    }
    let seven = builder.constant(7)?;
    let eight = builder.constant(8)?;
    let above_zeros = builder.at_most(seven, low)?;
    let below_ones = builder.at_most(high, eight)?;
    let z = builder.and(above_zeros, below_ones)?;
    let known = builder.or(zero, one)?;
    let mask = builder.not(known)?;
    let not_z = builder.not(z)?;
    let x = builder.and(mask, not_z)?;
    let value = builder.or(one, x)?;
    let both = builder.node(SLTNode::Concat(vec![(mask, padded), (value, padded)]))?;
    let resolved = builder.node(SLTNode::Unary(UnaryOp::FromPlanes, both))?;
    if padded == width {
        Ok(resolved)
    } else {
        builder.node(SLTNode::Slice {
            expr: resolved,
            access: BitAccess::new(0, width - 1),
        })
    }
}
