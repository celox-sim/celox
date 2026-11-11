//! Operator and call lowering shared by the cell and glue expression lowerings.

use super::*;

pub(super) fn parameter_value_bits(value: i128, width: usize) -> BigUint {
    let modulus = BigUint::from(1u8) << width;
    if value >= 0 {
        BigUint::from(value as u128) % modulus
    } else {
        let remainder = BigUint::from(value.unsigned_abs()) % &modulus;
        if remainder == BigUint::default() {
            remainder
        } else {
            modulus - remainder
        }
    }
}

/// A [`sv::ir::Callee::Stream`] call as slices and concatenations of its
/// arguments, each lowered once. `lower` lowers an operand with a context
/// width and signedness.
pub(super) fn lower_stream<A: std::hash::Hash + Eq + Clone>(
    args: &[Option<sv::ir::Expr>],
    stream: &sv::ir::Stream,
    arena: &mut SLTNodeArena<A>,
    lower: &mut dyn FnMut(
        &sv::ir::Expr,
        &mut SLTNodeArena<A>,
        Option<usize>,
        Option<bool>,
    ) -> Option<NodeId>,
) -> Option<NodeId> {
    fn slice<A: std::hash::Hash + Eq + Clone>(
        arena: &mut SLTNodeArena<A>,
        expr: NodeId,
        (lsb, width): (usize, usize),
    ) -> Option<NodeId> {
        if lsb == 0 && width == celox_slt::get_width(expr, arena) {
            return Some(expr);
        }
        arena
            .alloc(SLTNode::Slice {
                expr,
                access: BitAccess::new(lsb, lsb + width - 1),
            })
            .ok()
    }
    // The ranges of `expr`, the first in the most significant bits.
    fn gather<A: std::hash::Hash + Eq + Clone>(
        arena: &mut SLTNodeArena<A>,
        expr: NodeId,
        ranges: Vec<(usize, usize)>,
    ) -> Option<NodeId> {
        if let [range] = ranges.as_slice() {
            return slice(arena, expr, *range);
        }
        let parts = ranges
            .into_iter()
            .map(|range| Some((slice(arena, expr, range)?, range.1)))
            .collect::<Option<Vec<_>>>()?;
        arena.alloc(SLTNode::Concat(parts)).ok()
    }
    let mut parts = Vec::with_capacity(args.len());
    for (arg, layout) in args.iter().zip(&stream.args) {
        // An operand is evaluated in its own self-determined type (11.4.14.1).
        let node = lower(arg.as_ref()?, arena, None, None)?;
        let node = coerce_node_width(arena, node, Some(layout.width), false).ok()?;
        let bits = layout.bits.unwrap_or((0, layout.width));
        let node = slice(arena, node, bits)?;
        parts.push((gather(arena, node, layout.elements(bits.1))?, bits.1));
    }
    let generic = match parts.as_slice() {
        [(node, _)] => *node,
        _ => arena.alloc(SLTNode::Concat(parts)).ok()?,
    };
    let (width, stream_width) = (stream.width(), stream.stream_width());
    let ordered = if stream.unpack {
        let source = stream.args.first()?.width;
        let packed = slice(
            arena,
            generic,
            (source.checked_sub(stream_width)?, stream_width),
        )?;
        // The packed stream holds the blocks most significant first: put
        // each back at its place in the stream.
        let mut lsb = stream_width;
        let mut blocks = stream
            .blocks(stream_width)
            .into_iter()
            .map(|(block, size)| {
                lsb -= size;
                (block, (lsb, size))
            })
            .collect::<Vec<_>>();
        blocks.sort_by_key(|block| std::cmp::Reverse(block.0));
        gather(
            arena,
            packed,
            blocks.into_iter().map(|(_, range)| range).collect(),
        )?
    } else {
        let ordered = gather(arena, generic, stream.blocks(stream_width))?;
        if width > stream_width {
            let fill = width - stream_width;
            let zeros = arena
                .alloc(SLTNode::Constant(
                    BigUint::default(),
                    BigUint::default(),
                    fill,
                    false,
                ))
                .ok()?;
            arena
                .alloc(SLTNode::Concat(vec![
                    (ordered, stream_width),
                    (zeros, fill),
                ]))
                .ok()?
        } else {
            ordered
        }
    };
    if stream.targets.iter().all(|target| target.element.is_none()) {
        return Some(ordered);
    }
    let mut lsb = width;
    let mut parts = Vec::with_capacity(stream.targets.len());
    for target in &stream.targets {
        lsb -= target.width;
        let part = slice(arena, ordered, (lsb, target.width))?;
        parts.push((
            gather(arena, part, target.elements(target.width))?,
            target.width,
        ));
    }
    arena.alloc(SLTNode::Concat(parts)).ok()
}

