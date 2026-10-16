# Shared-port composition specifications with positive and negative examples (0.8 / JSON version 3)

0.9 added input quantification, execution modes, `ensure` and short trace calls to
version 3 as well. See [expectations and traces](expectations.md) for their new
semantics and limits. The positive/negative examples on this page keep the
original existence conditions.

A `.lyd` file with a `specification "name"` header gathers relationally written
specification components, their composition, and the finite behaviors to allow
or reject into one document. The header takes no semicolon, and no outer braces
surround the declarations that follow it.
[budgeted_counter.lyd](../examples/budgeted_counter.lyd) separates "counter
arithmetic" and "available budget" into components that share inputs and outputs.
Existing `design` / version-2 JSON can be used unchanged.
This page describes version 3, with shared global ports and synchronized
same-name operations. For local ports, explicit wiring, per-instance state
separation and independent operation sets, see the new version 4
[scoped specifications](scoped-specifications.md).

```
cargo run -- examples/budgeted_counter.lyd --check
cargo run -- examples/budgeted_counter.lyd --out /tmp/budgeted --z3 /path/to/z3
cargo run -- examples/contradictory_composition.lyd --out /tmp/contradiction --z3 /path/to/z3
```

The last example fails deliberately: a behavior that a component allows on its
own is not necessarily allowed after composition. `weakened_budget.lyd` is also a
deliberate failure; it detects an overly weakened specification that allows a
negative example.

## Component semantics

The specification as a whole declares `input`, `observation` and named
`operation` items. Each `component` has its own `state` declarations, an initial
condition `init`, an `invariant` for every state, and per-operation relations
`steps`.

```
specification "Counter"

input amount: bv<4>;
observation count: bv<4>;
operation add {}

component Counter {
  state value: bv<4>;
  init s.value == 0u4;
  invariant o.count == s.value;
  steps { add = n.value == s.value + i.amount; }
  // example declarations can follow here
}
```

`o.count` and `i.amount` refer to the document's observation / input
declarations. Widths, arithmetic and operator semantics are the same as in the
[language guide](language.md).

- `s.x`: the component's current private state
- `n.x`: the component's next private state
- `o.x`: the current shared observation
- `no.x`: the next shared observation
- `i.x`: the shared input consumed by this operation

init / invariant refer only to s and o. steps defines every named operation
explicitly; `true` leaves that operation unconstrained. To hold state, write it
explicitly, as in `n.x == s.x`. Components that use the same private field name
still have separate state.

## Composition

```
composition Arithmetic { members compose(Counter); }
composition Budgeted { members compose(Arithmetic, Budget); }
```

Named compositions can themselves be composed. The relations of the selected
components are conjoined over the same observations and inputs. Success is never
obtained by taking one witness under which an example passes for one component
and another witness under which it passes for a different component. One
consistent execution over the shared values is required.

For each trace operation name, all members satisfy that operation's relation
simultaneously. There is no implicit asynchronous execution, interleaving or
arbitrary reordering. Members have separate private state and share only
observations and inputs. Port renaming, wiring aliases and parameterized
instantiation are not part of this version.

Unknown members, cycles, empty compositions and reaching the same component more
than once are errors. For example, if P contains A, `compose(P, A)` is rejected.
Define a component under another name when an independent copy is needed. The
composition cycle check is a separate feature from a future cycle check over
proof assumptions.

## Positive and negative examples

List named `example` items directly inside each component / composition.

```
example add_two {
  expect positive;
  initial { count = 0u4; }
  trace {
    add { inputs { amount = 2u4; } observe { count = 2u4; } }
  }
}
example wrong_result {
  expect negative;
  initial { count = 0u4; }
  trace {
    add { inputs { amount = 2u4; } observe { count = 3u4; } }
  }
}
```

`operation add {}` declares the operation at the document root. Inside a trace,
`add { ... }` is written as before and denotes performing the operation once.

- positive: requires that **at least one** specification execution exists that
  matches the given observations and inputs
- negative: requires that **no** such execution exists, however private state and
  omitted values are filled in

initial is the frame 0 observation; init and invariant are imposed at frame 0.
Trace step k uses inputs k and relates frames k and k+1. `observe` constrains the
post-operation frame k+1, and invariant is imposed on every frame. No reset
operation is inserted implicitly; the initial predicate determines the example's
starting point.

Inputs and observations may be partially specified. Omitted values are
existentially quantified, not zero. Omitting a trace step's inputs / observe
block gives an empty partial specification. Private state values cannot be
specified directly in an example. Writing the same operation several times keeps
each occurrence as a separate step in order. An empty trace is also allowed.
An example is limited to 4096 steps, but this is not a strict time or memory
limit.

Each example produces a single SAT problem. On SAT, the concrete complete states,
observations and inputs are retrieved and SAT is confirmed again. UNSAT means
every hidden completion has been excluded. UNKNOWN, solver errors and failed
witness re-retrieval are never treated as example success.

Positive examples alone miss an overly weak specification, and negative examples
alone miss a contradictory specification that rejects everything. Supply both,
and in particular include behaviors that must still be allowed after composition.
Passing examples do not prove arbitrary-length executions, specification
completeness, deadlock freedom or liveness. The existence of a given finite
prefix does not mean it can be extended arbitrarily.
The report's coverage records positive/negative example counts per component and
composition, with warnings for a missing polarity. For a target with only
negative examples, passing them does not show that any allowed behavior exists.

## Restricted binding to an implementation

Any composition can be bound to an `implementation` in the same document. The
implementation has state/reset/next and Boolean expressions that select
operations. Every private state field and shared observation is given a mapping
that uses only the implementation's current state.

```
implementation {
  composition Budgeted;
  reset_input rst;
  state count: bv<4>;
  state remaining: bv<4>;
  reset { count = 0u4; remaining = 3u4; }
  wires { accept = i.go && i.amount <= s.remaining; }
  next {
    count = if w.accept { s.count + i.amount } else { s.count };
    remaining = if w.accept { s.remaining - i.amount } else { s.remaining };
  }
  operations { add = w.accept; }
  binding {
    bind Counter { value = s.count; }
    bind Budget { credits = s.remaining; }
    observation count = s.count;
    observation remaining = s.remaining;
  }
}
```

Declare `input rst: bool;` and `input go: bool;` as shared inputs.
`bind Counter { ... }` is a mapping to an existing component's state, not a new
state declaration. `observation count = ...;` inside binding is a mapping to a
shared observation, while the root `observation count: bv<4>;` declares its type.
Reset has top priority and returns to the abstract initial state.
Missing or extra binding fields and operation selectors are checked, and
input-dependent abstractions are rejected. The proved obligations are:

1. reset establishes the composed specification's init / invariant
2. outside reset, at most one operation is selected
3. for the selected operation, the mapped step satisfies every component's
   relation and the next-state invariant
4. on a step that selects no operation, all mapped private state and
   observations are held

This is safety / stuttering refinement under the supplied state mapping.
Reachability is not narrowed; the check covers every implementation state that
satisfies the mapped invariant. It does not require that every behavior of the
relational specification be implementable, or that the abstraction be surjective.
An implementation whose selectors are always false and that holds its state can
satisfy safety. Progress, fairness and termination need separate treatment, and
passing positive spec examples does not guarantee implementation progress either.

## Results and evidence

The main results are `spec_examples_passed`, `spec_examples_failed`, `unknown`
and `no_examples`. With a binding, `spec_examples_and_binding_verified`,
`implementation_binding_failed` and `binding_verified_no_examples` are also used.
Validation-only remains `validated` as before. The report keeps example outcomes
and the `implementation_binding` proof result separately inspectable.

