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
    multi_write_candidates(unit, |instruction| {
        write_destination(instruction).and_then(&key)
    })
}

/// Like [`multi_store_candidates`], over the stores alone. A flip-flop
/// evaluation seeds its working state with commits; only its stores are
/// writes of the process.
pub fn multi_store_only_candidates<A, K: Eq + std::hash::Hash + Clone>(
    unit: &ExecutionUnit<A>,
    key: impl Fn(&A) -> Option<K>,
) -> Vec<K> {
    multi_write_candidates(unit, |instruction| match instruction {
        SIRInstruction::Store(address, ..) => key(address),
        _ => None,
    })
}

/// [`multi_store_candidates`] with the written key of each instruction
/// given by `key`, for callers that count only some kinds of writes.
///
/// The blocks are condensed into strongly connected components once; each
/// component then knows the keys written in it or downstream of it, so a
/// write is a candidate if its key is written again later in its block, or
/// in a block reachable from it. Candidates come in the order of their
/// first such write, by block id and then position.
pub fn multi_write_candidates<A, K: Eq + std::hash::Hash + Clone>(
    unit: &ExecutionUnit<A>,
    key: impl Fn(&SIRInstruction<A>) -> Option<K>,
) -> Vec<K> {
    let mut blocks: Vec<_> = unit.blocks.keys().copied().collect();
    blocks.sort_unstable();
    let index: crate::HashMap<celox_sir::BlockId, usize> = blocks
        .iter()
        .enumerate()
        .map(|(position, &block)| (block, position))
        .collect();
    let mut key_ids: crate::HashMap<K, usize> = crate::HashMap::default();
    let mut keys: Vec<K> = Vec::new();
    let mut write_counts: Vec<usize> = Vec::new();
    let writes: Vec<Vec<usize>> = blocks
        .iter()
        .map(|block| {
            unit.blocks[block]
                .instructions
                .iter()
                .filter_map(&key)
                .map(|written| {
                    let id = *key_ids.entry(written).or_insert_with_key(|written| {
                        keys.push(written.clone());
                        write_counts.push(0);
                        keys.len() - 1
                    });
                    write_counts[id] += 1;
                    id
                })
                .collect()
        })
        .collect();
    let edges: Vec<Vec<usize>> = blocks
        .iter()
        .map(|block| {
            successors(&unit.blocks[block].terminator)
                .into_iter()
                .filter_map(|successor| index.get(&successor).copied())
                .collect()
        })
        .collect();
    let (component_of, components) = strongly_connected_components(&edges);
    let cyclic: Vec<bool> = components
        .iter()
        .map(|members| members.len() > 1 || edges[members[0]].contains(&members[0]))
        .collect();

    // A key written once is rewritten only around a loop; the others are
    // looked up in per-component bit sets, a chunk of keys at a time so the
    // sets stay small.
    let tracked: Vec<usize> = (0..keys.len()).filter(|&id| write_counts[id] > 1).collect();
    let mut bit_of = vec![usize::MAX; keys.len()];
    for (bit, &id) in tracked.iter().enumerate() {
        bit_of[id] = bit;
    }
    let mut reached: Vec<Vec<bool>> = writes
        .iter()
        .enumerate()
        .map(|(block, written)| vec![cyclic[component_of[block]]; written.len()])
        .collect();
    const CHUNK_WORDS: usize = 64;
    const CHUNK_BITS: usize = CHUNK_WORDS * 64;
    for chunk_start in (0..tracked.len()).step_by(CHUNK_BITS) {
        let in_chunk = |id: usize| {
            let bit = bit_of[id];
            (bit != usize::MAX && bit >= chunk_start && bit < chunk_start + CHUNK_BITS)
                .then(|| bit - chunk_start)
        };
        // Components come sinks first, so each one's successors are done.
        let mut downstream = vec![[0u64; CHUNK_WORDS]; components.len()];
        for (component, members) in components.iter().enumerate() {
            let mut set = [0u64; CHUNK_WORDS];
            for &block in members {
                for &id in &writes[block] {
                    if let Some(bit) = in_chunk(id) {
                        set[bit / 64] |= 1 << (bit % 64);
                    }
                }
                for &successor in &edges[block] {
                    let below = component_of[successor];
                    if below != component {
                        for (word, other) in set.iter_mut().zip(&downstream[below]) {
                            *word |= other;
                        }
                    }
                }
            }
            downstream[component] = set;
        }
        for (block, written) in writes.iter().enumerate() {
            let component = component_of[block];
            if cyclic[component] {
                continue;
            }
            for (position, &id) in written.iter().enumerate() {
                let Some(bit) = in_chunk(id) else {
                    continue;
                };
                reached[block][position] = edges[block].iter().any(|&successor| {
                    downstream[component_of[successor]][bit / 64] & (1 << (bit % 64)) != 0
                });
            }
        }
    }

    let mut is_candidate = vec![false; keys.len()];
    let mut candidates: Vec<K> = Vec::new();
    let mut remaining: crate::HashMap<usize, usize> = crate::HashMap::default();
    for (block, written) in writes.iter().enumerate() {
        remaining.clear();
        for &id in written {
            *remaining.entry(id).or_default() += 1;
        }
        for (position, &id) in written.iter().enumerate() {
            let left = remaining.get_mut(&id).expect("counted above");
            *left -= 1;
            if !is_candidate[id] && (*left > 0 || reached[block][position]) {
                is_candidate[id] = true;
                candidates.push(keys[id].clone());
            }
        }
    }
    candidates
}

