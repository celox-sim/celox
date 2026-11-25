# Optimization Architecture

Celox optimizes at three compiler layers and once more in the runtime. Each layer
uses the representation that can prove its transformations without depending on
details from adjacent phases.

```text
SLT structure
    ▼
SIR control flow and state accesses
    ▼
backend-private machine IR
    ▼
runtime event scheduling
```

User-facing optimization presets and trade-offs are documented in
[Optimization Tuning](/guide/optimization-tuning). This page describes where
the mechanisms live.

## Symbolic logic layer

SLT optimizations operate while RTL expressions and bit ranges are still
explicit.

- **Hash consing** interns structurally identical expressions so common logic has
  one symbolic identity.
- **Topological hoisting** materializes shared subexpressions once at a legal
  dependency point.
- **Range atomization** splits state at observed bit boundaries, avoiding
  whole-value work when only a slice is required.
- **Cost-directed mux lowering** chooses between eager selection and control flow
  before expression structure is lost.
- **Net inlining** removes combinational nets that only carry a value between
  paths: a net one path reads becomes part of that path's expression, a small
  net several paths read is copied into each, and a net nothing reads is
  dropped with its driver. A stored net costs a store and a load, and in a
  partitioned settle a comparison and a mark of its readers as well, because a
  partition forwards nothing to another one. The pass runs only for pure
  SystemVerilog designs in two-state mode whose dead-store policy lets signals
  go, and only on nets nothing but combinational paths reads: every net a
  flip-flop, a process, an event or the runtime reads, every port the policy
  keeps, and every net of a combinational loop stays. A waveform, a live or
  forced signal and a watched object turn it off, as does
  `-sir:net_inlining` (`SirPass::NetInlining`).

This layer may use design dependency facts, but it does not know physical memory
offsets or target instructions.

## SIR layer

`celox-sir-opt` owns backend-independent transformations over execution units and
their control-flow graphs. The main families are:

| Family | Purpose |
|---|---|
| Forwarding and coalescing | Reuse known state values and combine adjacent bit-range accesses |
| CFG simplification | Fold proven branches, remove unreachable blocks, and simplify block arguments |
| Value simplification | Eliminate duplicate expressions, redundant concatenations, and algebraic identities |
| Commit optimization | Avoid unnecessary Stable/Working copies and sink commits toward their producers |
| Dead state removal | Remove writes that are not observable under the selected preservation policy |
| Scheduling | Reorder independent work while preserving data and memory dependencies |
| Layout requirements | Record proven state aliases for validation during physical layout |

Passes exchange analysis results through SIR- and design-owned identities. They
must not inspect Veryl syntax, assume x86 instruction costs as correctness facts,
or mutate a finalized physical layout.

Some transformations are ordered deliberately. For example, control-flow and
forwarding passes can expose dead values, while coalescing may create wider
operations that a later target either keeps or splits. The pass manager owns this
ordering; individual passes should not invoke one another as hidden pipelines.

## Physical layout boundary

Optimization may prove that two semantic state homes can share storage, but it
does not assign byte offsets. It emits a layout requirement, and
`celox-state-layout` validates width, state representation, and region
compatibility before applying the alias.

This separation keeps semantic rewrites independent of packing decisions and
prevents a backend from changing addresses after code generation begins.

## Native machine layer

The x86-64 backend lowers SIR into a private word-level SSA machine IR. At this
point it can use target facts that would be inappropriate in SIR, including:

- immediate instruction forms and x86 operand constraints;
- constant folding and algebraic simplification at machine width;
- copy propagation, global value numbering, and dead-code elimination;
- branch simplification and if-conversion;
- known-bits reasoning and redundant-mask elimination;
- target instruction selection for operations such as population count or bit
  extraction;
- pressure-aware scheduling, spilling, and register allocation.

The compiled scheduler of a native image calls the larger bodies it runs, the
settle and the FF domains, as functions of their own instead of inlining them
into one function with the whole schedule: compiling one function takes time
and memory that grow faster than its size, and the bodies compile in parallel.
A body that is a unit of the settle calls the settle's own code. Bodies below
256 SIR instructions stay inline, where a call costs more than the body. A
design with a testbench scheduler keeps them inline, as does
`-sir:scheduler_callees` (`SirPass::SchedulerCallees`), which is on at every
level.

A timed simulation built on the x86-64 native backend (`Simulation::build`)
compiles each function unit, an out-of-line SystemVerilog subroutine, at its
first call. Until that call the unit's slot in the extern address table holds
a stub, which has a compile thread compile the unit with the same pipeline
and options an image compiles it with, points the slot at the code and
continues the call. The threads start at the first call, as many as an
eager compile of the units would use, with the stack an eager compile worker
has. While the caller waits, the other threads compile the units the called
unit calls, the callees of the latest call first: most of them are called
soon after, and a call of a unit being compiled waits for that compile. On a
UVM case about a quarter of these compiles are never called: the threads
spend that work on a shorter wait. A unit that neither the simulation nor a
called unit calls is never compiled; a design without function units
compiles as before. A compile error at a call ends the call as a simulation
error. The program stays in memory for these compiles, so such an image
holds no code for its units and cannot be loaded again or written out.
Images that are written out or loaded again (`compile_native`,
`build_native_from_image`) compile every unit, as do codegen traces, the
tiered backend and AArch64.

