//! SIR for watched signals and change-gated combinational units.
//!
//! A watched signal ([`EventWatch`]) counts its occurrences in counter
//! objects. [`emit_watched`] surrounds one store or commit to the signal
//! with the code that bumps those counters, so an occurrence is counted even
//! when a later store of the same execution overwrites it. Frontends use it
//! on every path that writes a watched signal: process kernels, flip-flop
//! evaluation and publication, and imperative combinational units. The code
//! is ordinary SIR, so every backend runs it unchanged.
//!
//! An instrumented combinational unit must only run when one of its inputs
//! changed (IEEE 1800-2023 9.2.2.2); otherwise re-running it, for example
//! `y = 0; if (a) y = 1;` with a steady `a`, would count a pulse on `y` at
//! every settle. [`gate_on_input_change`] adds that gate.

use celox_design::{BinaryOp, CounterKind, EVENT_COUNTER_WIDTH, EventWatch};
use celox_sir::{
    ExecutionUnit, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator, SIRValue,
};

/// A watched signal and its counters, addressed in the code being built.
#[derive(Clone, Debug)]
pub struct WatchCode<A> {
    /// The signal's stable state.
    pub signal: A,
    pub width: usize,
    /// Bit offset of the observed range within the signal.
    pub offset: SIROffset,
    /// Whether the signal is held in four-state form.
    pub four_state: bool,
    pub counters: Vec<(CounterKind, A)>,
}

impl<A> WatchCode<A> {
    /// The code view of `watch`, addressing each object through `address`.
    /// `four_state` is whether the build keeps the signal in four-state form.
    pub fn new<V: Copy>(watch: &EventWatch<V>, four_state: bool, address: impl Fn(V) -> A) -> Self {
        Self {
            signal: address(watch.signal),
            width: watch.width,
            offset: SIROffset::Static(0),
            four_state: four_state && watch.four_state,
            counters: watch
                .counters()
                .map(|(kind, counter)| (kind, address(counter)))
                .collect(),
        }
    }
}

fn load<A: Clone>(
    b: &mut SIRBuilder<A>,
    address: &A,
    width: usize,
    four_state: bool,
) -> RegisterId {
    load_bits(b, address, 0, width, four_state)
}

/// Load bits `lsb..lsb + width` of `address`.
fn load_bits<A: Clone>(
    b: &mut SIRBuilder<A>,
    address: &A,
    lsb: usize,
    width: usize,
    four_state: bool,
) -> RegisterId {
    load_range(b, address, SIROffset::Static(lsb), width, four_state)
}

fn load_range<A: Clone>(
    b: &mut SIRBuilder<A>,
    address: &A,
    offset: SIROffset,
    width: usize,
    four_state: bool,
) -> RegisterId {
    let register = if four_state {
        b.alloc_logic(width)
    } else {
        b.alloc_bit(width, false)
    };
    b.emit(SIRInstruction::Load(
        register,
        address.clone(),
        offset,
        width,
    ));
    register
}

fn binary(
    b: &mut SIRBuilder<impl Clone>,
    lhs: RegisterId,
    op: BinaryOp,
    rhs: RegisterId,
) -> RegisterId {
    let result = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Binary(result, lhs, op, rhs));
    result
}

