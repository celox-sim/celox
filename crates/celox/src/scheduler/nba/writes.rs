//! Logical-position stores for queued NBA ranges, independent of array stride.
use super::*;

#[derive(Clone, Copy)]
pub(in crate::scheduler) struct Object {
    pub signal: AbsoluteAddr,
    pub width: usize,
    pub element: Option<usize>,
}

impl Object {
    fn offset(self, g: &mut Generator<'_>, bit: RegisterId) -> SIROffset {
        match self.element {
            None => SIROffset::Dynamic(bit),
            Some(width) => {
                let width_reg = g.constant(width as u64, 64);
                let index = g.binary(64, bit, BinaryOp::DivU, width_reg);
                let within = g.binary(64, bit, BinaryOp::RemU, width_reg);
                SIROffset::Element {
                    index,
                    element_width: width,
                    bit_offset: 0,
                    dynamic_bit_offset: Some(within),
                }
            }
        }
    }

    fn capacity(self, g: &mut Generator<'_>, bit: RegisterId, remaining: RegisterId) -> RegisterId {
        let Some(width) = self.element else {
            return remaining;
        };
        let width = g.constant(width as u64, 64);
        let within = g.binary(64, bit, BinaryOp::RemU, width);
        let available = g.binary(64, width, BinaryOp::Sub, within);
        min(g, remaining, available)
    }

    fn load(self, g: &mut Generator<'_>, bit: RegisterId, width: usize) -> RegisterId {
        let result = if g.keeps_unknowns(self.signal) {
            g.b.alloc_logic(width)
        } else {
            g.b.alloc_bit(width, false)
        };
        let offset = self.offset(g, bit);
        g.b.emit(SIRInstruction::Load(
            result,
            stable(self.signal),
            offset,
            width,
        ));
        result
    }

