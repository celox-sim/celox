# hwverify-sir: symbolic one-event SIR lifting

This reusable Rust library lifts the **compile-only** Veryl/Celox export into
`hwverify_ir::Term` expressions. It neither runs a simulator nor asks a solver
for a representative input, branch, address or state. The existing finite solver
and refinement checker prove the resulting expressions separately.

```sh
cargo build --locked -p hwverify-sir
# Compile a {top,sources,four_state:false} document with the pinned frontend.
conformance/veryl-proof/target/debug/veryl-proof-frontend design.json > compiled.json
target/debug/hwverify-sir-lift compiled.json bindings.json > transition.json
```

Bindings name a selected event and explicitly map physical signals to typed
canonical inputs/state. For example:

```json
{
  "event": "clk",
  "inputs": {
    "rst_n": {"name": "rst", "type": "bool", "expr": ["not", "i.rst"]},
    "enable": {"name": "en", "type": "bool"},
    "data": {"name": "a", "type": {"bv": 32}}
  },
  "state": {"q": {"type": {"bv": 32}}},
  "outputs": {"current_q": {"signal": "q", "type": {"bv": 32}}}
}
```

An entry can use `signal` and `element` to select one flattened array element,
for example `"rom0":{"signal":"rom","element":0,"type":{"bv":37}}`.
Only unambiguous top-level source paths are accepted. Flattened internal
hierarchy can participate in SIR execution, but hierarchical API bindings are
not currently provided. Canonical names must be unique within inputs/state;
physical storage lanes cannot be bound twice or as both input and state. State
bindings are direct arbitrary symbols, never expressions or zero-filled values.

`lift(&compiled, &bindings)` returns `Transition { next, outputs, statistics }`.
`to_json(false)` exports canonical `next`/`outputs` plus shared `wires`. Install
all three into a validated hwverify implementation model. `to_json(true)` (CLI
`--inline`) emits expressions without wires, useful for reset equations.
The result says **lifted, not verified**: its existence is not a proof.

## Sampling and reset

- Inputs and selected state are arbitrary symbolic values at the beginning
- `eval_comb` executes first; `outputs` observe this **pre-edge** state, so a
  pipeline commit refers to the old retiring stage, not the new stage contents
- Exactly one selected `eval_apply_ffs` group executes in scheduled unit order;
  `next` observes the resulting selected state, without a second combinational
  evaluation. Bind sequential storage, not post-edge combinational outputs, as
  state
- Other named events are not implicitly fired. Aliases are resolved, with
  duplicate/cyclic aliases rejected. Cascaded events and triggered stores or
  commits are rejected; multi-clock waveform timing is not claimed
- `overrides` replaces canonical input symbols, e.g. `{"overrides":{"rst":true}}`.
  Reset lifting **still begins with arbitrary prestate**. The caller must reject
  prestate-dependent reset equations or prove their independence before using
  a format which represents reset solely as a function of inputs
- Source initializers are not assumptions of an arbitrary-state transition.
  Selected state stays arbitrary; unbound storage stays undefined until written.
  Unknown/masked initializers are rejected. This API does not prove power-on
  initialization or source reset completeness by itself

## Guarded state and memory semantics

A reachable acyclic control-flow graph is processed in topological order. Both
symbolic branch/switch outcomes contribute guarded frames. Join parameters and
live registers are merged with `ite`, as are storage contents and write masks.
No branch is chosen using a SAT model. A missing store preserves its incoming
value. Read-before-definition remains an error rather than becoming zero.

Region-1 snapshots and commits preserve scheduled nonblocking assignment reads.
Region-2 sparse NBA storage additionally tracks a per-bit write mask: only
written bits commit, untouched bits hold, and later aliasing writes win. The
snapshot source is immutable while a commit is copied, including partial lanes.

Static, dynamic bit and element addresses are expressed as mux/guarded writes.
Every in-range candidate participates; out-of-range reads return zero and writes
leave storage unchanged, matching the pinned two-state Backend storage contract.
Element-plus-dynamic-bit offsets include bounds before arithmetic, preventing
64-bit overflow from aliasing a huge index into a valid address. No address
uniqueness assumption is introduced. Dynamic arrays expand finitely; they are
not an unbounded-memory abstraction.

## Supported boundary and failure modes

The frontend must explicitly export `four_state:false`. Values in this route
are two-state; a source `logic` signal is restricted to its 0/1 domain, not
claimed to model X/Z. Nonzero immediate masks/unknown initialization fail closed.
The separate `conformance/veryl-proof` four-state corpus route is unchanged.

Supported instructions are typed immediates, selected unary reductions and
casts, Boolean/bitwise/arithmetic operations, comparisons, arbitrary-count
shifts, loads/stores/commits, concat/slice/mux, jump/branch/switch and return.
Comparison operands have equal widths; `Minus`/`BitNot` source/result widths must
match the pinned SIR verifier. SIR source context conversions are already
separate operations. Arithmetic promotes the signed left operand and unsigned
right operand; logical right shift always zero-fills.

Current limits: scalar registers/storage lanes **1..64 bits**, aggregate storage
object **1..65536 bits**, at most **10000 CFG blocks**, **100000 registers** and
**100000 instructions per block**. Finite solver limits apply separately.
Division/remainder, bit-count operations, runtime diagnostics/assertions,
cyclic/dynamic CFG loops, source four-state behavior, partial sparse commits,
unsupported region layouts and ambiguous bindings return errors. Malformed
instruction arities, missing targets, bad widths and overlapping bindings are
rejected before symbolic execution. No external solver fallback exists here.

The crate tests prove general small SIR equations for arbitrary data/control,
branch holds, CFG parameters, sparse NBA aliasing, dynamic/OOB addresses,
partial cross-lane writes, overflow guards and shifts. They also check genuine
counterexample feasibility, reset-prestate preservation and rejected inputs.
These are semantic unit checks, not a replacement for compiled Veryl examples.
See `conformance/veryl-symbolic` for the independently written Veryl CPU versus
sequential ISA inductive refinement, actual HDL mutations and non-vacuity.

### Scaling corrections

Flattened metadata.width is the total across unpacked dimensions; the binding
lane width is total/product(array_dims). A real exported array fixture guards
this convention. Whole-word sparse NBA writes additionally retain guarded update
DAGs until commit, including last-write-wins and branch joins. This avoids
unnecessary bit-mask expansion. Any partial write drops the optional fast path;
the exact mask representation remains authoritative as a fallback. The fast path
is checked against those mask equations at widths2–64.