/// Emit `write`, a store or commit that may change `watch.signal`, and
/// count the occurrence it causes. `value_type` is the type of a store's
/// source register.
pub fn emit_watched<A: Clone>(
    b: &mut SIRBuilder<A>,
    watch: &WatchCode<A>,
    write: SIRInstruction<A>,
    value_type: Option<&celox_sir::RegisterType>,
) {
    let old = load_range(
        b,
        &watch.signal,
        watch.offset.clone(),
        watch.width,
        watch.four_state,
    );
    // A store of the whole signal stores the new value itself; any other
    // write is read back.
    let stored = match &write {
        SIRInstruction::Store(_, SIROffset::Static(0), width, value, ..)
            if watch.offset == SIROffset::Static(0)
                && *width == watch.width
                && value_type
                    == Some(&if watch.four_state {
                        celox_sir::RegisterType::Logic { width: *width }
                    } else {
                        celox_sir::RegisterType::Bit {
                            width: *width,
                            signed: false,
                        }
                    }) =>
        {
            Some(*value)
        }
        _ => None,
    };
    // Everything derived from the old value is computed before the write,
    // so no backend can re-read the written state for it.
    let old_lsb = lsb(b, old, watch.width, watch.four_state);
    b.emit(write);
    if watch.counters.is_empty() {
        return;
    }
    let new = match stored {
        Some(value) => value,
        None => load_range(
            b,
            &watch.signal,
            watch.offset.clone(),
            watch.width,
            watch.four_state,
        ),
    };
    let new_lsb = lsb(b, new, watch.width, watch.four_state);
    for &(kind, ref counter) in &watch.counters {
        let occurred = occurrence(
            b,
            kind,
            (old, old_lsb),
            (new, new_lsb),
            watch.width,
            watch.four_state,
        );
        bump(b, counter, occurred);
    }
}

/// The least significant bit of `value`, kept at the signal's width: the
/// value masked with one. A `Slice` would do, but the native backends may
/// re-read a sliced load from state, which the write has changed.
fn lsb<A>(b: &mut SIRBuilder<A>, value: RegisterId, width: usize, four_state: bool) -> RegisterId {
    let one = constant(b, 1, width, four_state);
    let bit = if four_state {
        b.alloc_logic(width)
    } else {
        b.alloc_bit(width, false)
    };
    b.emit(SIRInstruction::Binary(bit, value, BinaryOp::And, one));
    bit
}

fn constant<A>(b: &mut SIRBuilder<A>, value: u8, width: usize, four_state: bool) -> RegisterId {
    let register = if four_state {
        b.alloc_logic(width)
    } else {
        b.alloc_bit(width, false)
    };
    b.emit(SIRInstruction::Imm(register, SIRValue::new(value)));
    register
}

/// Whether the change from `old` to `new`, each a value and its least
/// significant bit, is an occurrence of `kind`.
fn occurrence<A: Clone>(
    b: &mut SIRBuilder<A>,
    kind: CounterKind,
    (old, old_lsb): (RegisterId, RegisterId),
    (new, new_lsb): (RegisterId, RegisterId),
    width: usize,
    four_state: bool,
) -> RegisterId {
    if kind == CounterKind::Changes {
        // Case inequality distinguishes every unknown bit as well.
        return binary(b, old, BinaryOp::NeCase, new);
    }
    // IEEE 1800-2023 Table 9-2: a positive edge leaves 0 or reaches 1, a
    // negative edge leaves 1 or reaches 0, and either changes the bit. In
    // two-state form this is `old == 0 && new == 1` for a positive edge.
    let (from, to) = if kind == CounterKind::Rises {
        (0, 1)
    } else {
        (1, 0)
    };
    let from = constant(b, from, width, four_state);
    let to = constant(b, to, width, four_state);
    let changed = binary(b, old_lsb, BinaryOp::NeCase, new_lsb);
    let left = binary(b, old_lsb, BinaryOp::EqCase, from);
    let reached = binary(b, new_lsb, BinaryOp::EqCase, to);
    let either = binary(b, left, BinaryOp::Or, reached);
    binary(b, changed, BinaryOp::And, either)
}

/// Add `occurred`, a one-bit two-state register, to the counter at
/// `counter`.
fn bump<A: Clone>(b: &mut SIRBuilder<A>, counter: &A, occurred: RegisterId) {
    let value = load(b, counter, EVENT_COUNTER_WIDTH, false);
    let one = b.alloc_bit(EVENT_COUNTER_WIDTH, false);
    b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
    let incremented = b.alloc_bit(EVENT_COUNTER_WIDTH, false);
    b.emit(SIRInstruction::Binary(
        incremented,
        value,
        BinaryOp::Add,
        one,
    ));
    let next = b.alloc_bit(EVENT_COUNTER_WIDTH, false);
    b.emit(SIRInstruction::Mux(next, occurred, incremented, value));
    b.emit(SIRInstruction::Store(
        counter.clone(),
        SIROffset::Static(0),
        EVENT_COUNTER_WIDTH,
        next,
        Vec::new(),
        Vec::new(),
    ));
}

