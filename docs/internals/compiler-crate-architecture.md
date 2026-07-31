# Compiler Crate Architecture

## Status

This document defines the target crate architecture and the migration contract for splitting the
current `celox` compiler/runtime monolith. It is a design document, not a claim that the described
crates already exist.

Migration note: `celox-state-layout` now owns the generic layout algorithm and the compiler driver
uses a consuming `Program -> LaidOutProgram` transition. The current facade artifact still wraps
the mixed `Program`; dissolving that payload into the phase-specific target types below remains
part of Milestone 3.

The baseline is the compiler pipeline on `perf/native-simulation-throughput` after PR #322. The
split must preserve RTL semantics, generated-code quality, and the public `celox` API while making
phase ownership explicit.

## Why the current boundary is wrong

The problem is not merely that several source files are large. The current ownership graph has
cycles hidden by Rust modules inside one crate.

`ir::Program` currently owns all of the following at once:

- scheduled and lowered SIR execution units;
- the SLT arena and combinational observers that still refer to `NodeId`;
- hierarchy, module, variable, clock, reset, and runtime-event metadata;
- optimizer output such as `EvalCombPlan` and address aliases;
- an optional physical `MemoryLayout`;
- initial memory contents;
- uncompiled Veryl testbench statements and functions.

This creates concrete reverse dependencies:

- `ir` refers to `logic_tree`, `optimizer`, and `backend::MemoryLayout`;
- `optimizer` accepts `Program`, calls parser verification, and writes optimizer-specific plans
  back into `Program`;
- `backend::memory_layout` accepts `Program` and inspects optimizer plans;
- SLT construction imports parser and Veryl expression helpers, while SLT lowering emits SIR;
- the facade, compiler driver, backend selection, runtime, and testbench VM all live in `celox`.

The result is one mutable object whose valid fields depend on which phase happened to run. An
`Option<MemoryLayout>` and an `Option<EvalCombPlan>` encode pipeline state implicitly, and a backend
can see frontend structures that should have ceased to exist before code generation.

The split must therefore dissolve `Program`; moving directories into crates without changing that
ownership would only reproduce the monolith across package boundaries.

## Goals

1. Make the compilation dependency graph a directed acyclic graph visible to Cargo.
2. Give every phase an input and output type that is valid by construction.
3. Keep source-language objects out of SIR, optimizers, layouts, backends, and runtime.
4. Keep target-specific MIR, register allocation, and emission inside the target backend.
5. Preserve one backend-independent SIR and one shared physical state layout.
6. Allow SLT scheduling and lowering to combine comb and FF work without depending on the Veryl
   parser implementation.
7. Keep `celox-analysis` IR-independent and reusable by SLT, SIR, and backend MIR adapters.
8. Preserve existing public API paths through facade re-exports during migration.
9. Run a relevant semantic test gate after every migration step; do not defer validation until the
   final move.

## Non-goals

- This migration does not redesign RTL event semantics.
- It does not replace SIR, the fused comb/FF scheduler, or the native MIR pipeline.
- It does not require every proposed crate to be created up front.
- It does not create a generic register-allocation crate for an allocator whose constraints are
  currently x86-specific.
- It does not use crate boundaries to justify duplicated IRs, conversion copies, or fallback
  pipelines.
- It does not require release/LTO builds for each mechanical extraction. Release/LTO remains a
  final performance acceptance gate.

## Architectural rules

### Crate dependencies are semantic dependencies

A crate is introduced only when it owns a coherent contract and can be tested through that
contract. Empty placeholder crates and crates that merely re-export another crate are not useful
milestones.

Lower layers never depend on the `celox` facade. A dependency may point down the graph below but
must never be hidden through callbacks, re-export modules, or feature-selected reverse imports.

### Pipeline state is represented by types

The compiler driver moves through distinct artifacts:

```text
Veryl sources
    |
    v
ElaboratedDesign + SymbolicRtl
    |
    v
ScheduledRtl
    |
    v
SirProgram
    |
    v
OptimizedSir + LayoutRequirements
    |
    v
LaidOutProgram
    |
    v
BackendArtifact
    |
    v
ExecutableProgram
```

These are separate types, not one structure with phase-dependent optional fields. A phase may
consume an artifact or borrow it immutably; it must not leave a partially updated artifact behind
on failure.

