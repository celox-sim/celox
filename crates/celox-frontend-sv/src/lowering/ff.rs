//! Imperative SIR for sequential processes.
//!
//! An `always_ff` body becomes straight SIR control flow: branches for `if`
//! and `case`, loop blocks for loops whose trip count is not known, and
//! inlined subroutine bodies. Nonblocking targets are read from the stable
//! region and written to the working region, so every process of a trigger
//! observes the pre-edge state; blocking targets and procedural locals are
//! both read and written in the working region (IEEE 1800-2023 10.4).
//! Expressions are lowered through SLT, which keeps their width and
//! four-state semantics identical to the combinational lowering.

mod actuals;
mod assign;
mod calls;
mod classify;
mod comb_process;
mod containers;
mod delays;
mod dpi;
mod dpi_builtins;
mod dpi_calls;
mod edge_process;
mod events;
mod expr;
mod file_io;
mod format;
mod frame_liveness;
mod fread;
mod hoist;
mod inline_calls;
mod kernel;
mod locators;
mod loops;
mod memory_files;
mod monitors;
mod nba_stage;
mod nonblocking;
mod objects;
mod process;
mod random;
mod references;
mod rewrite;
mod sampled;
mod scan;
mod stmt;
mod streams;
mod strings;
mod strobe;
mod system;
mod system_tasks;
mod time_format;
mod waits;

use super::procedural::*;
use super::*;
use celox_design::{
    CounterKind, DeferredSite, EVENT_COUNTER_WIDTH, GateCounters, NbaPosition, NbaSite,
    SPARSE_WORKING_REGION, WaitGate, WaitSite,
};
use celox_frontend_core::process::{NBA_POSITION_WIDTH, ProcessKernelBuilder, ProcessKernelError};
use celox_sir::{RegisterId, RegisterType};
use celox_slt::SLTToSIRLowerer;
use num_traits::{ToPrimitive, Zero};

pub(super) use actuals::*;
use assign::*;
pub(super) use edge_process::*;
use events::*;
use inline_calls::*;
use monitors::*;
use nba_stage::*;
pub(super) use rewrite::*;
pub(super) use time_format::{TIME_FORMAT, calls_time_format};
pub(in crate::lowering) use time_format::{converts_time, time_format_operand};
pub(super) use waits::*;

type Addr = RegionedVarAddr;

struct LoopBlocks {
    break_block: BlockId,
    continue_block: BlockId,
}

struct FunctionBlocks {
    return_block: BlockId,
    return_var: Option<String>,
    loop_depth: usize,
    block_depth: usize,
}

/// One named block a `disable` can leave. In a process kernel, where an
/// activation can be suspended in the block while another runs, a block
/// a statement in its body disables carries the shared generation
/// variable of its declaration and this activation's snapshot of it, so
/// disabling the block in one activation ends every other active one at
/// its next statement (IEEE 1800-2023 9.6.2).
struct BlockEntry {
    id: u64,
    exit: BlockId,
    epoch: Option<(String, String)>,
}

pub(super) struct Ff<'p, 'a> {
    pub m: &'p mut ProcModule<'a>,
    /// What the process writes and how.
    writes: classify::Writes,
    b: SIRBuilder<Addr>,
    /// The constructs the statement being lowered is nested in.
    scope: stmt::Scope,
    /// The subroutine bodies being lowered.
    call_stack: inline_calls::CallStack,
    element_widths: HashMap<Addr, usize>,
    unrolled: usize,
    /// Read-modify-write views of a variable's working value.
    aliases: assign::Aliases,
    /// The random number streams of the process.
    rng: random::Rng,
    /// The kernel of a process being lowered. Its builder is `b` while the
    /// body is lowered, and the process reads and writes stable state
    /// directly.
    kernel: Option<ProcessKernelBuilder>,
    /// The activations of a process kernel and what each keeps for itself.
    activation: kernel::Activation,
    process_predicate: bool,
    postponed_locals: HashSet<SourceVarId>,
    /// The blocking assignments a process kernel publishes to its waits.
    observation: assign::Observation,
    /// What the process declares to the runtime while it is lowered.
    sites: kernel::Sites,
    direct: bool,
    /// Whether a combinational process also reruns for occurrences of its
    /// inputs that left their value unchanged.
    input_occurrences: bool,
    /// The sampled values and the deferred assertions of the process.
    assertions: sampled::Assertions,
    /// The function whose unit this lowers, if it lowers one.
    frame: Option<calls::FrameContext>,
    /// The slot where a process kernel leaves the end of the live frames
    /// when it suspends inside a call ([`ProcessSlots::call_top`]), once
    /// it calls a function that can suspend it.
    call_top: Option<SourceVarId>,
    /// Whether every call is inlined, in code that must not call.
    inline_calls: bool,
}

/// The clock of an edge-triggered process.
pub(super) struct Clock {
    /// The clock signal.
    pub signal: String,
    /// Whether a tick is a rising edge.
    pub rising: bool,
    /// The clock is the only event of the process, so every run is a tick.
    pub alone: bool,
}

/// The sampled values of variable `name` at the previous ticks of the clock
/// of a process: `chain[k - 1]` holds the value `k` ticks ago, counting
/// only the ticks at which the gate held.
struct History {
    name: String,
    gate: Option<sv::ir::Expr>,
    /// The hidden variable that holds the gate's value at the tick, before
    /// the body runs.
    gate_value: Option<String>,
    chain: Vec<String>,
}

fn stable(var_id: SourceVarId) -> Addr {
    RegionedVarAddrBase {
        region: STABLE_REGION,
        var_id,
    }
}