The original `.smt2`, the solver `.out` and the complete witness context mapping
are saved. The Rust checks, translation and kernel, and Z3, are trusted. The
whole toolchain is not formally proved sound.
Provisional proofs, holes, conditional theorems and a proof dependency DAG are
not implemented yet.

## 0.8 notation and compatibility

The old `specification "name" { ... }`, `operations { add {} }`,
`components { Counter { ... } }`, `compositions { Budgeted { ... } }`, and
`examples { overspend { ... } }` inside components and compositions are accepted
as compatibility notation. The old type-declaration forms `inputs { ... }` /
`observations { ... }` / `state { ... }`, and the old binding forms
`states { Counter { ... } }` / `observations { count = ...; }`, also work.
Individual declarations and old containers lower to the same JSON map, and
duplicate names across forms are rejected.

`input` / `observation` / `operation` / `component` / `composition` / `example` /
`state` / `bind` and similar words are interpreted by context. No new global
reserved words are added. `steps { add = ...; }`, the implementation's
`operations { add = ...; }`, the trace's `inputs { amount = ...; }` /
`observe { count = ...; }`, and assignment blocks such as `reset` / `next` /
`wires` are unchanged.

In the DSL, empty containers for sets with no declarations need not be written.
Omitting root inputs, observations, operations, components or compositions,
component/implementation state, component/composition examples, or binding state
and observation mappings fills the corresponding JSON map with `{}`. JSON input
must state required keys and empty objects explicitly. This changes only the
notation for empty sets; it does not skip semantic checks. For example, at least
one operation and one component are still required, and a composition's
`members` cannot be empty. Required fields and blocks such as `init` /
`invariant` / `steps` and an example's `expect` / `initial` / `trace` are still
required as before.

```sh
python scripts/json_to_lyd.py examples/budgeted_counter.json /tmp/budgeted_counter.lyd
cargo run --locked -- /tmp/budgeted_counter.lyd --emit-json /tmp/budgeted_counter.json
```

The printer converts version 2 / 3 / 4 JSON to the corresponding notation,
preserving expression trees and trace order. Documents on this page remain
version 3 and are not converted to version 4 automatically. Migrated documents
must still pass the complete Rust checks. The JSON schema version is separate
from the toolchain version number.

## Version-3 JSON structure

A complete input example is [budgeted_counter.json](../examples/budgeted_counter.json).
The array representation of types and expressions is shared with version 2; for
example, `0u4` is `["bv",4,0]` and `s.x == o.count` is `["eq","s.x","o.count"]`.

Required top-level fields:

- `version`: 3
- `kind`: `"specification"`
- `inputs`, `observations`: object of name → type
- `operations`: object of operation name → empty object (e.g. `{"add":{}}`)
- `components`: object of name → component
- `compositions`: object of name → composition (may be empty)

Optional fields are `name` and `implementation`.
Required component fields are `state`, `init`, `invariant`, `steps` and `examples`.
steps maps every operation name → Boolean expression, and examples is an object
of example name → example. A composition has `members` (an array of
component/composition names) and `examples`.

An example has `expect`, `initial` (observation name → expression) and `trace`
(an ordered array of steps). The old `expect` values are `"positive"` /
`"negative"`; the explicit modes of 0.9 are `"exists"` / `"not_exists"` /
`"forall"`. Explicit modes may specify an optional ordered `quantifiers` array,
whose variables are referenced as `q.name` in JSON. In ordinary `.lyd` source the
declared name is written as plain `name`; the old `q.name` is also accepted as
compatibility notation. Old modes take no `quantifiers`. Every step has
`operation` (the operation name), `inputs` and `observe`, and may add an
optional Boolean `ensure`. inputs / observe are objects of name → expression.
Without bound variables they are closed expressions as before. ensure refers
only to post-operation `o.` observations and bound variables.
In JSON, write `{}` explicitly even when there is no partial specification. In
the DSL, the frontend fills in `{}` when a trace step omits inputs / observe.
An example's `initial` and `trace` blocks are explicit even when empty.

Required implementation fields are `composition`, `reset_input`, `state`, `reset`,
`next`, `operations` and `binding`. `wires` is optional.
operations maps operation name → Boolean selector expression, and binding has
`states` and `observations`. states maps component name → (private field name →
state-only expression), and observations maps observation name → state-only
expression. Every target and type is checked.

## Conditional bounded response

Safety/stuttering alone permits an implementation to select no operation forever.
An optional `implementation.responses` contract adds an inductive bounded-response
check. The same contract is available in v3 and v4. Complete runnable examples are
[response.lyd](../examples/response.lyd) and
[scoped_response.lyd](../examples/scoped_response.lyd).

```lyd
responses {
  request_done {
    operation advance;
    accept w.accept;
    pending s.busy;
    rank s.ticks;
    bound 2;
    cover_depth 2;
    assume !i.stall;
  }
}
```

`accept` is the implementation's explicit acceptance handshake, not merely an
external request signal. Completion is the selected operation's existing Boolean
selector on the current implementation edge; its effects must still satisfy the
ordinary relational safety binding. Acceptance may complete on the same edge.
Otherwise `pending` must become true in the next state, and completion must occur
within `bound` subsequent nonreset edges. The example accepts at edge 0, waits at
edge 1 and completes at edge 2, incrementing its abstract counter exactly once.

The initial interface supports exactly **one contract per implementation and one
outstanding request**. Accepting while pending fails a checked obligation, even
if the same edge completes the old request: same-edge queue replacement is not
supported. Requests presented while busy may remain unaccepted; eventual
acceptance is not promised. Unsolicited completion is rejected. `pending` cannot
be silently dropped or cleared before completion. Reset has priority, cancels
the obligation and requires the reset state to have `pending == false`; acceptance
and completion on a reset edge do not create or discharge a nonreset obligation.

`pending` and `rank` are state-only expressions over `s.*`. `rank` must be an
unsigned word of at most 64 bits; `bound` must be a positive integer representable
in that width. No wraparound, saturation, ghost state, implicit queue or temporal
syntax is added. The author supplies the real implementation-state witness.
Unsupported fields, input-dependent ranks/pending predicates, multiple contracts,
unknown completion operations and invalid bounds are rejected during validation.

`assume` is a Boolean expression over **inputs only**, using `i.*`; implementation
state and wires are excluded so it cannot hide a stuck implementation state.
It must hold at acceptance and every subsequent nonreset edge until completion.
The example excludes stalls explicitly. Changing its assumption to `true` fails:
an indefinitely stalled implementation does not meet a physical-step deadline.
There is no implicit fairness or removal of stalled ticks from the count. After
an assumption violation the old request has no response guarantee. Independently
of response assumptions, the original safety obligations remain unconditional.

For pending predicate P, acceptance A, completion C, rank R and bound B, generated
checks establish reset cancellation, no overlapping A/P, and, on an assumed
nonreset step, `P_next == (P || A) && !C`. They establish `P_next => R_next < B` and
`P && !C => R_next < R`. The latter uses the same unsigned-rank comparison as v2
progress. The invariant `P => R < B` is established at acceptance and preserved;
strict decrease prohibits a non-completing step at rank zero. Thus the bound
holds for each accepted request by induction, not by enumerating a finite trace
and extrapolating it into unbounded liveness. Other operations receive no progress
claim, and request/response payload correspondence is limited to the separate
relational safety specification.

**A verified conditional response does not establish that an external request is
accepted, or that the implementation ever does useful work after reset.** `accept`
is an implementation-defined event, not an automatically enforced request
handshake. There is no separate external request predicate or request-to-acceptance
obligation in this version. A model can add `ever_enabled`, reset it to false,
preserve it forever, and require it in `accept`. Acceptance can then be feasible
only in unreachable states, while every reset-reachable run remains idle and the
conditional response checks pass. The regression suite preserves this example.