### Semantic and physical addresses remain separate

Frontend source IDs must not become backend identities.

- `SourceVarId` and Veryl AST/IR identities exist only in `celox-frontend-veryl`.
- `DesignVarId`, `InstanceId`, and `StateObjectId` are dense Celox-owned semantic identities.
- `StateRef { object, range, role }` identifies a semantic state range in SIR.
- `MemoryLayout` maps a semantic state reference to a physical region and byte/bit offset.
- A backend-specific address form may cache resolved offsets, but it cannot become the canonical
  identity used by SIR optimization.

The state role is a typed enum such as `Stable`, `Working`, `SparseWorking`, `Triggered`, or
`Scratch`, not an unvalidated integer passed between phases.

### Physical layout is an immutable compilation result

`MemoryLayout` is not stored as `Option<MemoryLayout>` inside a general program. Layout construction
returns `LaidOutProgram { sir, design, layout, runtime_schema }`.

Pre-layout optimization may produce aliases and placement requirements using semantic objects and
bit ranges. Post-layout transforms may only perform rewrites proven to preserve:

- object liveness;
- alias equivalence;
- allocated byte extent;
- runtime-visible signal identity;
- trigger and event-buffer ranges.

A post-layout pass that changes one of those facts must return to layout construction explicitly;
it cannot mutate the layout behind a backend's back.

### Backend plans do not live in SIR

`EvalCombPlan`, Cranelift tail-call chunks, native MIR spill recipes, and x86 register assignments
are backend or pass-pipeline products. They are not fields of `SirProgram` or `Design`.

The compiler driver may keep a backend plan beside a `LaidOutProgram`, but another backend must be
able to consume the same laid-out SIR without understanding that plan.

## Target crates

### `celox-analysis` (existing)

Owns IR-independent algorithms:

- CFG, dominators, postdominators, loops, and control dependence;
- SSA and MemorySSA construction;
- range-based dependence and interval indexing;
- generic DAG scheduling and pressure-aware ordering.

It owns no SIR, SLT, MIR, backend, or source-language types. Callers adapt their IDs and effects to
dense analysis inputs. The current crate already follows this rule; `cfg_order` should move here or
be replaced by the existing CFG API rather than be copied into another crate.

### `celox-design`

Owns source-language-independent elaborated design data:

- Celox-owned module, instance, variable, event, and state-object IDs;
- hierarchy and variable metadata;
- widths, signedness, two-state/four-state classification, ports, and clock/reset domains;
- bit ranges, state roles, triggers, and runtime-event schema;
- initial state represented without Veryl AST nodes;
- semantic operators shared by SLT and SIR.

It does not own SLT nodes, SIR blocks, physical offsets, compiler options, or executable code.

### `celox-sir`

Owns the backend-independent Simulator IR kernel:

- `ExecutionUnit`, `BasicBlock`, block and register IDs;
- SIR instructions, terminators, values, operators, and register types;
- builder, verifier, display, cloning/remapping, CFG adapter, and EU merge utilities;
- serialization required by tracing and snapshots.

It does not own `Program`, `MemoryLayout`, optimizer passes, SLT nodes, Veryl types, or backend plans.
The IR may initially remain generic over address identity during extraction, but the final public
form uses `celox-design::StateRef`.

### `celox-slt`

Owns source-language-independent symbolic RTL and scheduling:

- `NodeId`, `SLTNode`, arena interning, node facts, and verification;
- symbolic stores, ranges, logic paths, effects, and FF access recipes;
- comb/FF dependency graph construction;
- scheduling, SCC handling, and fused comb/FF ordering;
- SLT-to-SIR lowering through a shared SIR builder.

It may depend on `celox-design`, `celox-sir`, and `celox-analysis`. It must not import Veryl ASTs or
parser helpers. The current `logic_tree::comb` must therefore be split: node/fact/path/state logic
moves here, while Veryl expression traversal stays in the frontend.

### `celox-sir-opt`

Owns backend-independent SIR optimization:

- pass manager, pass options, and pass ordering;
- CFG simplification, GVN, DCE, store/load forwarding, alias discovery, scheduling, and SIR idiom
  recovery;
