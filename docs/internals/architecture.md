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

A procedural process is compiled into a process kernel: one SIR execution unit
that runs the process from its last suspension point to the next. Each process
owns three hidden two-state state objects:

- a resume slot, which the kernel's entry block dispatches on with a `Switch`
  (two levels beyond 255 suspension points);
- a status slot, in which the kernel reports how it returned: a delay, an
  inactive-region delay, a wait, a still-pending wait, a staged nonblocking
  assignment, a yield, the end of the process, or `$finish`;
- a delay slot, which holds the wait amount, or the index of the wait or
  nonblocking site.

A suspension stores these slots and returns, so no backend needs a new
terminator. Kernels are never merged with each other or with a phase, and
every backend compiles each one as its own function, invoked through
`SimBackend::run_process`. `ProcessKernelBuilder` is the frontend interface:
`delay`, `inactive_delay`, `yield_now`, `suspend_wait` with `wake_if` and
`wait_level`, `nonblocking` and `finish`.

#### Watched and counted signals

A wait names its terms: a signal and the occurrence it waits for (any change,
a positive or a negative edge, IEEE 1800-2023 9.4.2 Table 9-2). Most signals
change at most once between two points at which the runtime checks them: a
scheduled or host write, a flip-flop publication, a continuous assignment
settling, or a kernel storing it once between suspensions. For these the
runtime keeps the value each waiting process last saw and compares, and
flip-flop domains keep today's edge detection.

A signal is *counted* when a unit may store it twice without a check in
between, so an intermediate value would otherwise be lost: a kernel storing it
twice between two suspensions or inside a loop without one, or an imperative
combinational unit storing it twice on one path (`y = 0; if (a) y = 1;`).
Frontends report such candidates with counter objects
(`watch::multi_store_candidates`, `CounterSlots`); assembly counts only the
candidates a flip-flop domain or a wait watches. It then rewrites exactly the
stores to them in kernels and combinational process units with
`watch::emit_watched`, which bumps changes/rises/falls counters around each
store in plain SIR. A counted domain triggers when its counter moves, and a
wait on a counted signal compares counters. Combinational process units run
only when an input changed (IEEE 1800-2023 9.2.2.2), so settling again never
repeats an intermediate write. Nothing else in the design changes.

#### Time slots

A time slot is a sequence of rounds. A round is the batch step described above
with a process phase in front of the flip-flop domains, following the regions
of IEEE 1800-2023 4.5:

1. apply the scheduled values of the time;
2. run the ready processes in declaration order, including those they wake,
   then settle once (the batch step's settle) and wake the processes the
   settled values satisfy; when only inactive-region (`#0`) processes remain,
   evaluate the domains triggered so far first and keep their nonblocking
   assignments for the NBA region;
3. evaluate and publish the triggered domains as in the batch step, and
   publish the processes' nonblocking assignments with them;
4. wake the processes the published values satisfy; they, and processes that
   waited for a zero `Delay`, run in the next round of the same time.

A process woken by an edge therefore runs before that edge's domains publish:
it reads flip-flop outputs from before the edge, and a process that drives a
flip-flop input at the same edge races with the flip-flop, as in
SystemVerilog. A round without ready processes, waiting processes or counted
signals does exactly the batch step, with the fused evaluate-and-apply and
parallel units. A domain evaluated twice before its nonblocking assignments
publish keeps the earlier evaluation's working state, and both publish in
order. Nonblocking assignments of kernels go through an ordered queue, or a
single pending value per site when the frontend knows that nothing else
assigns the destination.

`$finish`, from a kernel or from flip-flop or combinational code, ends a timed
simulation where it is: the scheduler propagates it as a control result, and
the simulator freezes so that evaluation, host writes and dumps change
nothing. A host write is an update of the current time; the next step settles
it at that time, then advances, so host writes never hold the simulation at
one time. Checkpoints, state files and exported schedules carry
the waiting processes with what each compares against, the ready and inactive
processes, the queued nonblocking assignments and the counter values counted
domains last triggered on; the slots and counters themselves are ordinary
state. The optimizer does not yet optimize kernels, but it treats their
accesses like those of FF domains, so identity aliasing and dead-store
elimination keep the state they use.

A running simulator retains the elaborated design, source lookup, runtime schema,
bound testbench bytecode, and compiled backend. The backend owns the finalized
layout. Frontend and optimizer state are discarded after compilation.

Veryl testbench expressions and resumable processes lower to SIR before
optimization and state layout, using the same process builder and code-generation
backends as SystemVerilog. Integer operations, including wide values, use normal
SIR instruction selection. Strings and floating-point operations use the shared
heap and numeric ABI. There is no separate testbench VM, assembler or Cranelift
compiler. Tiered execution promotes the DUT and testbench kernels together.

The compiled process stores its continuation before requesting a clock/reset
wait or a host service such as display, assertions, random generation or component
calls. The Veryl event scheduler resumes these kernels and retains clock order,
private process state, tick limits and heap roots. This does not change
`clock_gen` or `clock.next` timing. Compiler-only builds retain the same lowering
and serialized kernel metadata without requiring a host JIT.

## Public API boundary

The Rust `celox` crate is a facade over the compiler and runtime components. The
Node binding and TypeScript package expose that facade to user testbenches. API
options select compiler policy or runtime behavior, but callers do not construct
phase artifacts or depend on concrete backend internals.