/// A [`sv::ir::Callee::Lookup`] call as nested multiplexers of case
/// equalities. The selector is lowered once and its node shared by every
/// comparison, so nested lookups lower in linear time. `lower` lowers an
/// operand with a context width and signedness.
pub(super) fn lower_lookup<A: std::hash::Hash + Eq + Clone>(
    args: &[Option<sv::ir::Expr>],
    width: usize,
    signed: bool,
    arena: &mut SLTNodeArena<A>,
    lower: &mut dyn FnMut(
        &sv::ir::Expr,
        &mut SLTNodeArena<A>,
        Option<usize>,
        Option<bool>,
    ) -> Option<NodeId>,
) -> Option<NodeId> {
    let (selector, rest) = args.split_first()?;
    let (default, pairs) = rest.split_last()?;
    let (pairs, []) = pairs.as_chunks::<2>() else {
        return None;
    };
    let selector = lower(selector.as_ref()?, arena, None, None)?;
    let resized = |expr: &Option<sv::ir::Expr>| {
        Some(sv::ir::Expr::Resize {
            expr: Box::new(expr.clone()?),
            width,
            signed,
        })
    };
    let mut result = lower(&resized(default)?, arena, Some(width), Some(signed))?;
    for [key, value] in pairs.iter().rev() {
        let key = lower(key.as_ref()?, arena, None, None)?;
        let compared = celox_slt::get_width(selector, arena).max(celox_slt::get_width(key, arena));
        let left = coerce_node_width(arena, selector, Some(compared), false).ok()?;
        let right = coerce_node_width(arena, key, Some(compared), false).ok()?;
        let cond = arena
            .alloc(SLTNode::Binary(left, BinaryOp::EqCase, right))
            .ok()?;
        let then_expr = lower(&resized(value)?, arena, Some(width), Some(signed))?;
        result = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr,
                else_expr: result,
            })
            .ok()?;
    }
    Some(result)
}

/// `expr inside { items }` as the comparisons the language defines: a value
/// matches by wildcard equality, a range by an inclusive bounds check.
pub(super) fn inside_as_comparisons(
    expr: &sv::ir::Expr,
    items: &[sv::ir::InsideItem],
) -> sv::ir::Expr {
    let compare =
        |left: sv::ir::Expr, op: sv::ir::BinaryOp, right: sv::ir::Expr| sv::ir::Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
    items
        .iter()
        .map(|item| match item {
            sv::ir::InsideItem::Value(value) => {
                compare(expr.clone(), sv::ir::BinaryOp::EqWildcard, value.clone())
            }
            sv::ir::InsideItem::Range { low, high } => compare(
                compare(expr.clone(), sv::ir::BinaryOp::Ge, low.clone()),
                sv::ir::BinaryOp::LogicAnd,
                compare(expr.clone(), sv::ir::BinaryOp::Le, high.clone()),
            ),
        })
        .reduce(|left, right| compare(left, sv::ir::BinaryOp::LogicOr, right))
        .unwrap_or_else(|| sv::ir::Expr::Literal("1'b0".to_string()))
}