- pre-layout `LayoutRequirements` and alias proofs;
- explicitly constrained post-layout SIR finalization.

It may use `celox-analysis` and `celox-state-layout`. It must not call parser verification or access
frontend state. Verification needed by a pass belongs in `celox-sir`, `celox-design`, or this crate.

`SirOptimizeOptions` lives here. Cranelift and x86 options do not.

### `celox-state-layout`

Owns shared physical simulation-state layout:

- stable, working, sparse-working, triggered, runtime-event, and scratch region layouts;
- packed and unpacked array layout policy;
- conversion from semantic state objects and layout requirements to immutable offsets;
- layout verification and backend-facing lookup APIs.

It depends on `celox-design` and `celox-sir`, not on optimizers or concrete backends. Scratch
requirements are explicit input data rather than discovered by inspecting an optimizer enum hidden
inside the program.

### `celox-frontend-veryl`

Owns every dependency on Veryl analyzer/parser IR:

- source parsing, analyzer setup, parameter overrides, and diagnostics;
- Veryl expression/type/context-width interpretation;
- module elaboration, hierarchy flattening, and conversion from `VarId` to Celox design IDs;
- construction of source-independent SLT nodes and FF recipes;
- compilation of Veryl initial blocks/functions to testbench bytecode.

It produces `ElaboratedDesign` and `SymbolicRtl`; it does not optimize SIR, choose a memory layout, or
invoke a backend.

### `celox-backend-x86`

Owns the complete self-hosted x86 backend:

- x86 feature detection and target policy;
- SIR instruction selection;
- MIR, legalization, verification, and MIR optimization;
- x86-specific SLP/vector selection;
- register allocation, spill planning, reload recipes, and SSA destruction;
- iced-x86 emission, executable memory, and native tick-loop construction.

The current allocator belongs here because its constraints include GPR/XMM classes, fixed x86
operands, FS/GS state addressing, and x86 reload costs. It must not be named `celox-regalloc` unless
a future allocator has a genuinely target-neutral machine contract and a second user.

The crate name is `celox-backend-x86`, not `celox-native`: the implementation and ABI assumptions
are specifically x86/x86-64 even when the resulting API works on multiple operating systems.

### `celox-backend-cranelift`

Owns Cranelift translation, module/JIT setup, and Cranelift-specific compile options. Its tail-call
or memory-spill plan is returned by its own planning phase and never stored in SIR.

### `celox-backend-wasm`

Owns SIR-to-WebAssembly code generation. Host instantiation through wasmtime may be an optional
feature of this crate; browser consumers use the generated bytes without depending on wasmtime.
Splitting host instantiation into another crate is deferred until the feature boundary proves
insufficient.

### `celox-testbench`

Owns language-independent testbench bytecode, values, formatting, and VM execution. The Veryl
frontend emits this bytecode. The VM accesses signals through a small semantic `TestbenchIo` trait;
`celox-runtime` implements that trait using the physical layout. This keeps testbench bytecode and
the frontend independent of `celox-state-layout`. Runtime executes it without retaining Veryl
statements or functions.

### `celox-runtime`

Owns executable simulation behavior after compilation:

- backend/runtime traits and event handles;
- simulation time scheduler and multi-phase/cascade execution;
- runtime event buffers, signal access, and memory ownership;
- VCD integration and runtime errors;
- `ExecutableProgram` assembled from backend artifacts and runtime metadata.

It depends on design/layout/testbench contracts, not on frontend, SLT, SIR optimization, or concrete
backend crates. Concrete backends implement runtime traits and are selected by the facade.

### `celox` facade

Remains the user-facing crate and compiler orchestrator:

- `SimulatorBuilder`, compile pipeline assembly, backend selection, and trace aggregation;
- stable public options assembled from phase-specific option types;
- compatibility re-exports for existing public APIs;
- default-backend selection by target.

It contains no optimizer implementation, machine IR, register allocator, parser internals, or
runtime VM implementation after migration.

## Dependency graph

The intended direct dependencies are listed below. An arrow means "depends on".