Optional `cover_depth N` (1–32) requests a **separate bounded acceptance cover**.
Edge 0 applies the original reset assignment with reset high. Edges 1 through N
are candidate acceptance edges, each with reset low and the declared input
assumption satisfied. The reset input vector is independent of subsequent input
vectors, and the assumption is not imposed on reset. There are no constraints on
inputs after the accepted edge. Depth counts nonreset implementation edges after
reset, independently of the response latency `bound`.

`adequacy.reset_acceptance_cover` reports the depth, limits, source location and:

- `reached`: a SAT prefix independently replayed through the original reset and
  next-state expressions. `witness.trace` contains concrete inputs and before/after
  states, ending at `witness.acceptance_edge`; `original_transitions_validated`
  is true. This proves existence of one accepting run, not service for all requests.
- `not_reached_within_bound`: UNSAT for this bounded cover only. It does **not** mean
  acceptance is unreachable at greater depth. The `ever_enabled` mutant receives
  this result while its conditional response theorem still verifies.
- `unknown`: budget exhaustion, unsupported scalar replay, or failed witness
  validation; no replay-validated witness is returned.
- `unchecked`: no `cover_depth` was supplied.

The cover uses the existing finite solver and word-level interpreter, not an
external solver or finite traces extrapolated into liveness. Initial replay
support is Bool/BV state and inputs of width at most 64, with at most 4096 frame
symbols; reported finite solver budgets also apply. It adds no mapped-invariant,
pending or rank assumptions to the original transition system. Its status never
changes the conditional proof status or CLI verification exit status.

`adequacy.reset_reachable_acceptance` mirrors the cover status;
`adequacy.external_request_to_acceptance` remains `"not_specified"`. The LSP shows
cover outcomes at `cover_depth` and retains a warning at `accept` about the missing
external request-to-acceptance obligation, including when the cover is reached.

Separate SAT checks require a feasible nonreset input assumption and a feasible
acceptance under the mapped invariant. Contradictory assumptions or `accept false`
produce `failed_nonvacuity`, not success. These witnesses are **not claims of reset
reachability**, general specification adequacy or deadlock freedom. Unknown in any
required obligation remains Unknown. A verified response requires all generated
obligations and the existing implementation binding to pass.

```sh
cargo build --release --locked
LYDITE_SOLVER=finite ../target/release/lydite examples/scoped_response.lyd --out /tmp/fresh-response
cargo test --release --locked -p lydite --test responses
```

Read `implementation_binding.responses` and the `response_*` obligations in the
report. Native reports attach `source_location` to each response obligation;
finite counterexample files retain current/next implementation and response
aliases. In the editor, **Check implementation responses and safety** explicitly
runs the same checker; edit-time analysis only validates syntax, names and types.
The editor action also checks the document's ordinary examples and safety binding,
so a passing response row alone does not override other failures in that result.

Failed or Unknown implementation bindings also produce a blocking implementation-level
editor diagnostic naming the outstanding obligations, even when every response
obligation passes.

## Reset-reachable failures and source RTL replay

`lydite-replay` adds bounded **failure** search and strict concrete replay for
native v3 relational implementations. It is separate from both inductive checking
and the positive acceptance cover above. Initial scope is scalar Bool/BV≤64,
one synchronous positive-edge clock, canonical reset at edge 0 followed by
1–32 nonreset edges, and direct top-level state/input bindings. Native v4, arrays,
multiple clocks, asynchronous/typed reset ports, and four-state replay are not
supported by this first source-replay route; existing checkers are unchanged.

Build with `cargo build --release --locked -p lydite`. The executable accepts
one JSON request on stdin; use `source` for native v3 text or `document` for its
canonical JSON. For example, a request has `version: 1`, `mode: "search"`,
`goal: "safety"`, `depth: 4`, and `source: "..."`. Goals are:

- `safety`: the original reset, operation-exclusion, relational-step, invariant,
  and stutter failure predicates shared with the inductive v3 binding checker.
  Response input assumptions do not restrict this safety search.
- `response_deadline`: a separate monitor for the sole declared response. It
  starts on qualifying acceptance, permits same-edge completion, and fails if
  completion is absent through the Bth subsequent nonreset edge. It maintains
  its own outstanding-request bit and age, never trusting DUT `pending` or rank.
  An assumption violation cancels the affected interval's guarantee, not later
  qualifying requests. The monitor does not add request-to-acceptance, payload,
  overlap, or unsolicited-completion guarantees; those are separate properties.

Search statuses are `reset_reachable_failure`, `bounded_no_failure`, and `unknown`.
Only the first carries an original-transition-and-property-validated witness.
`bounded_no_failure` means no violation of the selected goal within the bound,
not an unbounded proof. SAT states are compared against a fresh execution of the
original reset/next expressions; supplied intermediate states are never inputs.
The witness includes its complete canonical model and goal. `mode: "replay"`
rechecks a supplied `witness` and rejects stale models, corrupt frames, wrong reset
indexing, positive-cover records, and induction countermodels. `check_stimulus`
executes explicit `inputs` against a selected goal and reports a concrete
`trace_no_failure` or actual failure; it makes no universal claim.

The [Celox replay gate](../conformance/celox-replay/run_ci.sh) performs the full
source workflow. It compiles handwritten Veryl with the existing pinned frontend,
lifts DUT reset/next expressions without importing expected values, searches for
an actual failure, and drives only the resulting external inputs into an actual
Celox native simulator. Inputs settle first; acceptance/completion are sampled
before exactly one clock event; sequential state is sampled afterward. Celox's
post-edge combinational settle is never substituted for the pre-edge sample.
Reset must establish all selected state independently of power-on values.

The simulator subprocess receives **no expected outputs or property oracle**.
The independently authored native specification supplies the property, and the
original transition replay supplies the trace to compare. A simulator/model
mismatch is `simulator_divergence`, not a reproduced property failure. Celox and
the lifter share frontend dependencies, so agreement is not an independent proof
of frontend correctness. The source/property/binding/model hashes are checked
before saved stimuli run; a newer Celox replays the same saved stimuli.
Regressions retain stimuli and identities rather than simulator-generated goldens.

The gate uses the in-tree Celox and the workspace Veryl through the
`lydite-celox` binaries (`lydite-celox-export`, `lydite-celox-replay`). Their
host-runtime dependencies must be available; a missing simulator or build/runtime
error fails CI rather than skipping replay.
Run `./conformance/celox-replay/run_ci.sh /tmp/fresh-celox-replay` with the repository Rust toolchain.

Fixtures exercise an enabled counter's wrong update and a single-outstanding
request's dropped/deadline failures. Every saved failure is replayed on both the
mutant and correct source under identical stimuli. An unreachable-only fault
retains its induction countermodel but has no bounded reset failure and passes
concrete reset traces. Failure controls also reject internal-state writes,
malformed/stale witnesses, bad sampling/reset indices, and simulation divergence.
The mandatory `celox-replay` CI job uploads fresh evidence; `--record` is an
explicit developer action for changing reviewed regressions and is never used
by CI.

### User project entry point

The fixture gate and external projects use the same public CLI,
[project.py](../conformance/celox-replay/project.py). No fixture name, source layout,
or physical port naming convention is required. First build the tools from
`lydite/`:

```sh
cargo build --locked -p lydite-celox
cargo build --release --locked -p lydite -p lydite-celox
```

Place a manifest in your project directory. This example maps the canonical
names in a native counter specification to differently named RTL pins:

```json
{
  "version": 1,
  "name": "my_counter",
  "sources": ["rtl/counter.veryl"],
  "top": "MyCounter",
  "specification": "contracts/counter.lyd",
  "clock": "clock_pin",
  "reset": {"input": "rst", "active": 0},
  "inputs": {"rst": "reset_pin", "en": "enable_pin"},
  "state": {"count": "count_pin", "fault": "fault_pin"},
  "signals": {},
  "property": "safety",
  "depth": 8
}
```