/// One input of a change-gated unit: bits `lsb..lsb + width` of the input's
/// stable state and a private object of that width and representation that
/// remembers their value.
#[derive(Clone, Debug)]
pub struct GateInput<A> {
    pub input: A,
    pub lsb: usize,
    pub shadow: A,
    pub width: usize,
    pub four_state: bool,
}

/// Wrap `unit` so that it runs on its first execution and afterwards only
/// when an input differs from its value at the previous run. `ran` is a
/// private one-bit two-state object, zero before the first run.
///
/// Inputs are remembered before the body runs, so the unit's own writes do
/// not re-trigger it; a frontend excludes the variables the unit writes from
/// `inputs` (IEEE 1800-2023 9.2.2.2.1).
pub fn gate_on_input_change<A: Clone>(
    unit: &ExecutionUnit<A>,
    inputs: &[GateInput<A>],
    ran: &A,
) -> ExecutionUnit<A> {
    let mut b = SIRBuilder::new();
    let has_run = load(&mut b, ran, 1, false);
    let first = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Unary(
        first,
        celox_design::UnaryOp::LogicNot,
        has_run,
    ));
    let mut run = first;
    let mut currents = Vec::with_capacity(inputs.len());
    for input in inputs {
        let current = load_bits(
            &mut b,
            &input.input,
            input.lsb,
            input.width,
            input.four_state,
        );
        let previous = load(&mut b, &input.shadow, input.width, input.four_state);
        let differs = binary(&mut b, current, BinaryOp::NeCase, previous);
        run = binary(&mut b, run, BinaryOp::Or, differs);
        currents.push(current);
    }
    let execute = b.new_block();
    let done = b.new_block();
    b.seal_block(SIRTerminator::Branch {
        cond: run,
        true_block: (execute, Vec::new()),
        false_block: (done, Vec::new()),
    });
    b.switch_to_block(execute);
    for (input, current) in inputs.iter().zip(currents) {
        b.emit(SIRInstruction::Store(
            input.shadow.clone(),
            SIROffset::Static(0),
            input.width,
            current,
            Vec::new(),
            Vec::new(),
        ));
    }
    let one = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
    b.emit(SIRInstruction::Store(
        ran.clone(),
        SIROffset::Static(0),
        1,
        one,
        Vec::new(),
        Vec::new(),
    ));
    b.inline_execution_unit(unit);
    b.seal_block(SIRTerminator::Jump(done, Vec::new()));
    b.switch_to_block(done);
    b.flush_eu().expect("a gated unit has instructions")
}

fn successors(terminator: &SIRTerminator) -> Vec<celox_sir::BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| case.target)
            .chain(std::iter::once(*default))
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

/// The destination of a store or commit.
pub fn write_destination<A>(instruction: &SIRInstruction<A>) -> Option<&A> {
    match instruction {
        SIRInstruction::Store(address, ..) | SIRInstruction::Commit(_, address, ..) => {
            Some(address)
        }
        _ => None,
    }
}