```text
celox-analysis             -> (none)
celox-design               -> (none of the compiler crates)
celox-sir                  -> celox-design, celox-analysis
celox-slt                  -> celox-design, celox-sir, celox-analysis
celox-state-layout         -> celox-design, celox-sir
celox-sir-opt              -> celox-design, celox-sir,
                              celox-analysis, celox-state-layout
celox-testbench            -> celox-design
celox-runtime              -> celox-design, celox-state-layout, celox-testbench
celox-frontend-veryl       -> celox-design, celox-slt, celox-testbench
celox-backend-x86          -> celox-design, celox-sir,
                              celox-state-layout, celox-runtime
celox-backend-cranelift    -> celox-design, celox-sir,
                              celox-state-layout, celox-runtime
celox-backend-wasm         -> celox-design, celox-sir,
                              celox-state-layout, celox-runtime
celox facade               -> frontend, SLT, SIR optimization, layout,
                              selected backends, testbench, runtime
```

These are allowed dependencies, not a requirement that every crate import every listed crate. In
particular:

- backends consume finalized SIR/layout/runtime contracts but do not import `celox-sir-opt`;
- runtime does not import concrete backends;
- frontend does not import optimizers or concrete backends;
- `celox-analysis` imports none of the other crates.

Cargo features may remove optional dependencies; they must not reverse these edges.

## Phase artifact contracts

### `ElaboratedDesign`

Contains hierarchy, semantic state objects, clocks/resets, initial values, runtime event schema, and
source maps required for diagnostics. It contains no SLT/SIR/layout/backend plan.

### `SymbolicRtl`

Contains SLT arena roots, logic paths, FF recipes, observer recipes, and symbolic stores keyed by
design IDs. Every `NodeId` refers to the arena owned by this artifact.

### `ScheduledRtl`

Contains the selected comb/FF order, SCC execution policy, semantic regions, and lowering recipes.
Scheduling legality is complete at this boundary. SIR lowering may choose instruction details but
may not silently change RTL ordering.

### `SirProgram`

Contains named groups of SIR execution units and runtime-event references. It has no SLT arena,
`NodeId`, Veryl AST, physical offset, or backend plan.

### `OptimizedSir`

Contains verified optimized SIR plus semantic alias proofs and `LayoutRequirements`. Its constructor
is private to the optimizer pipeline so callers cannot label unverified SIR as optimized.

### `LaidOutProgram`

Contains a verified `SirProgram`, immutable `MemoryLayout`, design/runtime schema, and initial
physical-memory image. All semantic objects referenced by SIR resolve through the layout. No
backend has run yet.

This type is owned by `celox-state-layout`, which must not depend on `celox-sir-opt`. Layout
construction consumes `OptimizedSir` through an `into_verified_sir()` boundary plus explicit
`LayoutRequirements`; it stores the underlying SIR rather than the optimizer's newtype. A
layout-preserving finalizer in `celox-sir-opt` may consume and return `LaidOutProgram`, but layout
construction never calls back into the optimizer.

### `BackendArtifact`

Contains backend-owned compiled functions/code and an implementation of the runtime execution ABI.
MIR, register assignments, Cranelift plans, and relocation tables remain private to the producing
backend unless tracing explicitly requests a serialized diagnostic.

### `ExecutableProgram`

Contains backend artifacts, initialized state memory, event handles, signal lookup tables, and
optional testbench bytecode. It has no compiler IR.

## Dissolving the current `Program`

| Current field group | Destination |
| --- | --- |
| `eval_comb`, `eval_*_ffs`, `eval_comb_apply_ffs` | `SirProgram` |
| `comb_semantic_regions` | `ScheduledRtl`, then explicit SIR provenance if still needed |
| `arena`, `comb_observers` containing `NodeId` | `SymbolicRtl`; consumed before `SirProgram` |
| hierarchy/module/variable/clock/reset maps | `ElaboratedDesign` |
| runtime errors and event sites | design/runtime schema |
| `address_aliases` | `LayoutRequirements` with proof identity |
| `layout: Option<MemoryLayout>` | separate `LaidOutProgram` |
| `eval_comb_plan` | concrete backend planning result |
| initial memory values | design initial state, then laid-out memory image |
| Veryl `initial_statements` and `tb_functions` | compiled by frontend into `celox-testbench` bytecode |

No replacement structure may simply contain all of these fields under another name.

## Option ownership

