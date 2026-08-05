# Simulator Architecture

Celox compiles Veryl RTL into executable simulation kernels and runs them with an
event-driven runtime. The architecture is optimized for synchronous RTL
verification rather than gate-level timing or detailed delta-cycle emulation.

## System overview

```text
Veryl source
    │
    ▼
frontend analysis and hierarchy elaboration
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

The Veryl frontend analyzes modules, elaborates hierarchy, and records the source
lookup information needed by diagnostics and public signal paths. Combinational
expressions are represented as symbolic logic trees (SLT).

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

The backend is experimental and disabled by default. Enable the
`experimental-arm64-backend` feature on the `celox` crate to select it as the
default backend on AArch64; without the feature, AArch64 uses Cranelift.

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

The `celox-vpi` runtime layer exports the initial VPI C ABI directly on this
instance. It supports module/scope/signal handles, hierarchy iteration, common
integer and string properties, and immediate scalar, integer, vector, binary,
and hexadecimal value access. Delayed writes and callback registration remain
owned by the future simulation-region scheduler integration rather than being
approximated inside the ABI layer.

### Cranelift

The Cranelift backend translates SIR to Cranelift IR and uses Cranelift's JIT. It
is the native fallback where the custom x86-64 backend is unavailable.

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

A running simulator retains the elaborated design, source lookup, runtime schema,
bound testbench bytecode, and compiled backend. The backend owns the finalized
layout. Frontend and optimizer state are discarded after compilation.

## Public API boundary

The Rust `celox` crate is a facade over the compiler and runtime components. The
Node binding and TypeScript package expose that facade to user testbenches. API
options select compiler policy or runtime behavior, but callers do not construct
phase artifacts or depend on concrete backend internals.
