# Compiler Components

Celox separates source-language analysis, simulator IR, target code generation,
and runtime execution into crates with one-way dependencies. This page describes
the current ownership boundaries. It is not a migration plan or a record of past
refactors.

## Pipeline

```text
Veryl source ───────────────┐
                            ├─► celox-frontend ─► SymbolicRtl ─► ScheduledRtl
SystemVerilog source        │         │                 │               │
        │                   │         │            celox-slt            ▼
        ▼                   │         │                         UnoptimizedSir
celox-sv-analyzer ──────────┘         │                               │
                                      │                         celox-sir-opt
                                      │                               ▼
                                      │                          OptimizedSir
                                      │                               │
                                      │                      celox-state-layout
                                      │                               ▼
                                      │                         LaidOutProgram
                                      │                               │
                                      │            ┌──────────────────┼──────────────────┐
                                      │            ▼                  ▼                  ▼
                                      │     backend-x86      backend-cranelift    backend-wasm
                                      │            └──────────────────┼──────────────────┘
                                      │                               ▼
                                      └──────────────────────── RuntimeProgram
                                                                      │
                                                                celox-runtime
```

The `celox` crate is the public facade and compiler driver. It wires these phases
together, selects a backend, and exposes the simulator API. Lower-level crates do
not depend on the facade. `celox-backend-x86` and `celox-backend-arm64` depend on
`celox-backend-common` for allocation machinery; that crate is a compile-time
library, not another pipeline artifact.

`celox-frontend` is intentionally a multi-language frontend boundary rather than
a dependency from one language frontend to another. Its SystemVerilog feature
depends on the independently reusable `celox-sv-analyzer`, then adapts analyzed
SV into the same symbolic module vocabulary used by Veryl. That internal
vocabulary still uses Veryl-shaped local IDs and metadata in places; those types
are confined to the private symbolic compatibility core and Veryl-owned source
sidecars. They do not enter `FrontendLookup` or `ScheduledRtl`. Scheduled design
state, SIR, optimization, layout, and backends use `celox-design` identities. A
future neutralization of the local vocabulary can therefore happen inside this
crate without recreating a `frontend-sv -> frontend-veryl` dependency.

The frontend crate's internal modules make that ownership explicit:

| Module | Owns | Dependency rule |
|---|---|---|
| `shared` | `SourceVarId`, `FrontendLookup`, and scheduled output contracts | Must not import parser or analyzer types |
| `veryl` | Veryl analysis, module lowering, dynamic-loop diagnostics, and testbench source sidecars | May lower into `symbolic`; must not own SV analysis |
| `systemverilog` | SV analysis adapter, hierarchy preparation, and SV lowering | May lower into `symbolic`; must not import `veryl` |
| `symbolic` | Private pre-scheduling compatibility vocabulary and assembly | Must not be exposed as the runtime/frontend public contract |

The crate root is a facade over those areas. Parser-native aliases are exposed
only through the owning language module; source-independent consumers import
contracts from `shared`.

`celox-backend-arm64` is wired into native backend selection behind the
default-off `experimental-arm64-backend` feature and emits complete scalar
simulation kernels. Its production path temporarily uses the established
x86-owned scalar lowering and allocation pipeline as a compatibility bridge.
The migration target is separate x86 and AArch64 MIR pipelines which export only
opcode-free allocation facts to `celox-backend-common`.

## Native MIR and allocation boundary

Native backends do not share an instruction enum. Each target owns instruction
selection, machine optimization, legalization, register classes, ABI rules,
spill/reload construction, and post-allocation rewriting:

```text
                         x86 instruction selection -> X86 MIR -> x86 passes
LaidOutProgram -> SIR --<                                           |
                         A64 instruction selection -> A64 MIR -> A64 passes
                                                                     |
                                      opcode-free allocation facts <-+
                                                                     |
                                      shared allocation algorithms
```

Immediately before a reusable allocation analysis, a backend projects its MIR
into normalized facts: CFG successors, phi edges, virtual-register uses and
definitions, fixed operands, clobbers, and copy hints. Shared code must consume
these facts instead of matching target opcodes. Target drivers remain responsible
for turning allocation decisions into legal machine instructions.

Optimizations which are genuinely independent of machine instruction shape
belong in `celox-sir-opt`. DCE or CFG algorithms may be shared over allocation
facts when their required semantics are fully represented, but common MIR
opcodes must not be introduced merely to reuse a pass.

The facade's default `host-runtime` feature owns host execution, Cranelift,
Wasmtime, the x86 backend, and the test macro. WebAssembly bindings disable that
feature and depend only on the shared compiler plus `celox-backend-wasm`. Target
checks therefore remain at the x86 selection boundary; shared compiler crates do
not infer optimization or timing policy from the architecture they are compiled
for.

## Component ownership