The current `OptimizeOptions` mixes SIR passes, Cranelift settings, layout choices, and simulator
policy. It is split into:

- `SirOptimizeOptions` in `celox-sir-opt`;
- `LayoutOptions` in `celox-state-layout`;
- `X86BackendOptions` in `celox-backend-x86`;
- `CraneliftOptions` in `celox-backend-cranelift`;
- `WasmBackendOptions` in `celox-backend-wasm`;
- runtime policy in `celox-runtime`;
- user-facing `CompileOptions`/`SimulatorOptions` in the facade, which translate to the above.

An option owned by one backend cannot change the semantics or optimization pipeline seen by another
backend.

## Source relocation map

This is the intended ownership, not a command to move whole files unchanged.

| Current area | Target |
| --- | --- |
| `ir::{builder,cfg,verify}` and generic SIR types in `ir.rs` | `celox-sir` |
| address/hierarchy/domain/variable metadata in `ir.rs` | `celox-design` |
| `logic_tree` node/facts/path/state/range/lower core | `celox-slt` |
| Veryl expression traversal currently under `logic_tree::comb` | `celox-frontend-veryl` |
| `parser`, `flatting`, context-width and Veryl bit-access handling | `celox-frontend-veryl` |
| `parser::scheduler` generic dependency scheduling | `celox-slt` after parser types are removed |
| `optimizer` and backend-independent `optimizer::coalescing` passes | `celox-sir-opt` |
| `backend::memory_layout` | `celox-state-layout` |
| `backend::native` | `celox-backend-x86` |
| Cranelift translator/JIT code | `celox-backend-cranelift` |
| `wasm_codegen` and optional host WASM runtime | `celox-backend-wasm` |
| `testbench` VM | `celox-testbench` |
| `simulation`, runtime scheduler, event buffer, backend traits | `celox-runtime` |
| `simulator::builder`, backend choice, compilation trace assembly | `celox` facade |

Small helpers move to their semantic owner. There will be no `celox-utils`, `celox-core`, or
`celox-ir` dumping-ground crate.

## Migration plan and gates

Each milestone is independently reviewable and leaves the workspace buildable. Pure moves must not
change snapshots or generated code.

### Milestone 0: design contract

- Commit this document alone.
- Record the branch dependency on PR #322.
- Make no source change.

Gate:

- Markdown formatting and link inspection;
- clean diff containing only this document.

### Milestone 1: extract the SIR kernel

- Add `celox-sir`.
- Move generic SIR types, builder, verifier, CFG adapter, display, and merge utilities.
- Keep concrete design addresses and the mixed `Program` temporarily in `celox`.
- Re-export existing public SIR-facing names from `celox` where compatibility requires it.
- Move generic CFG ordering to `celox-analysis` instead of making SIR depend on the facade.

Gate:

- `cargo test -p celox-sir`;
- all existing SIR verifier/builder/serialization tests;
- `cargo test -p celox`;
- unchanged optimized-SIR snapshots;
- host `cargo clippy` and format checks.

### Milestone 2: introduce design-owned identities

- Add `celox-design`.
- Introduce Celox-owned dense IDs and source-to-design conversion maps.
- Move bit ranges, semantic state references, operators, hierarchy, domains, and initial-state schema.
- Keep Veryl IDs behind frontend conversion tables.
- Adapt SIR to `StateRef` without changing physical layout.

Gate:

- hierarchy, parameter override, initial state, multi-clock, four-state, and serialization tests;
- SIR snapshots differ only in intentionally renamed address formatting;
- native/Cranelift/WASM cross-validation corpus.

### Milestone 3: split phase artifacts and layout

- Replace mutable mixed `Program` use with `ElaboratedDesign`, `SirProgram`, `OptimizedSir`, and
  `LaidOutProgram` at compiler-driver boundaries.
- Add `celox-state-layout` and move layout construction/verification.
- Replace optimizer enums inspected by layout with explicit `LayoutRequirements`.
- Remove `Program::build_layout*` and `Option<MemoryLayout>`.

Gate:

- memory-layout unit tests;
- packed/unpacked boundary and alias tests;
- initial-memory and runtime-event layout tests;
- all three backend correctness tests;
- no source-language dependency in `celox-state-layout`.

### Milestone 4: separate SLT core from Veryl construction