// IEEE 1800-2023 20.9: bit vector functions return known counts/predicates.
pub(super) fn lower_bit_vector_function_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    function: sv::ir::SystemFunction,
    operands: &[NodeId],
    context_width: Option<usize>,
    context_signed: Option<bool>,
) -> Option<NodeId> {
    let inner = *operands.first()?;
    let known = arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, inner))
        .ok()?;
    use sv::ir::SystemFunction;
    let (width, signed) = function.result_type()?;
    let result = match function {
        // The analyzer folds coverage calls to literals; none reach here.
        SystemFunction::CoverageControl
        | SystemFunction::CoverageGetMax
        | SystemFunction::CoverageGet
        | SystemFunction::CoverageMerge
        | SystemFunction::CoverageSave
        | SystemFunction::GetCoverage => return None,
        SystemFunction::Process(_) => return None,
        SystemFunction::Countbits => {
            let controls = operands[1..]
                .iter()
                .map(|&expr| {
                    arena
                        .alloc(SLTNode::Slice {
                            expr,
                            access: BitAccess::new(0, 0),
                        })
                        .ok()
                })
                .collect::<Option<Vec<_>>>()?;
            let operand_width = celox_slt::get_width(inner, arena);
            let mut matches = Vec::with_capacity(operand_width);
            for bit in (0..operand_width).rev() {
                let selected = arena
                    .alloc(SLTNode::Slice {
                        expr: inner,
                        access: BitAccess::new(bit, bit),
                    })
                    .ok()?;
                let mut matched = None;
                for &control in &controls {
                    // Case equality distinguishes all four states and returns a
                    // known bit. OR makes repeated controls count only once.
                    let equal = arena
                        .alloc(SLTNode::Binary(selected, BinaryOp::EqCase, control))
                        .ok()?;
                    matched = Some(match matched {
                        None => equal,
                        Some(previous) => arena
                            .alloc(SLTNode::Binary(previous, BinaryOp::Or, equal))
                            .ok()?,
                    });
                }
                matches.push((matched?, 1));
            }
            let matching_bits = arena.alloc(SLTNode::Concat(matches)).ok()?;
            let count = arena
                .alloc(SLTNode::Unary(UnaryOp::PopCount, matching_bits))
                .ok()?;
            coerce_node_width(arena, count, Some(32), false).ok()?
        }
        SystemFunction::Clog2 => {
            // ceil(log2(x)): the bit length of x - 1, and 0 for x <= 1
            // (IEEE 1800-2023 20.8.1).
            let operand_width = celox_slt::get_width(known, arena);
            let constant = |arena: &mut SLTNodeArena<A>, value: usize, width: usize| {
                arena
                    .alloc(SLTNode::Constant(
                        BigUint::from(value),
                        BigUint::default(),
                        width,
                        false,
                    ))
                    .ok()
            };
            let one = constant(arena, 1, operand_width)?;
            let decremented = arena
                .alloc(SLTNode::Binary(known, BinaryOp::Sub, one))
                .ok()?;
            let zeros = arena
                .alloc(SLTNode::Unary(UnaryOp::CountLeadingZeros, decremented))
                .ok()?;
            let zeros = coerce_node_width(arena, zeros, Some(32), false).ok()?;
            let total = constant(arena, operand_width, 32)?;
            let bits = arena
                .alloc(SLTNode::Binary(total, BinaryOp::Sub, zeros))
                .ok()?;
            let small = arena
                .alloc(SLTNode::Binary(known, BinaryOp::LeU, one))
                .ok()?;
            let zero = constant(arena, 0, 32)?;
            arena
                .alloc(SLTNode::Mux {
                    cond: small,
                    then_expr: zero,
                    else_expr: bits,
                })
                .ok()?
        }
        SystemFunction::Isunknown => {
            // Case inequality detects either X or Z, including unknown bits whose
            // value plane is zero and would disappear during two-state conversion.
            arena
                .alloc(SLTNode::Binary(inner, BinaryOp::NeCase, known))
                .ok()?
        }
        SystemFunction::Countones | SystemFunction::Onehot | SystemFunction::Onehot0 => {
            let count = arena.alloc(SLTNode::Unary(UnaryOp::PopCount, known)).ok()?;
            // PopCount has a minimal unsigned width; $countones returns signed int.
            let count = coerce_node_width(arena, count, Some(32), false).ok()?;
            if function == SystemFunction::Countones {
                count
            } else {
                let one = arena
                    .alloc(SLTNode::Constant(
                        BigUint::from(1u8),
                        BigUint::default(),
                        32,
                        false,
                    ))
                    .ok()?;
                let op = if function == SystemFunction::Onehot {
                    BinaryOp::Eq
                } else {
                    BinaryOp::LeU
                };
                arena.alloc(SLTNode::Binary(count, op, one)).ok()?
            }
        }
        // The random number and time functions need process state, and the
        // analyzer folds the other functions to constants.
        SystemFunction::Ln
        | SystemFunction::Log10
        | SystemFunction::Exp
        | SystemFunction::Sqrt
        | SystemFunction::Floor
        | SystemFunction::Ceil
        | SystemFunction::Sin
        | SystemFunction::Cos
        | SystemFunction::Tan
        | SystemFunction::Asin
        | SystemFunction::Acos
        | SystemFunction::Atan
        | SystemFunction::Sinh
        | SystemFunction::Cosh
        | SystemFunction::Tanh
        | SystemFunction::Asinh
        | SystemFunction::Acosh
        | SystemFunction::Atanh
        | SystemFunction::Pow
        | SystemFunction::Atan2
        | SystemFunction::Hypot
        | SystemFunction::Rtoi
        | SystemFunction::Itor
        | SystemFunction::Realtobits
        | SystemFunction::Bitstoreal
        | SystemFunction::Shortrealtobits
        | SystemFunction::Bitstoshortreal
        | SystemFunction::Time
        | SystemFunction::Stime
        | SystemFunction::Realtime
        | SystemFunction::Random
        | SystemFunction::DistUniform
        | SystemFunction::DistNormal
        | SystemFunction::DistExponential
        | SystemFunction::DistPoisson
        | SystemFunction::DistChiSquare
        | SystemFunction::DistT
        | SystemFunction::DistErlang
        | SystemFunction::TestPlusargs
        | SystemFunction::ValuePlusargs
        | SystemFunction::Urandom
        | SystemFunction::UrandomRange
        | SystemFunction::System
        | SystemFunction::Fopen
        | SystemFunction::ScopeRandomize
        | SystemFunction::ScopeRandomizeEnum
        | SystemFunction::Fread
        | SystemFunction::Fgets
        | SystemFunction::Ferror
        | SystemFunction::Ftell
        | SystemFunction::Fseek
        | SystemFunction::Rewind
        | SystemFunction::Fgetc
        | SystemFunction::Ungetc
        | SystemFunction::Feof
        | SystemFunction::Sscanf
        | SystemFunction::Fscanf
        | SystemFunction::Timeunit
        | SystemFunction::Timeprecision
        | SystemFunction::Isunbounded
        | SystemFunction::Signed
        | SystemFunction::Unsigned
        | SystemFunction::Sampled
        | SystemFunction::Past
        | SystemFunction::Rose
        | SystemFunction::Fell
        | SystemFunction::Stable
        | SystemFunction::Changed
        | SystemFunction::Typename
        | SystemFunction::Bits
        | SystemFunction::Size
        | SystemFunction::Dimensions
        | SystemFunction::UnpackedDimensions
        | SystemFunction::ArrayQuery(_) => return None,
    };
    coerce_node_width(
        arena,
        result,
        Some(context_width.unwrap_or(width)),
        context_signed.unwrap_or(signed),
    )
    .ok()
}

