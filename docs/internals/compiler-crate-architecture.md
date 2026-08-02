# Compiler Components

Celox separates source-language analysis, simulator IR, target code generation,
and runtime execution into crates with one-way dependencies. This page describes
the current ownership boundaries. It is not a migration plan or a record of past
refactors.

## Pipeline

```text
Veryl source
    │
    ▼
celox-frontend-veryl ──► SymbolicRtl ──► ScheduledRtl
          │                    │                │
          │              celox-slt              ▼
          │                              UnoptimizedSir
          │                                    │
          │                              celox-sir-opt
          │                                    ▼
          │                               OptimizedSir
          │                                    │
          │                           celox-state-layout
          │                                    ▼
          │                              LaidOutProgram
          │                                    │
          │                 ┌──────────────────┼──────────────────┐
          │                 ▼                  ▼                  ▼
          │          backend-x86      backend-cranelift    backend-wasm
          │                 └──────────────────┼──────────────────┘
          │                                    ▼
          └───────────────────────────── RuntimeProgram
                                               │
                                         celox-runtime
```

The `celox` crate is the public facade and compiler driver. It wires these phases
together, selects a backend, and exposes the simulator API. Lower-level crates do
not depend on the facade.

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
| `celox-frontend-veryl` | Veryl analysis, source lookup, module construction, and frontend diagnostics | Optimization or target code generation |
| `celox-slt` | Symbolic logic trees, dependency scheduling, and SLT-to-SIR lowering | Veryl parser details or physical layout |
| `celox-sir` | Backend-independent simulator IR and control-flow structures | Target instructions or runtime scheduling |
| `celox-sir-opt` | Backend-independent SIR analyses and transformation passes | Veryl ASTs or target MIR |
| `celox-state-layout` | Semantic-to-physical state mapping and layout validation | Optimization policy or executable memory |
| `celox-backend-x86` | x86 MIR, instruction selection, register allocation, and machine-code emission | Frontend or runtime policy |
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
contain SLT identities and Veryl-specific lookup information because it has not
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

1. Source-language types stop at the frontend boundary.
2. Semantic state identities remain distinct from physical memory offsets until
   layout finalization.
3. SIR optimizations are independent of any concrete backend.
4. Target MIR, register allocation, and emission remain private to their backend.
5. Runtime code depends on backend contracts, not concrete compiler pipelines.
6. Testbench execution uses source-independent bytecode; only the frontend parses
   Veryl testbench syntax.
7. The facade coordinates phases but does not become a second owner of their
   algorithms or data structures.

These rules are enforced primarily by Cargo dependencies and artifact types. A
new dependency that points from a lower layer back toward the facade or frontend
is therefore an architectural change, not a convenient shortcut.

## Where changes belong

- A new Veryl construct or source diagnostic belongs in `celox-frontend-veryl`.
- A symbolic scheduling rule belongs in `celox-slt`.
- A backend-independent instruction or CFG rule belongs in `celox-sir`.
- A backend-independent transformation belongs in `celox-sir-opt`.
- A memory-region or address-placement rule belongs in `celox-state-layout`.
- An x86 instruction, register constraint, or emission rule belongs in
  `celox-backend-x86`.
- Event ordering, timed execution, or VCD behavior belongs in `celox-runtime`.
- Public construction options and backend selection belong in the `celox` facade.

See [Simulator Architecture](./architecture.md) for the end-to-end data flow and
[SIR Reference](./ir-reference.md) for the backend-independent instruction model.