/// Destinations, by `key`, that one execution of `unit` may write twice
/// without returning in between: a write from which another write of the
/// same destination is reachable, including itself around a loop. A
/// process kernel returns at every suspension, so for a kernel this is
/// "twice between two suspensions"; a write in one branch and another in
/// the other branch is not.
///
/// The result is a syntactic superset; a candidate costs only counter
/// objects unless something watches it.
pub fn multi_store_candidates<A, K: Eq + std::hash::Hash + Clone>(
    unit: &ExecutionUnit<A>,
    key: impl Fn(&A) -> Option<K>,
) -> Vec<K> {
    let writes = |block: &celox_sir::BasicBlock<A>| -> Vec<Option<K>> {
        block
            .instructions
            .iter()
            .map(|instruction| write_destination(instruction).and_then(&key))
            .collect()
    };
    let block_writes: crate::HashMap<celox_sir::BlockId, Vec<Option<K>>> = unit
        .blocks
        .iter()
        .map(|(&id, block)| (id, writes(block)))
        .collect();
    let mut blocks: Vec<_> = unit.blocks.keys().copied().collect();
    blocks.sort_unstable();
    let mut candidates: Vec<K> = Vec::new();
    for &start in &blocks {
        for (position, written) in block_writes[&start].iter().enumerate() {
            let Some(written) = written else {
                continue;
            };
            if candidates.contains(written) {
                continue;
            }
            let rewritten = |block: &celox_sir::BlockId, from: usize| {
                block_writes[block][from..]
                    .iter()
                    .any(|other| other.as_ref() == Some(written))
            };
            let mut found = rewritten(&start, position + 1);
            let mut seen = crate::HashSet::default();
            let mut work = successors(&unit.blocks[&start].terminator);
            while !found && let Some(block) = work.pop() {
                if !seen.insert(block) || !unit.blocks.contains_key(&block) {
                    continue;
                }
                found = rewritten(&block, 0);
                work.extend(successors(&unit.blocks[&block].terminator));
            }
            if found {
                candidates.push(written.clone());
            }
        }
    }
    candidates
}

/// Surround every store or commit in `unit` whose destination `watch_for`
/// maps to a counted signal with the code that counts its occurrences.
pub fn instrument_unit<A: Clone>(
    unit: &mut ExecutionUnit<A>,
    watch_for: impl Fn(&A) -> Option<WatchCode<A>>,
) {
    let mut next_register = unit
        .register_map
        .keys()
        .map(|id| id.0 + 1)
        .max()
        .unwrap_or(0);
    for block in unit.blocks.values_mut() {
        if !block.instructions.iter().any(|instruction| {
            write_destination(instruction)
                .and_then(&watch_for)
                .is_some()
        }) {
            continue;
        }
        let mut instructions = Vec::with_capacity(block.instructions.len());
        for instruction in std::mem::take(&mut block.instructions) {
            let Some(watch) = write_destination(&instruction).and_then(&watch_for) else {
                instructions.push(instruction);
                continue;
            };
            let value_type = match &instruction {
                SIRInstruction::Store(_, _, _, value, ..) => unit.register_map.get(value).cloned(),
                _ => None,
            };
            let mut b = SIRBuilder::new_with(next_register);
            emit_watched(&mut b, &watch, instruction, value_type.as_ref());
            b.seal_block(SIRTerminator::Return);
            let (mut blocks, registers, next) = b.drain();
            next_register = next;
            unit.register_map.extend(registers);
            instructions.extend(
                blocks
                    .remove(&celox_sir::BlockId(0))
                    .expect("the emitted block")
                    .instructions,
            );
        }
        block.instructions = instructions;
    }
}