pub(super) fn unary_op_from_sv(op: sv::ir::UnaryOp) -> Option<UnaryOp> {
    match op {
        sv::ir::UnaryOp::Plus => Some(UnaryOp::Ident),
        sv::ir::UnaryOp::Minus => Some(UnaryOp::Minus),
        sv::ir::UnaryOp::BitNot => Some(UnaryOp::BitNot),
        sv::ir::UnaryOp::LogicNot => Some(UnaryOp::LogicNot),
        sv::ir::UnaryOp::ToTwoState => Some(UnaryOp::ToTwoState),
        sv::ir::UnaryOp::RedAnd => Some(UnaryOp::And),
        sv::ir::UnaryOp::RedOr => Some(UnaryOp::Or),
        sv::ir::UnaryOp::RedXor => Some(UnaryOp::Xor),
    }
}

pub(super) fn binary_op_from_sv(op: sv::ir::BinaryOp, operands_signed: bool) -> Option<BinaryOp> {
    Some(match op {
        // Exponentiation exists only in constant expressions; hardware for a
        // run-time exponent is not lowered.
        sv::ir::BinaryOp::Pow => return None,
        sv::ir::BinaryOp::Add => BinaryOp::Add,
        sv::ir::BinaryOp::Sub => BinaryOp::Sub,
        sv::ir::BinaryOp::Mul => BinaryOp::Mul,
        sv::ir::BinaryOp::Div if operands_signed => BinaryOp::DivS,
        sv::ir::BinaryOp::Div => BinaryOp::DivU,
        sv::ir::BinaryOp::Mod if operands_signed => BinaryOp::RemS,
        sv::ir::BinaryOp::Mod => BinaryOp::RemU,
        sv::ir::BinaryOp::Shl => BinaryOp::Shl,
        sv::ir::BinaryOp::Shr => BinaryOp::Shr,
        sv::ir::BinaryOp::Sar if operands_signed => BinaryOp::Sar,
        sv::ir::BinaryOp::Sar => BinaryOp::Shr,
        sv::ir::BinaryOp::BitAnd => BinaryOp::And,
        sv::ir::BinaryOp::BitOr => BinaryOp::Or,
        sv::ir::BinaryOp::BitXor => BinaryOp::Xor,
        sv::ir::BinaryOp::LogicAnd => BinaryOp::LogicAnd,
        sv::ir::BinaryOp::LogicOr => BinaryOp::LogicOr,
        sv::ir::BinaryOp::Eq => BinaryOp::Eq,
        sv::ir::BinaryOp::Ne => BinaryOp::Ne,
        sv::ir::BinaryOp::EqCase => BinaryOp::EqCase,
        sv::ir::BinaryOp::NeCase => BinaryOp::NeCase,
        sv::ir::BinaryOp::EqWildcard => BinaryOp::EqWildcard,
        sv::ir::BinaryOp::NeWildcard => BinaryOp::NeWildcard,
        sv::ir::BinaryOp::Lt if operands_signed => BinaryOp::LtS,
        sv::ir::BinaryOp::Lt => BinaryOp::LtU,
        sv::ir::BinaryOp::Le if operands_signed => BinaryOp::LeS,
        sv::ir::BinaryOp::Le => BinaryOp::LeU,
        sv::ir::BinaryOp::Gt if operands_signed => BinaryOp::GtS,
        sv::ir::BinaryOp::Gt => BinaryOp::GtU,
        sv::ir::BinaryOp::Ge if operands_signed => BinaryOp::GeS,
        sv::ir::BinaryOp::Ge => BinaryOp::GeU,
    })
}
