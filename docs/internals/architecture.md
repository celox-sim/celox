# Simulator Architecture

Celox compiles Veryl and supported SystemVerilog RTL into executable simulation
kernels and runs them with an event-driven runtime. The architecture is
optimized for synchronous RTL verification rather than gate-level timing or
detailed delta-cycle emulation.

## System overview

```text
Veryl source ──────────┐
                      ├─► frontend analysis and hierarchy elaboration
SystemVerilog source ──┘
    │
    ▼
symbolic logic (SLT) and dependency scheduling
    │
    ▼
Simulator IR (SIR) and backend-independent optimization
    │
    ▼
physical state layout
    │
    ├──► native x86-64 code
    ├──► native AArch64 code
    ├──► Cranelift JIT code
    └──► WebAssembly
             │
             ▼
      event-driven runtime
```

Compilation uses distinct artifacts for each phase. Source-language objects do
not leak into optimization or code generation, and target-specific IR does not
leak back into SIR. See [Compiler Components](./compiler-crate-architecture.md)
for the crate and artifact boundaries.

## Frontend and scheduling

The shared frontend owns language adapters, hierarchy elaboration, and the source
lookup information needed by diagnostics and public signal paths. The reusable
SystemVerilog parser and semantic analyzer remain isolated in
`celox-sv-analyzer`; its Celox lowering adapter lives beside the shared assembly
instead of depending on the Veryl adapter. Combinational expressions from either
language are represented as symbolic logic trees (SLT).

The scheduler then:

1. flattens the elaborated hierarchy;
2. assigns source-independent state identities;
3. derives combinational dependencies and clock domains;
4. orders combinational work and detects dependency cycles;
5. lowers scheduled logic into SIR execution units.

After this transition, downstream phases use design identities and SIR rather
than Veryl parser nodes or SLT arena identifiers. The scheduling algorithm is
described in [Combinational Analysis](./combinational-analysis.md).

## SIR, optimization, and layout

SIR is the common source- and target-independent control-flow IR. It represents
bit-precise loads, stores, arithmetic, selection, and the execution units needed
for combinational and sequential phases.

Backend-independent passes simplify and reschedule SIR before physical addresses
are assigned. Layout then maps semantic state objects into regions of one
simulation buffer:

- **Stable** contains committed signal and register values.
- **Working** contains next-state values during split-phase sequential evaluation.
- **Sparse working metadata** tracks state that only needs selective commit.
- **Triggered bits** record event-producing changes.
- **Scratch** is backend-requested temporary storage when a kernel must be split.

Four-state objects store a value plane and a mask plane. Layout is immutable once
code generation begins, so every backend sees the same state representation.

See [SIR Reference](./ir-reference.md) and
[Optimization Architecture](./optimizations.md) for these layers.

## Backends

All backends consume laid-out SIR and implement the runtime's backend contract.

### Native x86-64

The default x86-64 backend lowers SIR to a private machine IR, performs
machine-level optimization and register allocation, and emits executable code.
Its instruction selection, allocation data, and executable-memory management are
not part of the shared compiler model.

### Native AArch64

The AArch64 backend owns its register policy, ABI lowering, and executable code
emission. Its machine IR and machine-level optimizations are intentionally
separate from x86. Both native backends may export opcode-free control-flow,
use/def, and register-constraint facts to shared allocation algorithms; those
facts are not a common machine IR.

On AArch64 the backend is included directly and selected as the default backend.

The simulator builder exposes the architecture-specific `build_x86_64` and
`build_arm64` entry points alongside `build_cranelift`. `build_native` is the
portable convenience entry point and routes to the backend for the compilation
target. Consequently, `build_arm64` is also available in Cargo cross-builds
targeting AArch64. Host-side generation of another architecture's images uses
the separate, default-off cross-codegen features and compilation/image API.
`arm64-codegen` selects AArch64 emission on an x86-64 target, while
`x86_64-codegen` selects x86-64 emission on an AArch64 target. The two features
are mutually exclusive.

### Native program images

The x86-64 and AArch64 compilers first produce a pointer-free
`NativeProgramImage`. Every generated evaluation function is copied intact into
one 16-byte-aligned code image; callable entries and event bindings are retained
as offsets from the image base. Internal branches, jump tables, constant tables,
and literal data remain inside their originating function blob.

The precompiled host runtime attaches the image by copying it once into
executable memory and resolving the recorded offsets into process-local function
pointers. The same artifact can therefore be copied to another address and
reattached without recompiling the design.

The image also carries a source-independent reflection table. It records the
elaborated instance tree and name-sorted signals, including their directions,
signedness, packed and unpacked dimensions, four-state representation, and
runtime state offsets. A precompiled runtime can therefore implement foreign
interfaces such as VPI without retaining the Veryl analyzer or compiler IR.

`NativeProgramImage` can also be serialized after a precompiled runtime
executable. A fixed-size EOF trailer records the container version, target ISA,
payload length, and checksum, so startup can find the design without parsing ELF
or another platform executable format. Replacing an attached design preserves
the runtime prefix and its file permissions. Container versions are validated
strictly; compatibility between different versions is not implicit.