- Add `celox-slt` and `celox-frontend-veryl`.
- Move source-independent arena, facts, symbolic state, scheduling, and lowerer into `celox-slt`.
- Keep Veryl AST traversal, context widths, case construction, and diagnostics in the frontend.
- Make comb and FF dependencies inputs to one scheduling/lowering contract; do not recreate a
  post-hoc concatenation path.
- Consume `SymbolicRtl` when producing `SirProgram`, proving no `NodeId` reaches later phases.

Gate:

- SLT node-fact/verifier tests;
- comb-loop/SCC and fused comb/FF scheduler tests;
- FF ordering, NBA, multi-clock, and observer semantics;
- Heliodor SIR shape and native execution correctness;
- no Veryl dependency in `celox-slt`.

### Milestone 5: extract SIR optimization

- Add `celox-sir-opt`.
- Move pass manager and backend-independent passes.
- Remove optimizer calls into parser modules.
- Split pre-layout and layout-preserving finalization contracts.
- Split `OptimizeOptions` by owner.

Gate:

- every pass unit test and snapshot;
- pass-by-pass verifier execution in tests;
- optimized SIR and generated MIR comparison for representative designs;
- compile-time/RSS scaling tests for large SIR;
- Heliodor correctness and non-LTO development performance check.

### Milestone 6: extract concrete backends

- Add `celox-backend-x86`, then Cranelift and WASM backend crates.
- Move x86 MIR/regalloc/emitter together; do not expose MIR as generic compiler API.
- Make backend inputs immutable `LaidOutProgram` views.
- Keep backend traces diagnostic-only.

Gate:

- backend unit and cross-validation tests after each backend move;
- Windows x86-64 check for the x86 backend;
- Linux x86-64 native tests;
- Linux AArch64 GNU NAPI build through Cranelift;
- browser and wasmtime WASM tests;
- unchanged native MIR/disassembly snapshots for pure moves.

### Milestone 7: extract testbench and runtime

- Add `celox-testbench` and compile Veryl testbench constructs before runtime.
- Add `celox-runtime`; move scheduler, simulation state, event handles, VCD, and backend traits.
- Leave orchestration and stable API re-exports in `celox`.
- Delete obsolete compatibility modules after downstream crates have migrated.

Gate:

- native testbench, formatting, runtime-event, VCD, and scheduler tests;
- NAPI and WASM package builds;
- public API compatibility tests;
- full workspace pre-push suite.

### Milestone 8: cleanup and final acceptance

- Delete dead modules, transitional type aliases, duplicate adapters, and unused feature paths.
- Verify the Cargo dependency graph against this document.
- Update architecture and IR reference documentation.
- Re-establish compile-time and runtime baselines.

Gate:

- clean workspace build on supported targets;
- complete semantic suite;
- final release/LTO Heliodor compilation and execution benchmark;
- generated-code comparison against the pre-split baseline;
- no unexplained regression accepted as a consequence of crate separation.

## Per-commit verification policy

Every implementation commit runs the smallest complete gate for the boundary being changed. Before
moving to the next milestone, the enclosing crate and `celox` integration tests both pass.

The normal iteration profile is the development profile. Release/LTO is used at performance
milestones and final acceptance, not on every mechanical move. Cross-target checks run when a moved
boundary contains target configuration or public API, rather than being postponed until all crates
have moved.

If a move changes generated SIR, MIR, machine code, RTL behavior, compile time, or runtime, that
change is treated as a functional change and explained separately. A crate extraction is not an
acceptable reason for an unexplained change.

## Completion criteria

The split is complete when:

- Cargo exposes a dependency DAG matching the ownership rules above;
- no type equivalent to the current mixed `Program` exists;
- SIR contains no SLT/Veryl/layout/backend-plan fields;
- backend crates consume immutable design/SIR/layout contracts;
- runtime contains no compiler or source-language IR;
- the x86 allocator and MIR remain fully encapsulated by `celox-backend-x86`;
- the `celox` facade preserves the intended public API without containing compiler implementations;
- semantic tests and final release/LTO performance gates pass.

The objective is not a larger number of crates. The objective is to make illegal phase coupling
unrepresentable while preserving the compiler's ability to generate fast simulation binaries.