All fields are required. `sources` may list multiple Veryl files; file paths are
relative to the manifest directory and must remain inside it. `top` selects the
module. `inputs` maps every specification input to exactly one external nonclock
RTL input; the boolean `reset.input` must equal the specification's `reset_input`.
`reset.active` is integer 0 or 1. `state` maps every implementation state variable
to a distinct top-level sequential signal. Names on each side may differ.
Types come from the native implementation declarations and must match the RTL
widths exactly. The counter example needs a native declaration for both `count`
and `fault`; adjust the mappings and declarations together for your design.

The native v3 file contains the relational components, implementation state
schema, abstraction binding, operation selectors, and optional response contract.
Its implementation `reset`, `next`, and wire expressions are typechecked template
fields, then **replaced by the expressions lifted from your source RTL**. They
are not taken as an independently implemented DUT or used to supply expected
outputs. The relational requirements and state/observation bindings remain the
user-authored property.

`signals` maps each declared implementation wire alias to a typed pre-edge DUT
signal. For example, aliases used by a response contract can be mapped as
`"accept": {"signal": "accepted_pin", "type": "bool"}` and
`"complete": {"signal": "done_pin", "type": "bool"}`. Use `"property":
"response_deadline"` to check the single declared response. Signal types are
`"bool"` or `{"bv": N}` for N=1–64; they must match the RTL. All declared wire
aliases must be mapped, with no extra aliases. These signals are also sampled
before the simulator clock edge and compared against the deterministic lifted
DUT expressions. No unique output is inferred from a relational-specification
witness: a relation allowing several next values continues to allow all of them.

Search and save a reproduced failure, then replay it without solver-generated
stimulus selection:

```sh
python3 conformance/celox-replay/project.py search /path/to/project/replay.json --out /tmp/my-search --save-regression /path/to/project/failure.json
python3 conformance/celox-replay/project.py replay /path/to/project/replay.json /path/to/project/failure.json --out /tmp/my-replay
```

Output directories must be fresh. `--save-regression` is optional and never
replaces an existing file. It writes only after an actual failure passes both
original-property replay and Celox simulation. A bounded no-failure or Unknown
result does not create a regression. Saved version-2 regressions contain concrete
inputs and hashes of the manifest, ordered sources, native specification,
resolved bindings, compiled transition model and dependency identity. Moving an
unchanged project directory is allowed; changing its relative layout, content,
mappings, property or depth requires a new search instead of silently accepting
an old regression.

The CLI prints a JSON result and retains details in the output directory.
Exit 0 means the request completed: inspect `status`, which can still be
`reset_reachable_failure`, `bounded_no_failure`, or `unknown`. Malformed projects,
stale regressions and tool failures return `project_error` with exit 2;
`simulator_divergence` remains a separate result with exit 2. No simulator match
is reported for a search with no executable failure witness.

Unsupported mappings fail explicitly: missing or duplicate input/state mappings,
hierarchical names, array lanes, width mismatches, state mapped to combinational
outputs, clock-as-data, inouts, typed/asynchronous reset, arbitrary mapping
expressions, unknown fields, duplicate JSON fields, and project paths escaping
the manifest directory. The existing lifter additionally rejects unsupported
SIR and reset equations depending on arbitrary prestate. This interface retains
the scalar two-state, one-clock, synchronous-reset scope described above.

## AXI4-Lite library