`NativeProgramInstance` is the runtime-only entry point for the resulting
artifact. It discovers an image in arbitrary runtime bytes or at the end of the
current executable, maps the code into executable memory, allocates independent
state, and exposes reflection plus the native backend to foreign-interface
adapters. Loading and executing this path does not require source text or a
compiler artifact.

The `celox-vpi` runtime layer exports the VPI C ABI directly on this instance.
It supports module/scope/signal handles, hierarchy iteration, common integer
and string properties, scalar and vector value access, and the callback regions
used by cocotb. The precompiled runtime loads cocotb's ordinary Icarus VPI
adapter and drives timers, phase callbacks, value-change callbacks, and native
clock/reset events without invoking the compiler. Delayed VPI writes and the
full IEEE object model remain outside the initial compatibility subset.

### Cranelift

The Cranelift backend translates SIR to Cranelift IR and uses Cranelift's JIT. It
is the native fallback where neither custom native backend is selected.

### WebAssembly

The WebAssembly backend translates the same SIR and layout into a Wasm module. It
supports both the Rust host path and the browser playground.

## Runtime execution

The runtime owns scheduling and observable simulator behavior. A simulation step
has four conceptual stages:

1. take all events scheduled for the current time;
2. detect clock edges and collect triggered domains;
3. evaluate next state from the current stable state;
4. commit updates and repeat combinational propagation until the step settles.

When several domains trigger together, evaluation and commit are separated so
that every domain reads the same pre-update state. If a committed result drives
another clock, the runtime discovers that domain and continues within the same
step. [Runtime Semantics](./cascade-limitations.md) describes this behavior and
its boundaries.

### Process kernels

A procedural process with timing controls is compiled into a process kernel:
one SIR execution unit that runs the process from its last suspension point
to the next. Each process owns three hidden two-state state objects:

- a resume slot, which the kernel's entry block dispatches on with a `Switch`
  (two levels beyond 255 suspension points);
- a status slot, in which the kernel reports a delay, a wait, a clock wait,
  the end of the process, or `$finish` before it returns;
- a delay slot, which holds the wait amount: time units or clock edges;
- a clock slot, which names the process clock of a clock wait among the
  clocks the process may wait on, listed with their periods in the runtime
  schema.

A suspension stores these slots and returns, so no backend needs a new
terminator. Kernels are never merged with each other or with a phase, and
every backend compiles each one as its own function, invoked through
`SimBackend::run_process`.

An event control (`@(posedge clk)`, `@(a or b)`) or a level wait
(`wait (expr)`) is a suspension whose condition the kernel evaluates itself.
Before an event wait suspends, the kernel samples each event expression into a
hidden state object of the process. Its resume point compares the current
values with the samples, refreshes the samples and either runs on or reports
that the wait is still pending. The runtime therefore needs no per-signal
waiter lists, four-state edge rules stay inside the generated code, and a
checkpoint captures a waiting process like any other state.

The timed scheduler keeps a queue of process wakeups next to its event queue
and a set of the processes waiting for an event or a condition. At each time
it applies the scheduled values, then runs the processes that resume at that
time together with the waiting ones, in declaration order, repeating the pass
while a process ran, so that a process another one wakes runs before the
registers of that time evaluate; this matches the active-region order in
which a process woken by a clock edge reads the registers' previous values.
It detects the clock edges the processes cause by comparing event signals
before and after, and then settles as for scheduled events. Processes that
waited for zero time, or that the settled state wakes, resume in a further
round at the same time, which repeats the edge detection and settling; a
round in which every waiting process reports a pending wait ends the time.
`Simulation::step` and `run_until` also run such a round first, so a host
write between steps wakes the processes that wait for it.

A clock wait asks for a number of rising edges of a process clock. The
scheduler generates that clock's edges from the first wait on it: a rising
edge is due at the clock's next edge time whenever a process waits, applied
after the processes of that time have run, and its falling edge half a
period later; each rising edge counts against the waits, and a process whose
count is reached resumes one period after that edge, before the edge that
follows. When every live process waits on the single clock that is due and
no event, resumption or falling edge is pending, the scheduler instead runs
as many edges as the earliest resumption needs through
`SimulationExecutor::tick_many`, the same fused combinational-plus-sequential
loop that event-driven ticks use, and advances time past them in one step.
Those ticks leave the clock signal untouched and skip the per-edge hooks of
external components, which is why the host can veto them. Because the slots
are ordinary state, a checkpoint captures suspended processes, and the
scheduler's snapshot carries the generated clocks and clock waits. The
optimizer does not yet optimize kernels, but it treats their accesses like
those of FF domains, so identity aliasing and dead-store elimination keep
the state they use.

A running simulator retains the elaborated design, source lookup, runtime schema,
bound testbench bytecode, and compiled backend. The backend owns the finalized
layout. Frontend and optimizer state are discarded after compilation.

## Public API boundary

The Rust `celox` crate is a facade over the compiler and runtime components. The
Node binding and TypeScript package expose that facade to user testbenches. API
options select compiler policy or runtime behavior, but callers do not construct
phase artifacts or depend on concrete backend internals.