    fn store(self, g: &mut Generator<'_>, bit: RegisterId, width: usize, value: RegisterId) {
        let logic = matches!(g.b.register(&value), celox_sir::RegisterType::Logic { .. });
        let value = match (g.keeps_unknowns(self.signal), logic) {
            (true, false) => {
                let zero = g.b.alloc_logic(width);
                g.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                let known = g.b.alloc_logic(width);
                g.b.emit(SIRInstruction::Binary(known, value, BinaryOp::Or, zero));
                known
            }
            (false, true) => {
                let known = g.b.alloc_bit(width, false);
                g.b.emit(SIRInstruction::Unary(
                    known,
                    celox_design::UnaryOp::ToTwoState,
                    value,
                ));
                known
            }
            _ => value,
        };
        let offset = self.offset(g, bit);
        if let Some((_, blocks)) = &mut g.partitioned_glue {
            blocks.insert(g.b.current_block());
        }
        g.b.emit(SIRInstruction::Store(
            stable(self.signal),
            offset,
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }
}

fn min(g: &mut Generator<'_>, left: RegisterId, right: RegisterId) -> RegisterId {
    let less = g.binary(1, left, BinaryOp::LtU, right);
    let result = g.b.alloc_bit(64, false);
    g.b.emit(SIRInstruction::Mux(result, less, left, right));
    result
}

/// Read at most 64 bits from a captured value/mask plane. A second word is
/// loaded only when this logical range crosses a word boundary.
fn plane(
    g: &mut Generator<'_>,
    record: RegisterId,
    offset: RegisterId,
    width: usize,
    base: usize,
) -> RegisterId {
    let six = g.constant(6, 64);
    let word = g.binary(64, offset, BinaryOp::Shr, six);
    let base = g.constant(base as u64, 64);
    let word = g.binary(64, word, BinaryOp::Add, base);
    let mask = g.constant(63, 64);
    let bit = g.binary(64, offset, BinaryOp::And, mask);
    let low = g.arena_load(record, word);
    let low = g.binary(64, low, BinaryOp::Shr, bit);
    if width == 1 {
        return low;
    }
    let threshold = g.constant((64 - width) as u64, 64);
    let crosses = g.binary(1, bit, BinaryOp::GtU, threshold);
    let next = g.b.new_block();
    let result = g.b.alloc_bit(64, false);
    let done = g.b.new_block_with(vec![result]);
    g.b.seal_block(SIRTerminator::Branch {
        cond: crosses,
        true_block: (next, Vec::new()),
        false_block: (done, vec![low]),
    });
    g.b.switch_to_block(next);
    let one = g.constant(1, 64);
    let word = g.binary(64, word, BinaryOp::Add, one);
    let high = g.arena_load(record, word);
    let sixty_four = g.constant(64, 64);
    let shift = g.binary(64, sixty_four, BinaryOp::Sub, bit);
    let high = g.binary(64, high, BinaryOp::Shl, shift);
    let merged = g.binary(64, low, BinaryOp::Or, high);
    g.b.seal_block(SIRTerminator::Jump(done, vec![merged]));
    g.b.switch_to_block(done);
    result
}

fn value(
    g: &mut Generator<'_>,
    record: RegisterId,
    source: Object,
    offset: RegisterId,
    width: usize,
) -> RegisterId {
    let bits = plane(g, record, offset, width, VALUE);
    let value = if g.keeps_unknowns(source.signal) {
        let mask = plane(g, record, offset, width, VALUE + source.width.div_ceil(64));
        let packed = g.b.alloc_bit(128, false);
        g.b.emit(SIRInstruction::Concat(packed, vec![mask, bits]));
        let value = g.b.alloc_logic(64);
        g.b.emit(SIRInstruction::Unary(
            value,
            celox_design::UnaryOp::FromPlanes,
            packed,
        ));
        value
    } else {
        bits
    };
    if width == 64 {
        value
    } else {
        let result = if g.keeps_unknowns(source.signal) {
            g.b.alloc_logic(width)
        } else {
            g.b.alloc_bit(width, false)
        };
        g.b.emit(SIRInstruction::Slice(result, value, 0, width));
        result
    }
}

/// Caller clips width to both the staged value and the destination object.
/// Full native chunks are preferred; only short tails use smaller stores.
pub(in crate::scheduler) fn write(
    g: &mut Generator<'_>,
    source: Object,
    target: Object,
    record: Option<RegisterId>,
    lsb: RegisterId,
    width: RegisterId,
) -> RegisterId {
    let offset = g.b.alloc_bit(64, false);
    let changed = g.b.alloc_bit(1, false);
    let loop_head = g.b.new_block_with(vec![offset, changed]);
    let result = g.b.alloc_bit(1, false);
    let done = g.b.new_block_with(vec![result]);
    let dispatch = g.b.new_block();
    let zero = g.constant(0, 64);
    let clean = g.constant(0, 1);
    g.b.seal_block(SIRTerminator::Jump(loop_head, vec![zero, clean]));
    g.b.switch_to_block(loop_head);
    let finished = g.binary(1, offset, BinaryOp::Eq, width);
    g.b.seal_block(SIRTerminator::Branch {
        cond: finished,
        true_block: (done, vec![changed]),
        false_block: (dispatch, Vec::new()),
    });
    g.b.switch_to_block(dispatch);
    let address = g.binary(64, lsb, BinaryOp::Add, offset);
    let remaining = g.binary(64, width, BinaryOp::Sub, offset);
    let mut capacity = target.capacity(g, address, remaining);
    if record.is_none() {
        capacity = source.capacity(g, offset, capacity);
    }
    let max = target.element.unwrap_or(64).min(if record.is_none() {
        source.element.unwrap_or(64)
    } else {
        64
    });
    let mut chunks = vec![64, 32, 16, 8, 4, 2, 1];
    chunks.extend([source.width.min(64), source.width % 64]);
    chunks.extend(target.element);
    if record.is_none() {
        chunks.extend(source.element);
    }
    chunks.retain(|&bits| bits > 0 && bits <= 64 && bits <= max);
    chunks.sort_unstable_by(|left, right| right.cmp(left));
    chunks.dedup();
    for bits in chunks {
        let write = g.b.new_block();
        let smaller = g.b.new_block();
        let amount = g.constant(bits as u64, 64);
        let fits = g.binary(1, capacity, BinaryOp::GeU, amount);
        g.b.seal_block(SIRTerminator::Branch {
            cond: fits,
            true_block: (write, Vec::new()),
            false_block: (smaller, Vec::new()),
        });
        g.b.switch_to_block(write);
        let old = target.load(g, address, bits);
        let value = match record {
            Some(record) => value(g, record, source, offset, bits),
            None => source.load(g, offset, bits),
        };
        target.store(g, address, bits, value);
        let new = target.load(g, address, bits);
        let differs = g.binary(1, old, BinaryOp::NeCase, new);
        let any = g.binary(1, changed, BinaryOp::Or, differs);
        let next = g.binary(64, offset, BinaryOp::Add, amount);
        g.b.seal_block(SIRTerminator::Jump(loop_head, vec![next, any]));
        g.b.switch_to_block(smaller);
    }
    let invalid = g.exit_block(SchedulerExit::Internal);
    g.b.seal_block(SIRTerminator::Jump(invalid, Vec::new()));
    g.b.switch_to_block(done);
    result
}

/// Restrict physical stores to bits inside the target, retaining the original
/// queued range in the record for portable host-boundary snapshots.
pub(super) fn clip(
    g: &mut Generator<'_>,
    lsb: RegisterId,
    width: RegisterId,
    target_width: usize,
) -> RegisterId {
    let bound = g.constant(target_width as u64, 64);
    let inside = g.binary(1, lsb, BinaryOp::LtU, bound);
    let left = g.binary(64, bound, BinaryOp::Sub, lsb);
    let clipped = min(g, width, left);
    let zero = g.constant(0, 64);
    let result = g.b.alloc_bit(64, false);
    g.b.emit(SIRInstruction::Mux(result, inside, clipped, zero));
    result
}