fn working(var_id: SourceVarId) -> Addr {
    RegionedVarAddrBase {
        region: WORKING_REGION,
        var_id,
    }
}

impl<'p, 'a> Ff<'p, 'a> {
    pub fn new(m: &'p mut ProcModule<'a>, random_seed: u32) -> Self {
        let mut element_widths = HashMap::default();
        for (&id, variable) in m.variables.iter() {
            if let Some(width) = unpacked_element_width(variable) {
                element_widths.insert(stable(id), width);
                element_widths.insert(working(id), width);
            }
        }
        Self {
            m,
            writes: classify::Writes::default(),
            b: SIRBuilder::new(),
            scope: stmt::Scope::default(),
            call_stack: inline_calls::CallStack::default(),
            element_widths,
            unrolled: 0,
            aliases: assign::Aliases::new(),
            rng: random::Rng::new(random_seed),
            kernel: None,
            activation: kernel::Activation::default(),
            process_predicate: false,
            postponed_locals: HashSet::default(),
            observation: assign::Observation::default(),
            sites: kernel::Sites::default(),
            direct: false,
            input_occurrences: true,
            assertions: sampled::Assertions::default(),
            frame: None,
            call_top: None,
            inline_calls: false,
        }
    }

    /// Lower an edge-triggered process with the clock its sampled value
    /// functions infer, if any.
    pub fn with_inferred_clock(mut self, clock: Option<Clock>) -> Self {
        self.assertions.inferred_clock = clock;
        self
    }

    /// Lower a combinational process that reruns only when an input value
    /// changed. Its inputs then need no occurrence counters, so the units
    /// that write them stay unchanged.
    pub fn without_input_occurrences(mut self) -> Self {
        self.input_occurrences = false;
        self
    }

    pub fn with_sparse_memories(mut self, names: &HashSet<String>) -> Self {
        self.writes.sparse_memories = names.iter().filter_map(|name| self.m.id(name)).collect();
        self
    }

    pub fn with_sparse_candidates(mut self, names: &HashSet<String>) -> Self {
        self.writes.sparse_candidates = names.iter().filter_map(|name| self.m.id(name)).collect();
        self
    }

    pub fn with_window_writes(mut self, names: &HashSet<String>) -> Self {
        self.writes.window_writes = names.iter().filter_map(|name| self.m.id(name)).collect();
        self
    }

    /// Variables other edge processes also write by blocking assignments.
    /// The process reads and writes their stable state directly, as a
    /// blocking assignment updates the variable at once (IEEE 1800-2023
    /// 10.4.1): its reads see its own preceding writes, and the last write of
    /// any process stays, even when a later process does not write it. A
    /// working value seeded by each process would instead be committed by
    /// every one of them.
    pub fn with_shared_variables(mut self, names: &HashSet<String>) -> Self {
        self.writes.shared = names.iter().filter_map(|name| self.m.id(name)).collect();
        self
    }

    /// Where a write of `id`, and a read of a value the process wrote, go:
    /// the working region, or stable state in a process kernel and for a
    /// shared variable.
    fn target(&self, id: SourceVarId) -> Addr {
        if self.kernel.is_some() || self.direct || self.writes.shared.contains(&id) {
            stable(id)
        } else {
            working(id)
        }
    }

    /// Run `f` on the kernel with its builder in place.
    fn with_kernel<T>(&mut self, f: impl FnOnce(&mut ProcessKernelBuilder) -> T) -> T {
        let mut kernel = self.kernel.take().expect("lowering a process kernel");
        std::mem::swap(&mut self.b, kernel.builder());
        let result = f(&mut kernel);
        std::mem::swap(&mut self.b, kernel.builder());
        self.kernel = Some(kernel);
        result
    }
}

/// The names of the user subroutines `expr` calls.
fn collect_call_names(expr: &sv::ir::Expr, names: &mut Vec<String>) {
    expr_calls(expr, &mut |callee| {
        if let Some(name) = callee.subroutine() {
            names.push(name.to_string());
        }
        false
    });
}

fn kernel_error(error: ProcessKernelError) -> sv::AnalyzerError {
    unsupported(error.to_string())
}

fn seal_drained(builder: SIRBuilder<Addr>) -> ExecutionUnit<Addr> {
    seal_drained_from(builder, BlockId(0))
}

/// [`seal_drained`] for a unit entered at `entry`.
fn seal_drained_from(mut builder: SIRBuilder<Addr>, entry: BlockId) -> ExecutionUnit<Addr> {
    let (blocks, register_map, _) = builder.drain();
    let mut unit = ExecutionUnit {
        entry_block_id: entry,
        blocks,
        register_map,
    };
    // Code after a jump (`break`, `return`, `$fatal`) leaves blocks that
    // control never enters.
    prune_unreachable_blocks(&mut unit);
    unit
}

/// Drop the blocks control never enters.
pub(super) fn prune_unreachable_blocks<A>(unit: &mut ExecutionUnit<A>) {
    let mut reachable = HashSet::default();
    let mut work = vec![unit.entry_block_id];
    while let Some(block) = work.pop() {
        if !reachable.insert(block) {
            continue;
        }
        match &unit.blocks[&block].terminator {
            SIRTerminator::Jump(target, _) => work.push(*target),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                work.push(true_block.0);
                work.push(false_block.0);
            }
            SIRTerminator::Switch { cases, default, .. } => {
                work.extend(cases.iter().map(|case| case.target));
                work.push(*default);
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    unit.blocks.retain(|id, _| reachable.contains(id));
}

#[cfg(test)]
mod vacuous_monitor_tests;