| Crate | Owns | Must not own |
|---|---|---|
| `celox-analysis` | Reusable graph and data-flow algorithms | Veryl or backend-specific types |
| `celox-design` | Source-independent design identities, hierarchy, events, and runtime schema | Parser nodes or physical addresses |
| `celox-sv-analyzer` | Reusable SystemVerilog syntax and semantic analysis | Celox scheduling or Veryl dependencies |
| `celox-frontend` | HDL adapters, source lookup, module construction, shared symbolic assembly, and frontend diagnostics | Optimization or target code generation |
| `celox-slt` | Symbolic logic trees, dependency scheduling, and SLT-to-SIR lowering | Veryl parser details or physical layout |
| `celox-sir` | Backend-independent simulator IR and control-flow structures | Target instructions or runtime scheduling |
| `celox-sir-opt` | Backend-independent SIR analyses and transformation passes | Veryl ASTs or target MIR |
| `celox-state-layout` | Semantic-to-physical state mapping and layout validation | Optimization policy or executable memory |
| `celox-backend-common` | Target-independent register sets, constraints, locations, and allocation mechanisms | Target registers, ABI rules, or instruction encodings |
| `celox-backend-arm64` | AArch64 MIR, register policy, AAPCS64 lowering, and machine-code emission | x86 constraints or frontend policy |
| `celox-backend-x86` | x86 MIR, instruction selection, target allocation policy, and machine-code emission | Frontend or runtime policy |
| `celox-backend-cranelift` | Cranelift translation and JIT construction | Frontend or x86-specific MIR |
| `celox-backend-wasm` | WebAssembly module generation | Host runtime behavior |
| `celox-testbench` | Source-independent testbench bytecode and values | Veryl AST traversal or simulator memory ownership |
| `celox-runtime` | Events, timed scheduling, VCD output, testbench execution, and backend contracts | Frontend, SIR optimization, or concrete backend internals |
| `celox` | Public API, compilation orchestration, and backend selection | New reusable compiler algorithms |

## Phase artifacts

Each major transition consumes an artifact and returns the next one. This keeps
phase validity in the type system instead of representing it with optional fields
on a shared mutable object.

### `SymbolicRtl`

Frontend-owned modules with symbolic combinational and sequential logic. It may
contain SLT identities and source-language lookup information because it has not
crossed the frontend boundary yet.

### `ScheduledRtl`

The result of flattening hierarchy, assigning source-independent state identities,
scheduling logic, and lowering symbolic roots. No downstream component needs the
SLT arena to reconstruct runtime behavior.

### `UnoptimizedSir`

Backend-independent SIR plus the design and runtime metadata needed by later
phases. Physical offsets have not been assigned.

### `OptimizedSir`

SIR after the pass manager has applied the selected optimization policy. Only the
optimizer transition constructs this artifact, so layout and code generation
cannot accidentally consume unoptimized input.

### `LaidOutProgram`

Optimized SIR paired with an immutable physical memory layout. All backend-visible
state addresses can now be resolved without consulting Veryl source objects.

### `RuntimeProgram`

The source-independent metadata retained after code generation: elaborated design
metadata, public path lookup, runtime schema, and bound testbench bytecode. A
running `Simulator` stores this artifact beside the executable backend; compiler
IR and layout requirements do not remain live during simulation.

## Dependency rules

The following rules define the intended architecture:

1. Source-language types stop at the frontend boundary. One language adapter
   must not depend on another language frontend; shared assembly belongs in
   `celox-frontend`.
2. Semantic state identities remain distinct from physical memory offsets until
   layout finalization.
3. SIR optimizations are independent of any concrete backend.
4. Target MIR, allocation policy, ABI handling, and emission remain private to
   their backend; target-independent allocation mechanisms belong in
   `celox-backend-common`.
5. Runtime code depends on backend contracts, not concrete compiler pipelines.
6. Testbench execution uses source-independent bytecode; only the frontend parses
   Veryl testbench syntax.
7. The facade coordinates phases but does not become a second owner of their
   algorithms or data structures.

These rules are enforced primarily by Cargo dependencies and artifact types. A
new dependency that points from a lower layer back toward the facade or frontend
is therefore an architectural change, not a convenient shortcut.

## Where changes belong

- A new Veryl lowering rule or source diagnostic belongs in the Veryl adapter
  within `celox-frontend`.
- SystemVerilog syntax and semantic rules belong in `celox-sv-analyzer`; their
  conversion into Celox symbolic modules belongs in the SystemVerilog adapter
  within `celox-frontend`.
- A symbolic scheduling rule belongs in `celox-slt`.
- A backend-independent instruction or CFG rule belongs in `celox-sir`.
- A backend-independent transformation belongs in `celox-sir-opt`.
- A memory-region or address-placement rule belongs in `celox-state-layout`.
- A target-independent register-allocation mechanism or constraint data type
  belongs in `celox-backend-common`.
- An AArch64 instruction, register constraint, or emission rule belongs in
  `celox-backend-arm64`.
- An x86 instruction, register constraint, or emission rule belongs in
  `celox-backend-x86`.
- Event ordering, timed execution, or VCD behavior belongs in `celox-runtime`.
- Public construction options and backend selection belong in the `celox` facade.

See [Simulator Architecture](./architecture.md) for the end-to-end data flow and
[SIR Reference](./ir-reference.md) for the backend-independent instruction model.