/// Tarjan's strongly connected components of the graph whose node `n` has
/// the successors `edges[n]`: the component of every node, and the members
/// of every component, listed so that a component comes after all the
/// components reachable from it.
fn strongly_connected_components(edges: &[Vec<usize>]) -> (Vec<usize>, Vec<Vec<usize>>) {
    const UNVISITED: usize = usize::MAX;
    let count = edges.len();
    let mut order = vec![UNVISITED; count];
    let mut low = vec![0; count];
    let mut on_stack = vec![false; count];
    let mut component_of = vec![UNVISITED; count];
    let mut components: Vec<Vec<usize>> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut next_order = 0;
    // Explicit call stack of (node, index of its next edge to follow).
    let mut calls: Vec<(usize, usize)> = Vec::new();
    for root in 0..count {
        if order[root] != UNVISITED {
            continue;
        }
        calls.push((root, 0));
        order[root] = next_order;
        low[root] = next_order;
        next_order += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (node, ref mut next_edge)) = calls.last_mut() {
            if let Some(&successor) = edges[node].get(*next_edge) {
                *next_edge += 1;
                if order[successor] == UNVISITED {
                    order[successor] = next_order;
                    low[successor] = next_order;
                    next_order += 1;
                    stack.push(successor);
                    on_stack[successor] = true;
                    calls.push((successor, 0));
                } else if on_stack[successor] {
                    low[node] = low[node].min(order[successor]);
                }
                continue;
            }
            calls.pop();
            if let Some(&(parent, _)) = calls.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == order[node] {
                let mut members = Vec::new();
                loop {
                    let member = stack.pop().expect("the root is on the stack");
                    on_stack[member] = false;
                    component_of[member] = components.len();
                    members.push(member);
                    if member == node {
                        break;
                    }
                }
                components.push(members);
            }
        }
    }
    (component_of, components)
}

/// Surround every store or commit in `unit` whose destination `watch_for`
/// maps to a counted signal with the code that counts its occurrences.
pub fn instrument_unit<A: Clone>(
    unit: &mut ExecutionUnit<A>,
    watch_for: impl Fn(&A) -> Option<WatchCode<A>>,
) {
    instrument_writes(unit, |instruction| {
        write_destination(instruction).and_then(&watch_for)
    });
}