At O2 the SystemVerilog frontend builds eligible two-state register updates
and combinational expressions in the same SLT arena. Each register range has
a next-value root and samples its inputs on its trigger. Combinational
definitions and these roots are canonicalized together before edge SIR is
emitted. A group with a common clock enable can share a branch. Effectful
procedures retain their position between groups of register updates. A split
edge schedule samples into working state and publishes the values in its NBA
phase. For a small pure RTL schedule, the combinational and clocked roots can
also share registers within one combined SIR unit. The selector is
`-sir:next_state_dag` to disable or `+sir:next_state_dag` to enable explicitly.

Cranelift performs the corresponding target optimization through its own IR and
pass pipeline. WebAssembly generation likewise remains backend-private. No
machine-level result is written back into SIR.

## Runtime layer

Runtime optimizations avoid invoking compiled work when event semantics prove it
unnecessary:

- an event that does not match the required edge does not evaluate its domain;
- a non-cascaded single domain may use a combined evaluate-and-commit kernel;
- cascaded evaluation continues only while newly triggered domains exist.
- an edge of an event with derived clock domains (gated clocks, dividers) runs
  as generated units, which judge the gated clocks, run the domains and the
  cascade rounds, instead of a walk of the derived-tick plan in Rust. These units
  compile the clock's domain once more; the `derived_edge_units` selector
  (`-sir:derived_edge_units`, `SirPass::DerivedEdgeUnits`) turns them off to
  trade tick speed for build time.
- the combinational settle can be cut into partitions, contiguous ranges of
  its schedule, that run only when their dirty flag is set
  (`partitioned_settle`, off unless enabled). A partition that runs sets the
  flags of the partitions reading what it writes; every other writer sets them
  too: generated stores and commits of state a partition reads or writes, host
  writes, state loads, and a retained raw view of the state, after which every
  partition runs. `SimulatorBuilder::settle_check` runs each settle a second
  time with every partition flagged and panics where the two differ; effects
  of partitions that run twice, such as runtime events, then happen twice.
  `SimulatorBuilder::settle_plan_output` (CLI: `--dump-settle-plan FILE`)
  writes the plan: each partition's cost, the bits it reads, the chunks it
  compares after it ran and the partitions each chunk marks, headed by a
  lint of backward reader edges (rerun in the next settle pass) and of
  readers marked for bits they do not read, which must be none.
  `SimulatorBuilder::settle_profile` (CLI: `--settle-profile FILE`) adds run
  counters after the flags: each partition's runs, the runs in which
  something it compares changed, and the settles (`settle_profile()` on the
  simulation). A run that changed nothing compared is work the settle could
  have skipped, such as unrelated logic sharing a partition with state that
  changes every cycle.
  `SimulatorBuilder::activation_profile` (CLI: `--activation-profile FILE`,
  which writes these counters too) makes the compiled scheduler of a native
  build count its own work: slots, domain judgments and how many found no
  edge, domain units, kernel runs and edges, settle requests by cause, settle
  runs and skips, event-driven settle groups and observers
  (`activation_profile()` on the simulation; see `docs/benchmarks/rtlmeter.md`
  for the per-clock reports).
- partition sizing in estimated mode is a heuristic, not a derived optimum
  (`SETTLE_PARTITION_COST`, `TARGET_ESTIMATED_SETTLE_PARTITIONS` in the
  frontend's assembly). It pays off only for large designs whose per-cycle
  activity is sparse and localized, with shallow dependency propagation;
  densely toggling logic pays the flag tests and extra runs for nothing.
  The current values were tuned on the RTLMeter designs (E906: ~10%
  steady-state gain from target cost 51 to 32, 2,610 to 3,900 partitions;
  E902 unchanged; finer and coarser targets both slower). Treat them as
  due for re-examination with `--settle-profile` evidence when a design
  with different activity characteristics appears; an activity-adaptive
  choice would be preferable to retuning the constants by hand.
- in the time slots a compiled scheduler runs, a combinational settle waits
  until its results are read: it runs, where the reference loop last
  settled, before a process kernel or domain that reads combinational outputs
  or may end the run mid-slot (`$finish`, an error), and before the run
  returns, so every reader sees the reference's values. A domain that only
  reads combinational outputs runs its fused evaluate-and-commit units instead
  where the frontend provides them (Veryl). Combinational logic that keeps state
  between settles (latches, guarded processes such as `always @*`, feedback)
  settles where the reference loop does.

These choices preserve the ordering model defined in
[Runtime Semantics](./cascade-limitations.md). They do not change signal values or
relax simulation consistency.

## Correctness boundary

Every compiler transformation must preserve observable state, events, errors,
and four-state value/mask behavior. A performance result can justify enabling or
disabling a legal transformation, but it is never evidence that an otherwise
unproven rewrite is correct.