[protocols/axi4lite.py](../protocols/axi4lite.py) generates reusable native v3
components and state-only bindings for sampled AXI4-Lite safety. The normative
reference is [Arm IHI 0022H, ID040120](https://developer.arm.com/-/media/Arm%20Developer%20Community/PDF/IHI0022H_amba_axi_protocol_spec.pdf),
not a claim to support every provision of that document. `rules()` exposes the
same rule identifiers, owners and sections in machine-readable form.

| Supported check / obligation ID | Reference | Independent regression |
| --- | --- | --- |
| `{aw,w,b,ar,r}_valid_stable`, `{aw,w,b,ar,r}_payload_stable` after a stalled sample, including the accepting edge | A3.2.1–A3.2.2 | Every channel and payload field; dropped VALID and changed payload |
| `b_requires_aw_w`: previously accepted address **and** data, separately counted; consume both on B handshake | A3.3–A3.3.1 | AW-first, W-first, simultaneous; missing half, same-edge premature response, duplicate B |
| `r_requires_ar`: previously accepted AR; consume one on R handshake | A3.3–A3.3.1 | Backpressure and continuous transfers; unsolicited, same-edge premature and duplicate R |
| `b_response_code`, `r_response_code`: exclude EXOKAY | B1.1.1 | Reject 1; permit 0, 2, 3 |
| `{manager,subordinate}_reset_valid`: reset-established VALID values are low | A3.1.2 | Each VALID; real reset-dependent combinational-output mutant |

AW and W have independent acceptance counts. Counts represent accepted requests
minus accepted responses; current-edge acceptance cannot justify current BVALID
or RVALID. A full queue may accept and retire simultaneously. Outstanding work
may remain indefinitely: **there is no READY fairness assumption or protocol
response deadline**. These checks do not prove eventual response or lost-response
freedom. Optional performance requirements must be separate explicit bounded
response contracts with their own environment assumptions.

Parameters are `address_width` 1–64, `data_width` 32 or 64, `capacity` 1–16 per
AW/W/AR count, and `role` (`manager`, `subordinate`, or trace-only `link`). Capacity
is a verification bound, not an AXI restriction. Exceeding it is reported as
`scope_exceeded`, never silently wrapped or declared a protocol violation.
Both data widths, all strobe bits (including zero), protection bits and response
bits participate in stability checks. This does not validate their functional
meaning for a particular register map.

For a manager DUT, AW/W/AR source rules are guarantees and B/R source rules are
counterpart assumptions; a subordinate swaps those roles. The generated observer
records counterpart violations separately and stops interpreting subsequent
samples in that reset epoch as a legal environment. A DUT violation found earlier
is retained. No DUT-owned rule is assumed. `link` checks both ends without any
counterpart assumptions. Composition does not discharge an environment assumption
by referring back to the same DUT guarantee. To close an interface, check the
complete sampled link or independently establish the counterpart contract.

### AXI source binding and replay

After the builds described in [User project entry point](#user-project-entry-point),
run the checked examples:

```sh
python3 protocols/axi4lite_project.py search examples/axi4lite/binding.json --out /tmp/axi-subordinate
python3 protocols/axi4lite_project.py search examples/axi4lite/manager-binding.json --out /tmp/axi-manager
./conformance/axi4lite/run_ci.sh /tmp/axi-ci
```

The [subordinate](../examples/axi4lite/subordinate.veryl) has independent AW/W slots
and one read slot; the [manager](../examples/axi4lite/manager.veryl) issues one
transaction of each kind and holds requests under backpressure. They illustrate
protocol control, not a functional peripheral or a certified AXI implementation.

Copy the example project files into your own directory, replace the RTL, and
provide a native v3 specification and [project manifest](../examples/axi4lite/project.json).
The first version requires one unconditional `tick` operation. Existing
components and bindings remain in the checked composition; event-selective and
v4 bindings are rejected. Each protocol pin must map to a distinct typed project
wire alias and an actual top-level port in the role's required direction.
State mapping, reset polarity and source path validation follow the existing
project entry point. The [AXI binding](../examples/axi4lite/binding.json) contains:

```json
{
  "version": 1,
  "project": "project.json",
  "config": {"address_width": 8, "data_width": 32, "capacity": 1, "role": "subordinate"},
  "signals": {"awvalid": "bus_awvalid", "awready": "bus_awready"}
}
```

The short `signals` example above must be expanded to **all** names returned by
`signal_types(config)`; the checked example supplies the complete mapping.
Unmapped or aliased pins, wrong directions, unsupported widths and unknown
configuration fields are errors. History/counter registers belong to the
observer; they are never written into DUT state or accepted as simulator inputs.

```sh
python3 protocols/axi4lite_project.py search /my/project/axi.json --out /tmp/search --regression /my/project/failure.json
python3 protocols/axi4lite_project.py replay /my/project/axi.json --out /tmp/replay --regression /my/project/failure.json
python3 protocols/axi4lite_project.py stimulus /my/project/axi.json --out /tmp/scenario --inputs /my/project/inputs.json
```

`--out` must be fresh. `inputs.json` is an array of complete canonical external
input objects: initial reset, then nonreset edges. Search saves a regression only
for a reproduced failure and never overwrites a file. Replay checks source,
specification, bindings, library/oracle code and dependency identities. Changed
identities are rejected. A failure is replayed against the original property,
then the actual Celox source simulation; an independent integer/snapshot
oracle checks those bus samples. Source behavior, not a chosen relational-spec
witness, determines the comparison values.

`model.json` is the generated native composition. `search.json`,
`scope-search.json`, `axi-contract.json`, simulation samples and the independent
oracle result retain the claim and its boundaries. Search through depth 1–32
reports `bounded_no_failure`, `reset_reachable_failure`, `scope_exceeded`, or
`unknown`; it does not establish unbounded induction. Capacity search uses the
same legal-counterpart prefix, separately from DUT obligations. Search alone does
not establish environment nonvacuity: supply a legal positive stimulus or a
separate cover. The examples require actual handshakes, not merely idle traces. A concrete
`stimulus` run reports environment violations explicitly. Exit 0 means the
request completed; inspect the status. Tool/mapping/divergence errors exit 2.

The Python document APIs default to **conditional guarantees**, not complete
trace acceptance. Generate the other objectives independently:

```python
from protocols.axi4lite import trace_document
checks = {goal: trace_document(config, objective=goal)
          for goal in ("guarantees", "environment", "scope")}
# For source documents, call bind(original_document, ..., objective=goal)
# separately on the original, uninstrumented document for each goal.
```

`environment` checks counterpart violations; `scope` checks capacity on a legal
counterpart prefix. Inspect these results separately from guarantees. Concrete
replay states also expose `axi_environment_bad` and raw `axi_scope_bad`; raw
capacity overflow is recorded even outside a legal counterpart prefix.
`axi4lite_reference.check_trace(rows, config)` returns separate
`conditional_guarantees`, `environment`, and `capacity` dispositions. Its existing
aggregate `status` prioritizes a detected DUT fault, then invalid environment,
then exceeded capacity; only `sampled_prefix_passed` means this supplied prefix
is both legal and in scope. `environment_nonvacuity.checked` is explicitly false:
these trace dispositions do not claim a quantified environment or cover check.

A capacity overflow does not mask a DUT fault on that same edge. A counterpart
violation **does** end the conditional prefix on its own edge, so a coincident
DUT violation is outside that conditional obligation; trace-only `link` mode
checks both without counterpart assumptions. Previously detected DUT faults
remain recorded despite later counterpart violations. Native guarantee replay
stops at its first failure; inspect a separate objective or the complete concrete
trace oracle when examining later events.

### Source-bound request correspondence and application offers

`protocols/source_contracts.py` provides two reusable canonical native-v3 template
APIs. They preserve the original specification and return **separate documents**
for each obligation. No sibling obligation is assumed. The source adapter
`protocols/source_contract_project.py` binds them to external scalar Veryl projects,
using the existing frontend, reset/next lifting, native failure search and actual
Celox replay. These contracts are explicitly supplied design semantics; an AXI
pin list cannot establish a register map, transaction origin or application offer.

`fifo_read(document, name, capacity, request_width, response_width, bindings,
request_valid, request_payload, response_ready, response_function)` supports
capacities 1–16 and scalar widths 1–64. `bindings` maps `count`, `slot_0` through
`slot_N`, `ready`, `valid`, and `data` to state-only implementation expressions.
The independently authored response function uses only the `request` placeholder
and native literals/operators. The obligations are:

- `storage`: reset empties the FIFO; actual request handshakes append payloads;
  actual response handshakes retire the oldest; surviving occupied slots retain
  their contents. Simultaneous pop/push, including a full queue, is supported.
- `capacity`: occupancy stays within the declared bound and no accepted request
  overruns a full queue without simultaneous retirement. This is a design capacity
  contract, not an AXI limit or a truncating observer.
- `response`: whenever an actual response is offered, the queue is nonempty and
  its data equals the supplied function of the oldest stored request.
- `stall`: an unaccepted offered response remains valid with unchanged data.

No empty bypass or eventual-response requirement is introduced. Inactive slot bits
are unconstrained. The source adapter requires explicit actual state mappings and
actual output ports; it rejects input-dependent output abstractions, wrong widths,
duplicate slot aliases, and response functions referring to DUT state. Each bound
output is also lowered separately with reset asserted. The native typed evaluator
checks that its settled reset value equals the normal state-only abstraction
applied to reset-established registers. Input-dependent or differing reset values
are rejected as unsupported bindings, before any obligation can pass. This does
not impose an AXI reset value on payload/READY: it rejects a phase-dependent output
that the single state-only binding cannot represent. Equal reset values, including
compatible reset muxes, remain supported. `reset-output-bindings.json`,
`reset-output-model.json` and `reset-output-check.json` retain this evidence;
concrete replay additionally compares the claimed physical outputs with Celox's
post-reset-edge sample in `reset-output-comparison.json`. No reset waveform or
asynchronous timing claim is added. A same-width
wrong slot mapping is still a possible author error: checked storage transitions,
not the field name, provide its behavioral evidence. Conclude correspondence only
when **all** obligations succeed for the same source/binding identity and scope.

The self-contained `examples/source-contracts/read-contract.json` binds a two-slot
read subordinate to a separately written ROM map: address 0 gives 17, address 4
gives 34, all other addresses give 0. Actual source slots hold accepted addresses
until response retirement. A mutant selects and removes the younger request,
returning 34 then 17 while keeping ordinary response counts, prerequisites and
stalled-payload stability legal. Both response correspondence and storage refinement
expose it. No response-time tag is assigned by a monitor. Repeated-address tests
retain multiplicity and FIFO positions, but equal response values cannot distinguish
physical origins; this example does not establish arbitrary memory/peripheral effects
or write-response origin from identical OKAY codes.

`idle_offer_step(document, name, bindings, offer, completion)` binds actual `busy`
and `valid_0` through `valid_N` expressions. It separately checks `resource` accounting
(completion wins the busy update) and `launch`: an application offer when not busy
and with no pending bound VALID establishes those VALIDs on the next edge. This is
an **explicit one-step application adapter contract**, not normative AXI latency,
fairness, general progress, or independence from all historical READY values.
The application defines offer/completion meaning. Configuring a READY-gated offer
would not justify that meaning and must not be presented as proof of AXI causality.
The read/write examples bind real `start_read`/`start_write` inputs, busy registers,
and corresponding VALID outputs; mutants gate initiation on READY. VALID aliases
must resolve to distinct physical output ports: two alias names for one pin cannot
establish two offers. Distinct physical pins with equal normal expressions remain
supported; expression equality alone is not an endpoint collision. Actual READY-low
covers demonstrate available work and detect the registered wait even though ordinary
sampled AXI and source combinational-path checks pass. This first template does not
claim simultaneous busy-slot replacement or arbitrary pipeline offer behavior.

Use the supplied binding files as external-project templates. Each names a source
project and either a `fifo_read` or `idle_offer_step` contract with explicit aliases.
For example:

```sh
python3 protocols/source_contract_project.py search \
  examples/source-contracts/read-contract.json --obligation response \
  --out /tmp/read-response --regression /tmp/read-response.regression.json
python3 protocols/source_contract_project.py search \
  examples/source-contracts/write-offer.json --obligation launch \
  --out /tmp/write-launch
# Also run storage/capacity/stall, and the offer resource obligation separately.
```

`stimulus --inputs file.json` checks concrete external inputs; `replay --regression
file.json` requires an exact saved failure identity. Source, manifest, specification,
contract/function, selected obligation and contract helper/driver hashes participate
in identity. Reports retain the selected obligation, declared bindings/semantics,
independent obligation names, actual simulator comparison and bounded scope. A
single successful obligation is not a combined verdict. The original specification
also remains checked: a failure may originate there or in the selected contract,
so inspect the native model and trace before attributing it. No failure means bounded
no-failure, not an unbounded theorem. Positive concrete covers establish only their
recorded executions, not general progress. One initial reset is supported; repeated
reset epochs remain rejected by the source replay route.

`conformance/source-contracts/run.py` tests independent depth-10 FIFO searches,
read/write offer searches, two outstanding requests, stalls, repeated addresses,
simultaneous events, full-queue backpressure, reset polarity, mutations, incorrect
bindings, reset-phase mismatches on VALID/data/READY under both polarities, physical
endpoint aliases, and stale witnesses. The complete AXI guarantee and capacity
queries at depth 10 are now mandatory bounded no-failure regressions, alongside
independent actual scenario traces. A future `Unknown` fails this regression.

The bounded-search encoder balances each frame's conjunction of reset/transition
equations instead of extending one linear chain. For this 54-state example at depth
10, the reset-prefix conjunction spine drops from 604 levels to 16; the unchanged
finite-solver depth limit is 512. It also factors the complete machine trajectory
outside the disjunction of failures. This preserves existence of a bounded failure:
every earlier failing prefix extends to the bound because each scalar machine
state has a total next expression and future nonreset inputs are unconstrained.
Specification invariants, relational guarantees and response assumptions are **not**
added as trajectory constraints. Every reset/next equation and failure predicate is
retained, and SAT witnesses still undergo original transition/property replay.

This factoring exposes mandatory equations to existing exact equality sharing. It
also resolves the 64-bit memory-effect query's clause exhaustion without changing
any limits or branch accounting. Three identical-request native-process runs gave:

| Depth-10 query | Earlier encoding | Current encoding | Current terms / final clauses / work |
|---|---|---|---|
| Full read-example AXI guarantees | depth-budget Unknown, median 0.073 s | bounded no-failure, 0.112 s | 6,055 / 28,126 / 1,633,794 |
| Full read-example AXI capacity | depth-budget Unknown, median 0.056 s | bounded no-failure, 0.091 s | 5,597 / 27,422 / 1,497,944 |
| 64-bit memory byte effects | balanced-prefix clause-budget Unknown, median 0.259 s | bounded no-failure, 1.067 s | 3,063 / 716,918 / 71,319,783 |

The first two baselines stopped before encoding terms. The 64-bit baseline had
allocated 934,659 clauses when another split copy would exceed the unchanged
1,000,000-clause budget. Timing depends on the environment; these are completed
queries replacing failures, not a claim that failed queries were slower.
`conformance/source-contracts/measure_search.py --out DIR --baseline-binary PATH`
records request/binary hashes, raw runs and counters; `--request-file FILE` accepts
an exact additional native safety-search request. Without that option it prepares
the full read-example AXI guarantee and capacity queries. Omitting the baseline
argument measures only the current binary.

Native regressions exhaust all seven-input conjunction valuations; check wide
frames and first/middle/last equation mutations; preserve reset and final-edge
failures; compare bounded searches against exhaustive concrete prefixes, including
already-failed invariants; replay original transitions; and retain `Unknown` for
a deliberately insufficient depth budget. No bounded result becomes an unbounded
safety theorem.

### Source-bound memory writes and byte effects

`memory_write(document, name, address_width, data_width, locations, initial,
bindings, inputs)` extends the same native source-contract library. The source
adapter's `memory_write` descriptor in
[the example binding](../examples/source-contracts/memory-contract.json) supplies
explicit memory-register locations/reset contents, input aliases, request-storage
state, and actual effect/response signals. It supports 32/64-bit words and 1–16
explicit aligned cells, one pending read response, and one outstanding AW/W pair.
The example has two cells;
it is a memory model, not inferred semantics for an arbitrary peripheral.

The source driver also supports [checked inductive strengthening](lemmas.md#source-bound-inductive-safety) through `induct`. The declared 32/64-bit memory examples now establish all six safety obligations for unbounded execution, with reset establishment, lifecycle-invariant preservation and each original target use checked separately. Bounded search and original-source replay remain available for failure discovery.

The six independently checked documents are:

- `requests`: accepted AW and W payloads enter their actual source holding slots
  independently and remain there while pending. Inactive payload bits are free.
- `capacity`: neither write holding slot nor the pending read slot is overwritten
  by a new acceptance before retirement (same-edge retirement/replacement is
  representable, including read retire/refill). This is the
  declared adapter capacity, not an AXI-wide outstanding-transaction limit.
- `effects`: actual memory-register transitions equal byte-enabled updates from
  the captured request, only on the bound application event. Disabled lanes,
  other cells and all cells outside an event remain unchanged. Reset contents
  are checked against the separately declared values.
- `completion`: an application event needs both accepted payloads and cannot be
  repeated for an already applied pair; the real applied flag tracks application
  through response retirement. No application deadline is introduced.
- `response`: an offered B response needs the applied pair, has the declared
  region's status, and remains stable while stalled. This explicitly binds the
  response to the write-effect lifecycle rather than assigning response-time tags.
- `readback`: an accepted read returns the pre-edge addressed memory word and
  status on the next state, retaining it under backpressure. Payload is unconstrained
  while the next response is invalid; clearing idle RDATA/RRESP is supported. This is the example's
  explicit one-slot, one-edge **read-before-write** contract, not a normative AXI latency or
  universal simultaneous-access ordering rule.

Buffered read implementations need a separately declared FIFO correspondence
contract; this memory template does not impose its one-slot capacity on arbitrary
AXI designs. The independent byte-array oracle counts a pending accepted read
until response handshake and rejects replacement, even for identical addresses
and data. Actual 32/64-bit always-ready mutants exercise different data, equal
data and different response status under a stall; both original witness replay
and explicit legal retire/refill traces are checked. Stall retention is an
independent `readback` clause and never assumes the sibling capacity obligation.

All six must succeed for the same source/semantic binding before combining their
claims. The function/region model is independently supplied: byte lanes are
little-endian, address low bits select offsets within the aligned bus-width word,
and WSTRB selects modified bytes. Unmapped writes change no cell and complete with
DECERR; unmapped reads return zero/DECERR. The `apply` alias may bind an actual
internal source signal (never an external input); other signal aliases require
actual output ports. The declared event meaning is not inferred from a name.
Actual request retention, memory transitions and response conditions are checked
against it; no monitor-created transaction origin or assumed DUT guarantee is used.
As with the other templates, separate reset lowering and actual reset-output
comparison protect state-only bindings.

The source suite exercises both widths, all address offsets, full/sparse/zero
strobes, independent AW/W arrival, full-slot backpressure, successive writes,
response stalls, reset polarities, unmapped accesses, and reads simultaneous with
or stalled across a write. A separately implemented byte-array oracle tracks actual
handshakes and compares every actual memory cell after each Celox edge. Mutants
ignore strobes, select the wrong lane/word, lose accepted payload, clobber disabled
bytes, acknowledge without an effect, repeat an application, corrupt readback or
write without a request. They must fail both the native obligation and an explicit
legal source scenario; the ordinary AXI checks still pass those concrete traces.
An additional slot-overrun mutant is classified separately as a declared-capacity
failure. Source/semantic identity protects saved witnesses.

The generic application search does not assume AXI counterpart legality. Tests
therefore keep searched witnesses and explicit legal bus stimuli separate, instead
of depending on a solver's choice of input values. Positive application/response
covers establish the exercised progress only. These contracts establish bounded
memory semantics for declared bindings; they do not promise eventual service,
multi-outstanding write reordering, external memory visibility, cache coherence,
physical timing, or unbound peripheral side effects.

### Explicit optional response-output profile

Version 1 AXI bindings still require the full signal set. Version 2 supports one
narrow optional profile: an AXI4-Lite subordinate with **both BRESP and RRESP
absent**, declared not to support exclusive accesses or generate error responses.
Arm IHI 0022H A9.1 and A9.3.6 provide the omission conditions; Tables A9-2/A9-4
supply the two-bit OKAY defaults. B1.1.1 refers Lite optional signals to A9.
This is profile support, not a requirement that every subordinate omit responses.

Keep the existing `project`, `config` and `signals` binding fields, set
`version` to 2, omit `bresp` and `rresp` from `signals`, and add:

```json
"profile": {
  "name": "subordinate_no_error_responses",
  "capabilities": {
    "supports_exclusive_accesses": false,
    "generates_error_responses": false
  },
  "omitted": {
    "bresp": {"port": "bresp", "type": {"bv": 2}},
    "rresp": {"port": "rresp", "type": {"bv": 2}}
  }
}
```

The declared port names must be absent from the actual top-level frontend
reflection, not merely removed from the binding. Remove the corresponding
physical signal mappings from the source project and their placeholder wires
from the native specification; no fake observed wire is created. All other AXI
signals remain explicitly mapped with normal direction/width checks. Every actual
top-level output must be one of those mapped AXI outputs for this initial profile.
Thus declaring fake absent names cannot hide remaining response ports. Additional
non-AXI outputs, partial response omission, manager-side omissions, other defaults,
and unknown profiles are unsupported configurations rather than protocol faults.

Only after validation does the binder use the ordinary native constant `0u2` for
normal/reset response samples. Simulation samples come from real ports for all
present signals; the two synthesized canonical response values are labeled as
profile defaults in `axi-profile.json`, `axi-contract.json`, and result/oracle
`signal_profile` metadata. Reports include their normative origin, reflected
absence evidence, and the list of source-sampled signals. Structural obligations
cover real ports only; absent signals do not acquire fictional physical endpoints.

The two capability values are **explicit user declarations**, not deductions or
functional proofs from missing pins. Results remain conditional on them and on
correct integration of the default values. Physical tie-offs/interconnect wiring,
internal error semantics and functional completeness are not verified. Changes to
the profile invalidate saved replay identity. No required VALID/READY signal is
defaulted, and version 1 never infers defaults from missing mappings.

Source tests compare 32/64-bit omitted designs with explicit-OKAY counterparts
using bounded formal search and identical concrete traffic, including AW-first,
W-first, simultaneous channels and backpressure. This is bounded/concrete evidence,
not an unbounded equivalence theorem. Existing protocol mutants must still fail;
configuration errors must remain `project_error`, not protocol counterexamples.

### First reset-release sample

`manager_reset_release_valid` checks the manager rule in Arm IHI 0022H A3.1.2,
Figure A3-1. During the first released tick, settled **before-edge** AWVALID,
WVALID and ARVALID must be low. They may become high immediately **after** that
tick. This does not require another idle cycle or constrain request latency.
The rule is a manager/link guarantee and a subordinate-side counterpart condition.
A3.1.2 names only those manager outputs in its earliest-release sentence;
subordinate B/RVALID remains subject to reset-low and accepted-request
prerequisites, rather than an invented additional release-delay rule.

The generic helper `protocols.sampled_phase.first_release_guard(state_name,
required_before)` produces ordinary native v3 state/reset/next expressions and
a violation predicate. A private phase bit resets to true, clears on the first
unconditional nonreset tick, and gates `not required_before`. Callers must merge
its state with collision checks and route its violation into a checked obligation.
For example, an equivalent native implementation fragment is:

```text
state release_pending: bool;
state release_bad: bool;
reset { release_pending = true; release_bad = false; }
next {
  release_pending = false;
  release_bad = s.release_bad || (s.release_pending && i.valid);
}
```

The phase guard observes the current input/state expressions; it does not test
`n.valid`. AXI uses it with the conjunction of the three manager VALID-low
predicates. Normal native type validation applies. This is a library expression,
not a new special-purpose native solver or syntax construct.

The source result's `reset_release.source_phases` retains original Celox
`before` and `after` VALID observations for the reset and first released ticks.
The adapter drives each frame, settles combinational logic, records `before`,
ticks the declared positive-edge clock, then records `after`. Reset polarity is
resolved from the existing project manifest. Formal replay uses the same pre-edge
signal expressions; the independent oracle checks the concrete samples.
A reset-only prefix reports release as `pending`, not passed.

The native/source scope remains exactly one initial reset. Later reset frames
are rejected by both replay and the simulator adapter. The standalone oracle can
inspect explicitly supplied repeated epochs, but this does not authorize a
repeated-reset source or formal claim. Physical synchronous deassertion,
asynchronous assertion, glitches, recovery/removal and behavior between sampled
phases remain external timing obligations. Sampled success is not physical reset
compliance.

### Offered write address/strobe consistency

`write_address_strobe` checks Arm IHI 0022H A3.2.2, A3.4.4 and B1.1.3 for the
explicit 32/64-bit full-width Lite profile. A3.2.2 requires valid payload when
VALID is asserted; A3.4.4 permits arbitrary strobes only when WVALID is low and
requires consistency with the unaligned address. Therefore a known corresponding
AW/W offer is checked before READY, not only at acceptance.

Independent bounded queues retain accepted AW address offsets and W strobes.
Each channel's oldest unmatched accepted entry precedes its current VALID offer.
The checker compares those oldest known counterparts; if a queue is empty, its
current VALID payload supplies that side. This covers accepted AW with stalled
W, accepted W with stalled AW, and both stalled first offers. A live offer cannot
skip earlier accepted entries. Queue advancement still requires handshakes, and
existing VALID/payload stability checks protect stalled offer identity. No READY
fairness or completion deadline is assumed.

A mask is legal when no asserted lane is below `AWADDR modulo (data_width / 8)`.
Aligned addresses permit any mask; zero and sparse masks are legal. The concrete
oracle and source result expose `write_pairing`: unmatched known positions are
`pending`, not validated; `known_offers_checked` covers only the sampled known
counterparts, not future offers or eventual acceptance. Invalid environment or
capacity makes correlation `unknown_outside_legal_scope`; detected strobe faults
remain `violated`. Search without a concrete trace reports correlation as
`not_established_by_bounded_search`. Native state `axi_write_pair_pending` exposes
the raw last-sample missing-counterpart indicator, meaningful only on a legal,
in-scope prefix. An idle trace says `no_pending_offers`, not transaction coverage.

This is manager-owned safety: manager/link guarantees check it, while subordinate
checks treat it as a counterpart requirement. The queues reset with the monitor;
oldest-entry consumption and simultaneous append preserve ordering. Existing
AW/W-to-B outstanding counts still define capacity independently. An unmatched
queue cannot exceed its outstanding count on a legal prefix, so capacity remains
a tool bound, not a strobe rule. Earlier failures remain sticky, including a
strobe fault on the same edge as capacity overflow. After an earlier overflow,
truncated pair queues and saturated response counters no longer establish
transaction correspondence or multiplicity. New address/strobe and response-
prerequisite accusations are therefore suppressed at their raw predicates,
including counterpart/environment flags. Correlation is unknown outside scope;
earlier definite faults remain recorded. Native trace replay uses one
initial reset; the independent oracle additionally tests repeated reset epochs.

This does not prove subordinate byte-write effects, memory contents, or response
transaction origin. The separate source-bound FIFO template above supplies bounded
read correspondence evidence only with an independent response function and actual
RTL storage/output bindings. The explicit memory template additionally binds a single outstanding write pair
to its actual byte effects and response lifecycle. Other write/peripheral origin
models and further optional profiles remain distinct conformance scopes.

### AXI limits and validation

The gate runs the existing Celox replay tests plus positive source scenarios,
manager/subordinate mutants, replay and stale-identity controls. Hand-authored
expected traces cover every supported rule, both roles and widths, simultaneous
pop/push at capacity, indefinite stalls, and reset epochs. Exhaustive short write
sequences and fixed-seed full-channel sequences compare the generated native
monitor to a separately implemented procedural oracle. Actual source runs cover
AW-first, W-first, simultaneous, continuous traffic and response backpressure;
mutant stimuli also run on correct RTL with explicit expected transfer counts.

The sampled contracts alone leave unchecked: input-to-output combinational paths, VALID dependence on READY or
other channels, physical asynchronous reset assertion/deassertion, glitches and recovery/removal
timing, transaction-identity ordering and functional address/data/strobe behavior,
fairness/liveness, X/Z, CDC, bursts, IDs and other AXI variants. Source replay uses
a single positive-edge clock, ordinary bit synchronous reset, scalar two-state
ports, one initial reset and the Celox/Veryl subset. Standalone
`trace_document(config)` / `bind(...)` provide the generated contract API;
`axi4lite_reference.check_trace(...)` accepts full sampled traces, including
explicit reset epochs. Neither constitutes an asynchronous-reset timing check.

This experimental tooling is provided as-is, with no AXI certification or fitness
claim. Review its assumptions and independently validate it for your use. This
notice does not change the repository's licensing terms or provide legal guarantees.

## Native source structural contracts

A v3 component may require absence of a combinational path independently of its
behavioral relations. This is **new source-graph checking**, not a temporal or
physical timing proof. See the complete [native example](../examples/no_comb_path.lyd)
and [Veryl source](../examples/no_comb_path.veryl).

```text
component Isolation {
  init true;
  invariant true;
  steps { tick = true; }
  structure {
    no_comb_path isolated { from i.a; to o.y; }
  }
}
```

Endpoints reference declared shared inputs (`i.a`) or observations (`o.y`) for
their names/types. `implementation` binds them **separately** to actual source
signals, not to the behavioral state-only observation expressions:

```text
endpoints { i.a = a; o.y = y; }
```

Mappings are plain typed signal paths, including named child ports. Direct scalar
parent/child port connections denote the same source net and are traversable in
both directions, including when a contract starts at a child input. Assignment
data/control edges remain directed; shared assignment inputs do not make their
destinations aliases. Register cuts remain non-traversable. Expressions,
undeclared logical endpoints and direction/width mismatches are rejected. A
hardware endpoint binding does not itself prove that a behavioral abstraction
matches the same physical signal. Existing behavioral refinement obligations
remain separate. Nested v3 compositions collect contracts from their selected
components, retain qualified rule names, and apply the existing duplicate-member
checks. V4 structural declarations are not implemented and are rejected.

With the pinned frontend and release binaries built, run:

```sh
python3 conformance/celox-replay/structure_project.py examples/no_comb_path.project.json --out /tmp/no-comb-path
```

The source command checks structural obligations only. To check the native
behavioral model and structural obligations together using that artifact:

```sh
cargo run --release --bin lydite -- examples/no_comb_path.lyd --structural-artifact /tmp/no-comb-path/structural-graph.json --out /tmp/combined
```

This checks the supplied native behavioral model; it does not by itself establish
that its reset/next abstraction matches the RTL. Source lowering/replay provides
that separate connection. Structural violations produce `structural_contract_failed`;
unbound/unsupported structural checks keep the combined result `unknown`.

The small manifest names `version: 1`, `top`, a nonempty list of source filenames,
and `specification`; files must remain within its directory. The command compiles
the original source with the existing pinned frontend, extracts a graph from the
**original source tokens**, cross-checks every variable and instance path against
typed frontend reflection, and runs the generic Rust `lydite-structure` checker.
The source parser is part of the trusted frontend boundary. The Rust checker
validates and traverses its artifact; it does not independently recompile RTL or
authenticate an arbitrary caller-supplied graph.

The graph contains whole-variable data, control and hierarchy-connection edges.
Assignments in supported positive-edge `always_ff` blocks produce explicit
sequential cuts. Data/control paths through combinational assignments remain even
in `a ^ a`, masked expressions, overwritten assignments or constant branches.
Thus `violated` means a path in this conservative **source-variable graph**; it
need not imply a sensitizable path or a path in optimized synthesized hardware.
No absence claim is inferred from simplified behavioral expressions or SAT
support sets. A register cut does not establish independence from historical
READY values or compliance with transaction-offer dependency requirements.

Supported source subset: scalar bit/logic widths 1–64, ordinary assignments,
expressions without function calls/casts/selects, `always_comb`, block `if/else`,
positive-edge `always_ff` with an ordinary clock, and fully named scalar module
connections. Parent/child aliases and sequential boundaries are retained.
Generics, generate blocks, arrays/slices, interfaces, external/opaque modules,
functions, casts, loops, case statements, `else if`, and typed/asynchronous reset
constructs are unsupported. Source budgets are explicit. Any unsupported construct
or incomplete reflection coverage makes the **whole artifact unsupported**;
there is no assumption that a missing subgraph is isolated.

Results are `verified`, `violated`, `unsupported`, `unbound`, or
`invalid_or_tool_error` (`not_requested` if no contracts are declared). Only `verified` completes the requested structural
checks. Path witnesses identify source files/lines and edge kinds; reports retain
native declaration locations, source identity, extractor identity, coverage and
sequential cuts. The CLI exits 0 for verified, 1 for violated, 3 for incomplete
checks and 2 for invalid/tool errors. An ordinary `lydite` or editor proof
without the source artifact reports the structural obligations as unbound and
keeps the overall result `unknown`; the editor provides a blocking declaration
location diagnostic. Behavioral replay rejects documents with retained structural
contracts, rather than silently ignoring them.

The AXI source entry point now generates a separate native structural composition
for every protocol input/output pair and uses this same checker. It emits
`structure-model.json`, `structural-graph.json` and `structural-result.json` beside
the sampled evidence. A path reports `structural_violation`; incomplete structural
coverage prevents a successful combined result. Clock/reset physical timing and
registered wait-on-READY are not established by this structural checker. The
regression suite includes registered READY waits that pass this check and fail the
separate, explicitly declared application offer contracts above. Those contracts
do not establish unrestricted temporal offer causality for arbitrary DUTs.

The [machine-readable AXI conformance inventory](../protocols/axi4lite-conformance.json)
records exact sections, profiles, tests and outstanding gaps. It is explicitly
partial, pending an independent completeness audit. Address/WSTRB consistency is checked for the earliest known corresponding
offers, including stalled and W-first traffic; response correspondence still needs independent transaction-origin
evidence, not response counters alone.
Physical reset timing, repeated-reset source execution, optional/default signal
profiles beyond the explicit optional-response profile and memory-versus-peripheral
requirements remain visible gaps. No full AXI compliance claim is made.