/// Declare counter objects for `candidates` among the variables of a module
/// and return them. Candidates that are not signals of the design
/// (`module_affiliated`) are skipped, since nothing outside the module
/// watches hidden state, and so are unpacked arrays, which no event control
/// watches as a whole.
pub fn declare_counter_slots(
    variables: &mut crate::HashMap<crate::SourceVarId, crate::symbolic::artifact::SymbolicVariable>,
    candidates: impl IntoIterator<Item = crate::SourceVarId>,
) -> Vec<celox_design::CounterSlots<crate::SourceVarId>> {
    use celox_design::{DomainKind, PortTypeKind, VariableMetadata};
    let mut candidates: Vec<_> = candidates
        .into_iter()
        .filter(|id| {
            variables
                .get(id)
                .is_some_and(|var| var.module_affiliated && var.metadata.array_dims.is_empty())
        })
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    let mut next = variables.keys().map(|id| id.0 + 1).max().unwrap_or(0);
    candidates
        .into_iter()
        .map(|signal| {
            let name = variables[&signal].path.join(".");
            let mut counter = |kind: &str| {
                let id = crate::SourceVarId(next);
                next += 1;
                variables.insert(
                    id,
                    crate::symbolic::artifact::SymbolicVariable {
                        path: vec![format!("$counter[{name}]"), kind.to_string()],
                        kind: crate::VariableKind::Variable,
                        signed: false,
                        metadata: VariableMetadata {
                            width: EVENT_COUNTER_WIDTH,
                            is_4state: false,
                            kind: DomainKind::Other,
                            type_kind: PortTypeKind::Bit,
                            array_dims: Vec::new(),
                        },
                        unpacked_ranges: Vec::new(),
                        packed_dims: vec![EVENT_COUNTER_WIDTH],
                        source: None,
                        module_affiliated: false,
                    },
                );
                id
            };
            celox_design::CounterSlots {
                signal,
                changes: counter("changes"),
                rises: counter("rises"),
                falls: counter("falls"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use celox_sir::{SIRBuilder, SIRInstruction, SIROffset, SIRTerminator, SIRValue};

    use super::multi_store_candidates;

    /// Store a constant to `address` at the open block.
    fn store(b: &mut SIRBuilder<u32>, address: u32) {
        let value = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Imm(value, SIRValue::new(1u8)));
        b.emit(SIRInstruction::Store(
            address,
            SIROffset::Static(0),
            1,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }

    fn candidates(b: &mut SIRBuilder<u32>) -> Vec<u32> {
        let unit = b.flush_eu().unwrap();
        multi_store_candidates(&unit, |address| Some(*address))
    }

    #[test]
    fn two_stores_on_one_path_are_candidates() {
        let mut b = SIRBuilder::new();
        store(&mut b, 1);
        store(&mut b, 2);
        store(&mut b, 1);
        assert_eq!(candidates(&mut b), [1]);
    }

    #[test]
    fn stores_in_exclusive_branches_are_not() {
        let mut b = SIRBuilder::new();
        let condition = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Imm(condition, SIRValue::new(1u8)));
        let (then_block, else_block, join) = (b.new_block(), b.new_block(), b.new_block());
        b.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (then_block, Vec::new()),
            false_block: (else_block, Vec::new()),
        });
        for block in [then_block, else_block] {
            b.switch_to_block(block);
            store(&mut b, 1);
            b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        }
        b.switch_to_block(join);
        assert!(candidates(&mut b).is_empty());
    }

    #[test]
    fn a_store_in_a_loop_is_a_candidate() {
        let mut b = SIRBuilder::new();
        let header = b.new_block();
        b.seal_block(SIRTerminator::Jump(header, Vec::new()));
        b.switch_to_block(header);
        store(&mut b, 3);
        let condition = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Imm(condition, SIRValue::new(0u8)));
        let exit = b.new_block();
        b.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (header, Vec::new()),
            false_block: (exit, Vec::new()),
        });
        b.switch_to_block(exit);
        assert_eq!(candidates(&mut b), [3]);
    }

    #[test]
    fn a_return_separates_stores() {
        // A kernel suspends by returning; the next resume point is entered
        // from the dispatch, not from the store before the suspension.
        let mut b = SIRBuilder::new();
        let resume = b.new_block();
        let dispatch_condition = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Imm(dispatch_condition, SIRValue::new(0u8)));
        let start = b.new_block();
        b.seal_block(SIRTerminator::Branch {
            cond: dispatch_condition,
            true_block: (resume, Vec::new()),
            false_block: (start, Vec::new()),
        });
        b.switch_to_block(start);
        store(&mut b, 4);
        b.seal_block(SIRTerminator::Return);
        b.switch_to_block(resume);
        store(&mut b, 4);
        assert!(candidates(&mut b).is_empty());
    }
}
