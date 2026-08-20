# Compiler Components

Celox separates source-language analysis, simulator IR, target code generation,
and runtime execution into crates with one-way dependencies. This page describes
the current ownership boundaries. It is not a migration plan or a record of past
refactors.

## Pipeline

```text
Veryl source ----------> celox-frontend-veryl --------------------\
SystemVerilog source --> celox-sv-analyzer --> celox-frontend-sv --+--> celox-frontend-core
External frontend -----> celox-frontend-sdk artifact -------------/             |
                                                                                 v
                                                                            SymbolicRtl
                                                                                 |
                                                                            celox-slt
                                                                                 v
                                                                            ScheduledRtl
                                                              │
                                                        UnoptimizedSir
                                                              │
                                                        celox-sir-opt
                                                              ▼
                                                         OptimizedSir
                                                              │
                                                     celox-state-layout
                                                              ▼
                                                        LaidOutProgram
                                                              │
                                           ┌──────────────────┼──────────────────┐
                                           ▼                  ▼                  ▼
                                    backend-x86      backend-cranelift    backend-wasm
                                           └──────────────────┼──────────────────┘
                                                              ▼
                                                       RuntimeProgram
                                                              │
                                                        celox-runtime
```

The `celox` crate is the public facade and compiler driver. It wires these phases
together, selects a backend, and exposes the simulator API. Lower-level crates do
not depend on the facade. `celox-backend-x86` and `celox-backend-arm64` depend on
`celox-backend-common` for allocation machinery; that crate is a compile-time
library, not another pipeline artifact.

The frontend boundary is split into a published authoring contract, one
source-independent adapter crate, and parser-backed adapters for bundled source
languages. `celox-frontend-sdk` owns a versioned, serializable artifact that an
external frontend can construct without depending on Celox compiler internals.
`celox-frontend-core` owns the symbolic assembly,
flattening, source lookup, tracing, and scheduled output contracts shared by all
adapters. It must not depend on a parser, analyzer, or language adapter.

`celox-frontend-veryl` owns Veryl analysis, lowering, diagnostics, and testbench
source sidecars. `celox-frontend-sv` depends on the independently reusable
`celox-sv-analyzer` and adapts analyzed SystemVerilog into the core symbolic
vocabulary. Both adapters may depend on `celox-frontend-core`; neither adapter
may depend on the other. The public `celox` compiler driver selects an adapter
and consumes core contracts directly, so there is no multi-language frontend
facade or language-specific compatibility re-export.

Each adapter projects parser-native identities into the source-independent
`SourceVarId` namespace before constructing core symbolic structures. Veryl IDs
therefore remain in the Veryl adapter and its source sidecars; they do not enter
`celox-frontend-core`, `FrontendLookup`, or `ScheduledRtl`. Scheduled design
state, SIR, optimization, layout, and backends use `celox-design` identities.

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
| `celox-frontend-sdk` | Published, versioned frontend artifact schema and validated module builder | Parser types, Celox compiler internals, or backend policy |
| `celox-frontend-core` | Source lookup, shared symbolic assembly, flattening, tracing, and scheduled frontend contracts | Parser, analyzer, or language-adapter dependencies |
| `celox-frontend-sv` | SystemVerilog hierarchy preparation and lowering into frontend-core contracts | Veryl dependencies, optimization, or target code generation |
| `celox-frontend-veryl` | Veryl analysis, lowering, diagnostics, and testbench source sidecars | SystemVerilog dependencies, optimization, or target code generation |
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
   `celox-frontend-core`.
2. External frontend lowering depends on `celox-frontend-sdk`; a Rust simulator
   adapter additionally depends on `celox`, and an N-API/WASI adapter on
   `celox-napi`. Internal symbolic and scheduled types are not a public
   compatibility surface.
3. Semantic state identities remain distinct from physical memory offsets until
   layout finalization.
4. SIR optimizations are independent of any concrete backend.
5. Target MIR, allocation policy, ABI handling, and emission remain private to
   their backend; target-independent allocation mechanisms belong in
   `celox-backend-common`.
6. Runtime code depends on backend contracts, not concrete compiler pipelines.
7. Testbench execution uses source-independent bytecode; only the frontend parses
   Veryl testbench syntax.
8. The facade coordinates phases but does not become a second owner of their
   algorithms or data structures.

These rules are enforced primarily by Cargo dependencies and artifact types. A
new dependency that points from a lower layer back toward the facade or frontend
is therefore an architectural change, not a convenient shortcut.

## Where changes belong

- A new Veryl lowering rule or source diagnostic belongs in the Veryl adapter
  `celox-frontend-veryl`.
- SystemVerilog syntax and semantic rules belong in `celox-sv-analyzer`; their
  conversion into Celox symbolic modules belongs in the SystemVerilog adapter
  `celox-frontend-sv`.
- Source-independent frontend lookup, symbolic assembly, flattening, and tracing
  belong in `celox-frontend-core`.
- Stable external-frontend construction and serialization belong in
  `celox-frontend-sdk`; a netlist parser or other source-specific analysis stays
  in the independently developed frontend.
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