/// [`instrument_unit`] with the watched signal of each instruction given
/// by `watch_for`, for callers that count only some kinds of writes.
pub fn instrument_writes<A: Clone>(
    unit: &mut ExecutionUnit<A>,
    watch_for: impl Fn(&SIRInstruction<A>) -> Option<WatchCode<A>>,
) {
    let mut next_register = unit
        .register_map
        .keys()
        .map(|id| id.0 + 1)
        .max()
        .unwrap_or(0);
    for block in unit.blocks.values_mut() {
        if !block
            .instructions
            .iter()
            .any(|instruction| watch_for(instruction).is_some())
        {
            continue;
        }
        let mut instructions = Vec::with_capacity(block.instructions.len());
        for instruction in std::mem::take(&mut block.instructions) {
            let Some(watch) = watch_for(&instruction) else {
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

/// The result of [`instrument_writes`] once per pass, in pass order, in one
/// walk over `unit`. `watch_for(instruction, after)` gives the first pass
/// from `after` on that watches `instruction`, and its code.
///
/// A later pass also sees the code an earlier one added, and wraps the write
/// inside it, so a write watched by several passes is surrounded by their
/// code from the first pass outward in. The new registers are numbered as
/// the passes one after another would number them.
pub fn instrument_writes_in_passes<A: Clone>(
    unit: &mut ExecutionUnit<A>,
    watch_for: impl Fn(&SIRInstruction<A>, usize) -> Option<(usize, WatchCode<A>)>,
) {
    let ExecutionUnit {
        blocks,
        register_map,
        ..
    } = unit;
    let first_new = register_map.keys().map(|id| id.0 + 1).max().unwrap_or(0);
    let mut next_register = first_new;
    // The pass, block and first register of every piece of code added, in
    // the order the walk adds them.
    let mut added: Vec<(usize, usize, usize)> = Vec::new();
    let mut instrumented = Vec::new();
    for (order, block) in blocks.values_mut().enumerate() {
        if !block
            .instructions
            .iter()
            .any(|instruction| watch_for(instruction, 0).is_some())
        {
            continue;
        }
        let mut instructions = Vec::with_capacity(block.instructions.len());
        let mut work: Vec<(SIRInstruction<A>, usize)> = Vec::new();
        for instruction in std::mem::take(&mut block.instructions) {
            work.push((instruction, 0));
            while let Some((instruction, after)) = work.pop() {
                let Some((pass, watch)) = watch_for(&instruction, after) else {
                    instructions.push(instruction);
                    continue;
                };
                let value_type = match &instruction {
                    SIRInstruction::Store(_, _, _, value, ..) => register_map.get(value).cloned(),
                    _ => None,
                };
                let mut b = SIRBuilder::new_with(next_register);
                emit_watched(&mut b, &watch, instruction, value_type.as_ref());
                b.seal_block(SIRTerminator::Return);
                let (mut emitted, registers, next) = b.drain();
                added.push((pass, order, next_register));
                next_register = next;
                register_map.extend(registers);
                let emitted = emitted
                    .remove(&celox_sir::BlockId(0))
                    .expect("the emitted block")
                    .instructions;
                work.extend(emitted.into_iter().rev().map(|emitted| (emitted, pass + 1)));
            }
        }
        block.instructions = instructions;
        instrumented.push(block.id);
    }
    if added.is_empty() {
        return;
    }

    // Renumber the new registers pass by pass, then block by block in the
    // order a pass visits them; within a block the walk already added each
    // pass's code in order.
    let mut ends: Vec<usize> = added.iter().skip(1).map(|&(.., first)| first).collect();
    ends.push(next_register);
    let mut pieces: Vec<(usize, usize, usize, usize)> = added
        .iter()
        .zip(ends)
        .map(|(&(pass, order, first), end)| (pass, order, first, end))
        .collect();
    pieces.sort_by_key(|&(pass, order, ..)| (pass, order));
    let mut renumbered = vec![0; next_register - first_new];
    let mut next = first_new;
    for (.., first, end) in pieces {
        for register in first..end {
            renumbered[register - first_new] = next;
            next += 1;
        }
    }
    let renumber = |register: &mut RegisterId| {
        if register.0 >= first_new {
            register.0 = renumbered[register.0 - first_new];
        }
    };
    for id in instrumented {
        let block = blocks.get_mut(&id).expect("an instrumented block");
        for instruction in &mut block.instructions {
            if let Some(register) = instruction.defined_register_mut() {
                renumber(register);
            }
            instruction.for_each_use_mut(renumber);
        }
    }
    let new_registers: Vec<_> = (first_new..next_register)
        .filter_map(|id| register_map.remove_entry(&RegisterId(id)))
        .collect();
    register_map.extend(new_registers.into_iter().map(|(mut id, ty)| {
        renumber(&mut id);
        (id, ty)
    }));
}

/// Objects that hold back the occurrences of one flip-flop evaluation's
/// writes to a signal until the commit that publishes them.
///
/// A flip-flop evaluates into the signal's next state, and a commit
/// publishes it in the active or the NBA region. Every write is an update
/// of the signal there, applied in issue order (IEEE 1800-2023 4.5,
/// 10.4.2): `y <= 0; y <= 1;` is two updates. Counting at the writes would
/// wake a waiter before the commit, and counting at the commit alone sees
/// only the final value.
///
/// So the writes go to a private copy of the stream (`value`) as well, and
/// `covered` marks the bits written so far. A write that changes covered
/// bits is an occurrence whatever the published value is: it marks its bits
/// in `changed`, and an edge of the least significant bit counts in `rises`
/// or `falls`. A write of uncovered bits compares with the value the commit
/// publishes over, so `first` keeps the value it wrote there. A commit
/// counts, in the bits it publishes, the occurrence from the published value
/// to `first` and the held ones, and releases those bits.
///
/// A waiter compares a counter only with the value it saw, so the commit
/// counts one change for all the changes it releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingSlots<A> {
    pub signal: A,
    /// For the nonblocking writes of a variable the process also writes by
    /// blocking assignments: the staging value they go to. Otherwise the
    /// writes go to the signal's next state.
    pub staged: Option<A>,
    pub value: A,
    pub covered: A,
    pub first: A,
    pub changed: A,
    /// The bits of the write being counted.
    pub written: A,
    pub rises: A,
    pub falls: A,
}

impl<A: Copy> PendingSlots<A> {
    pub fn map<B>(self, mut map: impl FnMut(A) -> B) -> PendingSlots<B> {
        PendingSlots {
            signal: map(self.signal),
            staged: self.staged.map(&mut map),
            value: map(self.value),
            covered: map(self.covered),
            first: map(self.first),
            changed: map(self.changed),
            written: map(self.written),
            rises: map(self.rises),
            falls: map(self.falls),
        }
    }
}

/// The code view of a signal's [`PendingSlots`].
#[derive(Clone, Debug)]
pub struct PendingCode<A> {
    /// The signal's stable state and its counters.
    pub watch: WatchCode<A>,
    /// Where the writes go: the signal's next state, or its staging value.
    pub write: A,
    /// Whether `write` is a staging value, which a store to the signal
    /// publishes.
    pub staged: bool,
    pub value: A,
    pub covered: A,
    pub first: A,
    pub changed: A,
    pub written: A,
    /// The held edges of the least significant bit.
    pub edges: Vec<(CounterKind, A)>,
}

/// What an instruction of a flip-flop unit does to a [`PendingCode`]
/// stream.
enum PendingRole {
    /// A write of the evaluation.
    Write,
    /// The publication.
    Release,
}

impl<A: PartialEq> PendingCode<A> {
    fn role(&self, instruction: &SIRInstruction<A>) -> Option<PendingRole> {
        match instruction {
            SIRInstruction::Store(address, ..) if *address == self.write => {
                Some(PendingRole::Write)
            }
            SIRInstruction::Store(address, ..) if self.staged && *address == self.watch.signal => {
                Some(PendingRole::Release)
            }
            // A seed copies the stable state into the next state; it is
            // not a write.
            SIRInstruction::Commit(source, destination, ..)
                if !self.staged && *source == self.write && *destination == self.watch.signal =>
            {
                Some(PendingRole::Release)
            }
            _ => None,
        }
    }
}

fn value_register<A>(b: &mut SIRBuilder<A>, width: usize, four_state: bool) -> RegisterId {
    if four_state {
        b.alloc_logic(width)
    } else {
        b.alloc_bit(width, false)
    }
}

fn store<A: Clone>(b: &mut SIRBuilder<A>, address: &A, width: usize, value: RegisterId) {
    b.emit(SIRInstruction::Store(
        address.clone(),
        SIROffset::Static(0),
        width,
        value,
        Vec::new(),
        Vec::new(),
    ));
}

fn mask<A>(b: &mut SIRBuilder<A>, bits: &num_bigint::BigUint, width: usize) -> RegisterId {
    let mask = b.alloc_bit(width, false);
    b.emit(SIRInstruction::Imm(mask, SIRValue::new(bits.clone())));
    mask
}

fn ones(width: usize) -> num_bigint::BigUint {
    (num_bigint::BigUint::from(1u8) << width) - 1u8
}

/// `base` with the bits `mask` marks taken from `value`.
fn overlay<A>(
    b: &mut SIRBuilder<A>,
    base: RegisterId,
    value: RegisterId,
    mask: RegisterId,
    width: usize,
    four_state: bool,
) -> RegisterId {
    let keep = b.alloc_bit(width, false);
    b.emit(SIRInstruction::Unary(
        keep,
        celox_design::UnaryOp::BitNot,
        mask,
    ));
    let kept = value_register(b, width, four_state);
    b.emit(SIRInstruction::Binary(kept, base, BinaryOp::And, keep));
    let taken = value_register(b, width, four_state);
    b.emit(SIRInstruction::Binary(taken, value, BinaryOp::And, mask));
    let merged = value_register(b, width, four_state);
    b.emit(SIRInstruction::Binary(merged, kept, BinaryOp::Or, taken));
    merged
}

/// `lhs & !rhs`, both masks.
fn and_not<A>(b: &mut SIRBuilder<A>, lhs: RegisterId, rhs: RegisterId, width: usize) -> RegisterId {
    let inverted = b.alloc_bit(width, false);
    b.emit(SIRInstruction::Unary(
        inverted,
        celox_design::UnaryOp::BitNot,
        rhs,
    ));
    let result = b.alloc_bit(width, false);
    b.emit(SIRInstruction::Binary(result, lhs, BinaryOp::And, inverted));
    result
}

/// Emit `write`, a store of the evaluation to the stream of `code`, and
/// keep its occurrence (see [`PendingSlots`]).
fn emit_pending_write<A: Clone>(
    b: &mut SIRBuilder<A>,
    code: &PendingCode<A>,
    write: SIRInstruction<A>,
) {
    let (width, four_state) = (code.watch.width, code.watch.four_state);
    let SIRInstruction::Store(_, offset, bits, source, ..) = &write else {
        unreachable!("a write of a stream is a store");
    };
    let (offset, bits, source) = (offset.clone(), *bits, *source);
    let old = load(b, &code.value, width, four_state);
    let covered = load(b, &code.covered, width, false);
    b.emit(write);
    // The write applied to the private copy, and the bits it writes.
    b.emit(SIRInstruction::Store(
        code.value.clone(),
        offset.clone(),
        bits,
        source,
        Vec::new(),
        Vec::new(),
    ));
    let none = mask(b, &num_bigint::BigUint::default(), width);
    store(b, &code.written, width, none);
    let all = mask(b, &ones(bits), bits);
    b.emit(SIRInstruction::Store(
        code.written.clone(),
        offset,
        bits,
        all,
        Vec::new(),
        Vec::new(),
    ));
    let written = load(b, &code.written, width, false);
    let new = load(b, &code.value, width, four_state);
    // The covered bits before the write, on a fixed background.
    let background = constant(b, 0, width, four_state);
    let before = overlay(b, background, old, covered, width, four_state);
    let after = overlay(b, background, new, covered, width, four_state);
    let changed = binary(b, before, BinaryOp::NeCase, after);
    let marked = load(b, &code.changed, width, false);
    let with_write = b.alloc_bit(width, false);
    b.emit(SIRInstruction::Binary(
        with_write,
        marked,
        BinaryOp::Or,
        written,
    ));
    let next_marked = b.alloc_bit(width, false);
    b.emit(SIRInstruction::Mux(
        next_marked,
        changed,
        with_write,
        marked,
    ));
    store(b, &code.changed, width, next_marked);
    let before_lsb = lsb(b, before, width, four_state);
    let after_lsb = lsb(b, after, width, four_state);
    for &(kind, ref held) in &code.edges {
        let occurred = occurrence(
            b,
            kind,
            (before, before_lsb),
            (after, after_lsb),
            width,
            four_state,
        );
        bump(b, held, occurred);
    }
    // The bits the write covers first keep its value.
    let fresh = and_not(b, written, covered, width);
    let first = load(b, &code.first, width, four_state);
    let first = overlay(b, first, new, fresh, width, four_state);
    store(b, &code.first, width, first);
    let now_covered = b.alloc_bit(width, false);
    b.emit(SIRInstruction::Binary(
        now_covered,
        covered,
        BinaryOp::Or,
        written,
    ));
    store(b, &code.covered, width, now_covered);
}

/// After a publication of the stream of `code` in the bits `published`
/// writes, count their occurrences: the one from `before`, the signal's
/// value before the publication, to the first written values, the held
/// changes, and with the least significant bit its held edges. Those bits
/// of the stream are then empty.
fn emit_release<A: Clone>(
    b: &mut SIRBuilder<A>,
    code: &PendingCode<A>,
    before: RegisterId,
    published: &num_bigint::BigUint,
) {
    let (width, four_state) = (code.watch.width, code.watch.four_state);
    let bits = mask(b, published, width);
    let covered = load(b, &code.covered, width, false);
    let released = b.alloc_bit(width, false);
    b.emit(SIRInstruction::Binary(
        released,
        covered,
        BinaryOp::And,
        bits,
    ));
    let first = load(b, &code.first, width, four_state);
    let first = overlay(b, before, first, released, width, four_state);
    let before_lsb = lsb(b, before, width, four_state);
    let first_lsb = lsb(b, first, width, four_state);
    let lsb_released = published.bit(0);
    for (kind, counter) in &code.watch.counters {
        let occurred = occurrence(
            b,
            *kind,
            (before, before_lsb),
            (first, first_lsb),
            width,
            four_state,
        );
        if *kind == CounterKind::Changes {
            let marked = load(b, &code.changed, width, false);
            let held = b.alloc_bit(width, false);
            b.emit(SIRInstruction::Binary(held, marked, BinaryOp::And, bits));
            let none = mask(b, &num_bigint::BigUint::default(), width);
            let held = binary(b, held, BinaryOp::Ne, none);
            let occurred = binary(b, occurred, BinaryOp::Or, held);
            bump(b, counter, occurred);
            continue;
        }
        bump(b, counter, occurred);
        let Some((_, held)) = code.edges.iter().find(|(k, _)| k == kind) else {
            continue;
        };
        if !lsb_released {
            continue;
        }
        let total = load(b, counter, EVENT_COUNTER_WIDTH, false);
        let pending = load(b, held, EVENT_COUNTER_WIDTH, false);
        let sum = b.alloc_bit(EVENT_COUNTER_WIDTH, false);
        b.emit(SIRInstruction::Binary(sum, total, BinaryOp::Add, pending));
        store(b, counter, EVENT_COUNTER_WIDTH, sum);
        let zero = b.alloc_bit(EVENT_COUNTER_WIDTH, false);
        b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        store(b, held, EVENT_COUNTER_WIDTH, zero);
    }
    let remaining = and_not(b, covered, bits, width);
    store(b, &code.covered, width, remaining);
    let marked = load(b, &code.changed, width, false);
    let remaining = and_not(b, marked, bits, width);
    store(b, &code.changed, width, remaining);
}

/// Empty the bits `cleared` of the stream of `code`, with the held edges
/// when they include the least significant bit.
fn emit_clear<A: Clone>(
    b: &mut SIRBuilder<A>,
    code: &PendingCode<A>,
    cleared: &num_bigint::BigUint,
) {
    let width = code.watch.width;
    let bits = mask(b, cleared, width);
    for address in [&code.covered, &code.changed] {
        let held = load(b, address, width, false);
        let remaining = and_not(b, held, bits, width);
        store(b, address, width, remaining);
    }
    if cleared.bit(0) {
        let zero = b.alloc_bit(EVENT_COUNTER_WIDTH, false);
        b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        for (_, held) in &code.edges {
            store(b, held, EVENT_COUNTER_WIDTH, zero);
        }
    }
}

/// The instructions `emit` builds, with registers from `next_register` on.
fn emitted<A>(
    register_map: &mut impl Extend<(RegisterId, celox_sir::RegisterType)>,
    next_register: &mut usize,
    emit: impl FnOnce(&mut SIRBuilder<A>),
) -> Vec<SIRInstruction<A>> {
    let mut b = SIRBuilder::new_with(*next_register);
    emit(&mut b);
    b.seal_block(SIRTerminator::Return);
    let (mut blocks, registers, next) = b.drain();
    *next_register = next;
    register_map.extend(registers);
    blocks
        .remove(&celox_sir::BlockId(0))
        .expect("the emitted block")
        .instructions
}

/// Instrument the writes and publications of a flip-flop unit for the
/// streams of `codes` (see [`PendingSlots`]). The units of one evaluation,
/// such as the parts of a parallel build, share a stream: each publication
/// releases only the bits it publishes.
///
/// A unit starts its own bits of a stream empty: the bits it seeds into the
/// next state, or all of a staging value, which one process writes. An
/// evaluation that a runtime error ended before its commit, and that the
/// host then continues, leaves nothing behind for the next one, and other
/// units of the same evaluation keep what they hold.
pub fn instrument_pending<A: Clone + PartialEq>(
    unit: &mut ExecutionUnit<A>,
    codes: &[PendingCode<A>],
) {
    if codes.is_empty() {
        return;
    }
    let mut next_register = unit
        .register_map
        .keys()
        .map(|id| id.0 + 1)
        .max()
        .unwrap_or(0);
    let mut blocks: Vec<_> = unit.blocks.keys().copied().collect();
    blocks.sort_unstable();
    for id in blocks {
        let block = unit.blocks.get_mut(&id).expect("a block of the unit");
        let roles: Vec<_> = block
            .instructions
            .iter()
            .map(|instruction| {
                codes
                    .iter()
                    .enumerate()
                    .find_map(|(index, code)| code.role(instruction).map(|role| (index, role)))
            })
            .collect();
        if roles.iter().all(Option::is_none) {
            continue;
        }
        // The bits each stream's publications in this block write, and
        // where its last publication is.
        let mut written = vec![num_bigint::BigUint::default(); codes.len()];
        let mut last = vec![None; codes.len()];
        for (position, (instruction, role)) in block.instructions.iter().zip(&roles).enumerate() {
            let Some((index, PendingRole::Release)) = role else {
                continue;
            };
            last[*index] = Some(position);
            let range = match instruction {
                SIRInstruction::Commit(_, _, SIROffset::Static(lsb), bits, _) => {
                    ones(*bits) << *lsb
                }
                _ => ones(codes[*index].watch.width),
            };
            written[*index] |= range;
        }
        let mut published: Vec<Option<RegisterId>> = vec![None; codes.len()];
        let mut instructions = Vec::with_capacity(block.instructions.len());
        for (position, (instruction, role)) in std::mem::take(&mut block.instructions)
            .into_iter()
            .zip(roles)
            .enumerate()
        {
            match role {
                None => instructions.push(instruction),
                Some((index, PendingRole::Write)) => {
                    instructions.extend(emitted(&mut unit.register_map, &mut next_register, |b| {
                        emit_pending_write(b, &codes[index], instruction)
                    }));
                }
                Some((index, PendingRole::Release)) => {
                    let code = &codes[index];
                    if published[index].is_none() {
                        instructions.extend(emitted(
                            &mut unit.register_map,
                            &mut next_register,
                            |b| {
                                published[index] = Some(load(
                                    b,
                                    &code.watch.signal,
                                    code.watch.width,
                                    code.watch.four_state,
                                ));
                            },
                        ));
                    }
                    instructions.push(instruction);
                    if last[index] == Some(position) {
                        let before = published[index].expect("loaded before the first commit");
                        instructions.extend(emitted(
                            &mut unit.register_map,
                            &mut next_register,
                            |b| emit_release(b, code, before, &written[index]),
                        ));
                    }
                }
            }
        }
        block.instructions = instructions;
    }
    let mut entry = Vec::new();
    for code in codes {
        let cleared = owned_bits(unit, code);
        if cleared != num_bigint::BigUint::default() {
            entry.extend(emitted(&mut unit.register_map, &mut next_register, |b| {
                emit_clear(b, code, &cleared)
            }));
        }
    }
    let block = unit
        .blocks
        .get_mut(&unit.entry_block_id)
        .expect("the entry block");
    entry.append(&mut block.instructions);
    block.instructions = entry;
}

/// The bits of the stream of `code` that `unit` owns: the bits it seeds
/// from the signal into the next state, or the whole staging value if it
/// writes it.
fn owned_bits<A: PartialEq>(unit: &ExecutionUnit<A>, code: &PendingCode<A>) -> num_bigint::BigUint {
    let mut owned = num_bigint::BigUint::default();
    for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
        match instruction {
            SIRInstruction::Store(address, ..) if code.staged && *address == code.write => {
                return ones(code.watch.width);
            }
            SIRInstruction::Commit(source, destination, SIROffset::Static(lsb), bits, _)
                if !code.staged && *source == code.watch.signal && *destination == code.write =>
            {
                owned |= ones(*bits) << *lsb;
            }
            _ => {}
        }
    }
    owned
}

type SymbolicVariables =
    crate::HashMap<crate::SourceVarId, crate::symbolic::artifact::SymbolicVariable>;

/// Whether `id` is a signal of the design that an event control can watch:
/// state of the module (`module_affiliated`), since nothing outside the
/// module watches hidden state, and no unpacked array, which no event
/// control watches as a whole.
fn watchable(variables: &SymbolicVariables, id: &crate::SourceVarId) -> bool {
    variables
        .get(id)
        .is_some_and(|var| var.module_affiliated && var.metadata.array_dims.is_empty())
}

/// Declare a hidden object `path` of `width` bits and return it.
fn declare_hidden(
    variables: &mut SymbolicVariables,
    path: Vec<String>,
    width: usize,
    is_4state: bool,
) -> crate::SourceVarId {
    use celox_design::{DomainKind, PortTypeKind, VariableMetadata};
    let id = crate::SourceVarId(variables.keys().map(|id| id.0 + 1).max().unwrap_or(0));
    variables.insert(
        id,
        crate::symbolic::artifact::SymbolicVariable {
            path,
            kind: crate::VariableKind::Variable,
            signed: false,
            metadata: VariableMetadata {
                width,
                is_4state,
                kind: DomainKind::Other,
                type_kind: if is_4state {
                    PortTypeKind::Logic
                } else {
                    PortTypeKind::Bit
                },
                array_dims: Vec::new(),
                value_kind: Default::default(),
            },
            unpacked_ranges: Vec::new(),
            packed_dims: vec![width],
            source: None,
            module_affiliated: false,
        },
    );
    id
}

/// Declare counter objects for `candidates` among the variables of a module
/// and return them. Candidates that are no state of the module, or are
/// unpacked arrays, are skipped (see `watchable`).
pub fn declare_counter_slots(
    variables: &mut SymbolicVariables,
    candidates: impl IntoIterator<Item = crate::SourceVarId>,
) -> Vec<celox_design::CounterSlots<crate::SourceVarId>> {
    let mut candidates: Vec<_> = candidates
        .into_iter()
        .filter(|id| watchable(variables, id))
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    candidates
        .into_iter()
        .map(|signal| {
            let name = variables[&signal].path.join(".");
            let mut counter = |kind: &str| {
                declare_hidden(
                    variables,
                    vec![format!("$counter[{name}]"), kind.to_string()],
                    EVENT_COUNTER_WIDTH,
                    false,
                )
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

/// Declare the [`PendingSlots`] of `streams` among the variables of a
/// module and return them. A stream is a signal and, for nonblocking writes
/// staged apart from blocking ones, its staging value. Streams of signals
/// that are not watchable (see `watchable`) are skipped.
pub fn declare_pending_slots(
    variables: &mut SymbolicVariables,
    streams: impl IntoIterator<Item = (crate::SourceVarId, Option<crate::SourceVarId>)>,
) -> Vec<PendingSlots<crate::SourceVarId>> {
    let mut streams: Vec<_> = streams
        .into_iter()
        .filter(|(signal, _)| watchable(variables, signal))
        .collect();
    streams.sort_unstable();
    streams.dedup();
    streams
        .into_iter()
        .map(|(signal, staged)| {
            let name = variables[&signal].path.join(".");
            let (width, is_4state) = {
                let metadata = &variables[&signal].metadata;
                (metadata.width, metadata.is_4state)
            };
            let stream = match staged {
                Some(_) => format!("$staged[{name}]"),
                None => format!("$pending[{name}]"),
            };
            let mut hidden = |kind: &str, width: usize, is_4state: bool| {
                declare_hidden(
                    variables,
                    vec![stream.clone(), kind.to_string()],
                    width,
                    is_4state,
                )
            };
            PendingSlots {
                signal,
                staged,
                value: hidden("value", width, is_4state),
                covered: hidden("covered", width, false),
                first: hidden("first", width, is_4state),
                changed: hidden("changed", width, false),
                written: hidden("written", width, false),
                rises: hidden("rises", EVENT_COUNTER_WIDTH, false),
                falls: hidden("falls", EVENT_COUNTER_WIDTH, false),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use celox_sir::{SIRBuilder, SIRInstruction, SIROffset, SIRTerminator, SIRValue};

    use super::{
        WatchCode, instrument_writes, instrument_writes_in_passes, multi_store_candidates,
        multi_write_candidates, successors, write_destination,
    };

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

    /// The direct search [`multi_write_candidates`] replaced: a walk over
    /// the reachable blocks from every write.
    fn candidates_by_walk(unit: &celox_sir::ExecutionUnit<u32>) -> Vec<u32> {
        let key = |instruction: &SIRInstruction<u32>| write_destination(instruction).copied();
        let mut blocks: Vec<_> = unit.blocks.keys().copied().collect();
        blocks.sort_unstable();
        let mut candidates: Vec<u32> = Vec::new();
        for &start in &blocks {
            for (position, written) in unit.blocks[&start].instructions.iter().enumerate() {
                let Some(written) = key(written) else {
                    continue;
                };
                if candidates.contains(&written) {
                    continue;
                }
                let rewritten = |block: &celox_sir::BlockId, from: usize| {
                    unit.blocks[block].instructions[from..]
                        .iter()
                        .any(|other| key(other) == Some(written))
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
                    candidates.push(written);
                }
            }
        }
        candidates
    }

    /// A random unit: sparse block ids, forward and backward edges (loops),
    /// edges to missing blocks, returns, and stores, commits and other
    /// instructions over a few addresses.
    fn random_unit(seed: u64) -> celox_sir::ExecutionUnit<u32> {
        use celox_sir::{BasicBlock, BlockId, RegisterId, SIRSwitchCase};
        let state = std::cell::Cell::new(seed);
        let next = |bound: u64| {
            state.set(
                state
                    .get()
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407),
            );
            (state.get() >> 33) % bound
        };
        let count = 1 + next(30) as usize;
        let addresses = 1 + next(12) as u32;
        let back_edges = next(4);
        let id = |n: usize| BlockId(n * 3 + 1);
        let mut blocks = crate::HashMap::default();
        for n in 0..count {
            let mut instructions = Vec::new();
            for _ in 0..next(5) {
                let address = next(addresses as u64) as u32;
                instructions.push(match next(4) {
                    0 => SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    1 => SIRInstruction::Commit(
                        address,
                        address,
                        SIROffset::Static(0),
                        1,
                        Vec::new(),
                    ),
                    _ => SIRInstruction::Store(
                        address,
                        SIROffset::Static(0),
                        1,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                });
            }
            let target = || {
                let roll = next(10);
                if roll == 0 {
                    BlockId(0)
                } else if roll <= back_edges {
                    id(next(n as u64 + 1) as usize)
                } else {
                    id((n + 1 + next(4) as usize).min(count))
                }
            };
            let terminator = match next(5) {
                0 => SIRTerminator::Return,
                1 | 2 => SIRTerminator::Jump(target(), Vec::new()),
                3 => SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (target(), Vec::new()),
                    false_block: (target(), Vec::new()),
                },
                _ => SIRTerminator::Switch {
                    selector: RegisterId(0),
                    cases: (0..3u8)
                        .map(|value| SIRSwitchCase {
                            value: value.into(),
                            target: target(),
                        })
                        .collect(),
                    default: target(),
                },
            };
            blocks.insert(
                id(n),
                BasicBlock {
                    id: id(n),
                    params: Vec::new(),
                    instructions,
                    terminator,
                },
            );
        }
        celox_sir::ExecutionUnit {
            entry_block_id: id(0),
            blocks,
            register_map: Default::default(),
        }
    }

    #[test]
    fn candidates_match_a_walk_from_every_write() {
        for seed in 0..5000 {
            let unit = random_unit(seed);
            let fast = multi_write_candidates(&unit, |instruction| {
                write_destination(instruction).copied()
            });
            assert_eq!(fast, candidates_by_walk(&unit), "seed {seed}:\n{unit}");
        }
    }

    #[test]
    fn candidates_beyond_one_chunk_of_keys_are_found() {
        // Two blocks storing 10000 addresses each, the second only the
        // even ones: more keys than one chunk of the bit sets holds.
        use celox_sir::{BasicBlock, BlockId, RegisterId};
        let stores = |addresses: &mut dyn Iterator<Item = u32>| {
            addresses
                .map(|address| {
                    SIRInstruction::Store(
                        address,
                        SIROffset::Static(0),
                        1,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut blocks = crate::HashMap::default();
        for (id, instructions, terminator) in [
            (
                0,
                stores(&mut (0..10000)),
                SIRTerminator::Jump(BlockId(1), Vec::new()),
            ),
            (1, stores(&mut (0..10000).step_by(2)), SIRTerminator::Return),
        ] {
            blocks.insert(
                BlockId(id),
                BasicBlock {
                    id: BlockId(id),
                    params: Vec::new(),
                    instructions,
                    terminator,
                },
            );
        }
        let unit = celox_sir::ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map: Default::default(),
        };
        let found = multi_store_candidates(&unit, |address| Some(*address));
        assert_eq!(found, (0..10000).step_by(2).collect::<Vec<u32>>());
    }

    /// One pass of the equivalence test: the writes it watches and the
    /// code it adds.
    struct Pass {
        address: u32,
        stores_only: bool,
        code: WatchCode<u32>,
    }

    impl Pass {
        fn watches(&self, instruction: &SIRInstruction<u32>) -> bool {
            match instruction {
                SIRInstruction::Store(address, ..) => *address == self.address,
                _ => !self.stores_only && write_destination(instruction) == Some(&self.address),
            }
        }
    }

    /// A random unit of a few blocks writing a few addresses, and random
    /// passes over the same addresses; a pass's counters may be written
    /// by another pass's code.
    fn random_instrumentation(seed: u64) -> (celox_sir::ExecutionUnit<u32>, Vec<Pass>) {
        use celox_design::CounterKind;
        let state = std::cell::Cell::new(seed);
        let next = |bound: u64| {
            state.set(
                state
                    .get()
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407),
            );
            (state.get() >> 33) % bound
        };
        let addresses = 1 + next(6) as u32;
        let widths = [1, 4, 8];
        let mut b = SIRBuilder::new();
        let blocks = 1 + next(4);
        for block in 0..blocks {
            for _ in 0..next(6) {
                let width = widths[next(3) as usize];
                let value = if next(2) == 0 {
                    b.alloc_logic(width)
                } else {
                    b.alloc_bit(width, false)
                };
                b.emit(SIRInstruction::Imm(value, SIRValue::new(next(256) as u8)));
                let address = next(addresses as u64) as u32;
                b.emit(match next(3) {
                    0 => SIRInstruction::Commit(
                        address,
                        address,
                        SIROffset::Static(0),
                        width,
                        Vec::new(),
                    ),
                    _ => SIRInstruction::Store(
                        address,
                        SIROffset::Static(next(2) as usize * 2),
                        width,
                        value,
                        Vec::new(),
                        Vec::new(),
                    ),
                });
            }
            if block + 1 < blocks {
                let following = b.new_block();
                b.seal_block(SIRTerminator::Jump(following, Vec::new()));
                b.switch_to_block(following);
            }
        }
        let value = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Imm(value, SIRValue::new(0u8)));
        let unit = b.flush_eu().unwrap();
        let passes = (0..1 + next(8))
            .map(|_| Pass {
                address: next(addresses as u64) as u32,
                stores_only: next(2) == 0,
                code: WatchCode {
                    signal: next(addresses as u64) as u32,
                    width: widths[next(3) as usize],
                    offset: SIROffset::Static(next(2) as usize * 2),
                    four_state: next(2) == 0,
                    counters: CounterKind::ALL
                        .into_iter()
                        .filter(|_| next(2) == 0)
                        .map(|kind| (kind, next(addresses as u64 + 2) as u32))
                        .collect(),
                },
            })
            .collect();
        (unit, passes)
    }

    #[test]
    fn instrumenting_in_passes_matches_one_pass_after_another() {
        for seed in 0..3000 {
            let (unit, passes) = random_instrumentation(seed);
            let mut one_by_one = unit.clone();
            for pass in &passes {
                instrument_writes(&mut one_by_one, |instruction| {
                    pass.watches(instruction).then(|| pass.code.clone())
                });
            }
            let mut at_once = unit;
            instrument_writes_in_passes(&mut at_once, |instruction, after| {
                let found = passes[after.min(passes.len())..]
                    .iter()
                    .position(|pass| pass.watches(instruction))?;
                Some((after + found, passes[after + found].code.clone()))
            });
            assert_eq!(at_once, one_by_one, "seed {seed}");
        }
    }
}
