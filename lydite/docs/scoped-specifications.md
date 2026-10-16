# Specifications with ports and explicit composition (0.9 / JSON version 4)

Each specification declares its own local inputs and outputs, and composition
binds ports explicitly.
An `operation` is a relation that permits one logical operation; it is not a clock
event or a sequentially executed block like `always_ff`.
The existing version 2 `design` and the version 3 shared global-port form are unchanged.
For the latter, see [relational specifications](specifications.md).

For 0.9 current/primed values, expectations, input and execution quantification, and
the short operation-call notation, see [expectations and quantified traces](expectations.md).
The positive/negative examples in this document keep the original existence checks.

## Examples to read first

- [scoped_budgeted_counter.lyd](../examples/scoped_budgeted_counter.lyd): explicitly wires arithmetic and budget specifications, and checks finite examples and an implementation binding
- [scoped_dual_operator.lyd](../examples/scoped_dual_operator.lyd): selects independent left/right operations and checks one at a time, both at once, and both idle. The implementation's two selectors may also be true at the same time
- [scoped_independent_counters.lyd](../examples/scoped_independent_counters.lyd): uses the same Counter twice and renames ports in a nested composition. State is independent; operations synchronize through the shorthand. Binds the state of `pair.left` / `pair.right`
- [scoped_contradictory_outputs.lyd](../examples/scoped_contradictory_outputs.lyd): an intentional example where separate specifications constrain the same output and only the positive example after composition fails

Each `.lyd` and the `.json` of the same name represent the same version 4 document.

```sh
cargo run --locked -- examples/scoped_budgeted_counter.lyd --check --out /tmp/scoped-check
cargo run --locked -- examples/scoped_budgeted_counter.lyd --out /tmp/scoped-budgeted --z3 /path/to/z3
cargo run --locked -- examples/scoped_independent_counters.lyd --out /tmp/scoped-pair --z3 /path/to/z3
cargo run --locked -- examples/scoped_dual_operator.lyd --out /tmp/scoped-dual --z3 /path/to/z3
cargo run --locked -- examples/scoped_contradictory_outputs.lyd --out /tmp/scoped-conflict --z3 /path/to/z3 # exit 1
```

`--check` / `--emit-json` perform full syntax, name and type checking but do not run proof obligations.

## Local interface of a specification

```
specification "Counter"

spec Counter(input amount: bv<4>, output count: bv<4>) {
  state value: bv<4>;
  init value == 0u4;
  invariant count == value;
  operation add { expect value' == value + amount; }

  example add_two {
    expect positive;
    initial { count = 0u4; }
    trace { add { inputs { amount = 2u4; } observe { count = 2u4; } } }
  }
}
```

`specification "name"` needs no enclosing braces or semicolon after it.
The parentheses of `spec Counter(...)` hold that specification's own interface; write `()` even with no parameters.
Ports of other specifications and implicit document-wide global variables cannot be referenced.

- `amount` / `i.amount`: input of the current operation
- `count` / `o.count`: current output
- `count'` / `no.count`: next output
- `value` / `s.value`: current private state
- `value'` / `n.value`: next private state

Bare port names and `i.` / `o.` references are both resolved in that specification's declaration scope.
`init` / `invariant` reference only the current private state and outputs.
`operation add = ...;` is a Boolean relation over inputs, current/next state and current/next outputs.
State relations to be updated are stated explicitly; omitted state is not interpreted as an implicit hold.
`operation add = true;` leaves the operation unconstrained; write a hold as `n.value == s.value`.

An `output` is a relational observable, not a declaration of a single hardware driver.
Multiple specifications may constrain the same output; if the constraints conflict, that behavior no longer exists.
Types, fixed-width wrapping and expression operators are shared with the [language guide](language.md).

## Port binding and composition

```
composition Budgeted(input amount: bv<4>, output count: bv<4>, output remaining: bv<4>) {
  use Counter(amount: amount, count: count);
  use Budget(amount: amount, remaining: remaining);
  operation add = actions(Counter.add, Budget.add);
}
```

`use Counter(...)` creates an instance named Counter.
The left side of a binding is the child's port name; the right side is this composition's port name.
Every child port is bound exactly once, with exactly matching type and direction.
A child input connects to a parent input, and a child output to a parent output.
Undeclared ports, width mismatches, crossed directions, unconnected ports and duplicate bindings are errors.
The right side cannot be a constant, expression or internal wire, and an output cannot be connected to another child's input.

### Exposing operations and independent execution

`operation add = actions(Counter.add, Budget.add);` defines the composition's public operation `add`,
conjoining the relations of the two named child operations in one step.
The right side is a nonempty list of direct child instance names and their public operation names (`alias.operation`).
If a child is a composition, its public operations expand recursively down to leaf spec operations.
A parent may choose public operation names different from the child's, but cannot reach past the child into its internal paths.

```
composition Pair(input a: bv<4>, input b: bv<4>, output x: bv<4>, output y: bv<4>) {
  use left: Counter(amount: a, count: x);
  use right: Counter(amount: b, count: y);
  operation left = actions(left.add);
  operation right = actions(right.add);
}
```

This Pair can run `left` and `right` separately or at the same time.
A public operation declaration defines a permitted group; it does not start execution automatically.
Each trace frame specifies the set of public operations chosen at that point.
Having two operations does not require enumerating an alias for every subset in advance.

Groups are treated as sets. The same operation of the same leaf instance runs only once even if reached through several paths.
Operations of different leaves can be chosen together. Choosing different operations of the same leaf at once is incompatible, and that trace is UNSAT.
This is distinct from syntax or name errors, and a negative example can confirm that it is disallowed.
Independence is decided by the full instance path, so `left` and `right` using the same spec are different leaves.
With explicit groups, children may have different sets of operation names.

If a composition omits all operation declarations, the shorthand synchronizes all members on same-named operations.
Only in this case must all members have the same nonempty set of operation names; each operation combines the same-named operation of every child.
Once any explicit group is written, no other operations are added implicitly.
Empty groups and references to unknown children/operations are rejected.

### Reuse and nesting

```
composition Pair(input a: bv<4>, input b: bv<4>, output x: bv<4>, output y: bv<4>) {
  use left: Counter(amount: a, count: x);
  use right: Counter(amount: b, count: y);
}
```

`left` / `right` are instance names. Private state is independent even when the same specification is used;
constraints are linked only through shared values when the instances connect to the same inputs or outputs.
Instance names cannot repeat within one composition.
Using a composition, as in `use pair: Pair(...)`, composes port bindings in a nested way.
The leaf identifiers of the private state become `pair.left` / `pair.right`.
Unknown targets and cycles are rejected before expansion.
All unused declarations are also checked before the solver starts. Expansion is limited to 64 levels of nesting,
4096 leaves per target, and 16384 leaves summed over all expansions in the document. Exceeding a limit is an input error, never a verification success.

## Scope of positive and negative examples

Place `example name { ... }` directly in each `spec` / `composition`.
`initial` / `observe` use that target's output names; trace `inputs` use its input names.
`add { ... }` in a trace is an occurrence of an operation, a separate syntax from the `operation` declaration.
Writing the same operation several times keeps each as a separate step, in order.

```
trace {
  left { inputs { a = 2u4; b = 7u4; } observe { x = 2u4; y = 0u4; } }
  actions(right) { inputs { a = 9u4; b = 3u4; } observe { x = 2u4; y = 3u4; } }
  actions(left, right) { inputs { a = 4u4; b = 5u4; } observe { x = 6u4; y = 8u4; } }
  actions() { inputs { a = 1u4; b = 1u4; } observe { x = 6u4; y = 8u4; } }
}
```

`left { ... }` and `actions(left) { ... }` mean the same; the former is the original single-operation notation.
`actions(left, right)` chooses both in the same step; it does not become two sequential steps.
`actions()` is a step that chooses nothing. The boundaries of ordered trace frames define logical steps.

**Unchosen leaves hold only their private state.** Current/next invariants of all leaves are required in every frame.
No uniform hold or implicit driver ownership is imposed on public outputs.
For example, this Counter's output is `count == s.value`, so it is held even when not chosen,
but in a specification whose invariant does not uniquely determine the output, the output may change even when no leaf is chosen.
When two child outputs connect to the same parent output, both relations must hold,
so even leaves that can be chosen together can make a trace UNSAT through conflicting output constraints.

positive requires at least one finite execution matching the description; negative requires that none exists.
Omitted input and output values and private state are existentially quantified, not filled with 0 or similar.
`initial {}` / `trace {}` must be written explicitly even when empty.
A trace step's `inputs` / `observe` may be omitted and are treated as an empty partial description.

**Each declaration's examples are checked once, against that declaration's own interface.**
Instantiation does not inherit or duplicate examples, and passing child examples does not count as success of the composition.
Write examples on the composition itself for any required post-composition behavior.
In `scoped_contradictory_outputs`, the standalone examples of One and Two pass,
while the positive example of the composition connecting both to the same output fails.

SAT witnesses are obtained over all states and all inputs/outputs and rechecked; UNSAT rules out every hidden completion.
UNKNOWN and solver failures are never success. Passing finite examples is not a proof of specification completeness,
arbitrary-length executions, deadlock freedom, liveness, or that the implementation can realize every positive example.
Passing only negative examples does not show that any permitted execution exists.

## Implementation and state mapping

An implementation selects one composition and declares its inputs with the same names and types.
Additional inputs such as reset or accept controls are also declared explicitly in the implementation.
In this version, `implementation` has no port signature and uses individual `input` declarations.

```
implementation {
  composition Budgeted;
  input rst: bool;
  input go: bool;
  input amount: bv<4>;
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
    output count = s.count;
    output remaining = s.remaining;
  }
}
```

The implementation's `operations { add = ...; }` is a Boolean selector, while the spec's
`operation add = ...;` is a relation. The names correspond but the roles differ.
The existing machine semantics of `reset` / `next` / `wires` / selectors are unchanged.
Reset is active-high, synchronous and highest priority, and each next expression reads the pre-update state.

The binding maps every state of every leaf instance and every output of the selected composition,
with nothing missing or extra. Nested leaves are written `bind pair.left { value = s.left; }`.
Mapping expressions depend only on the implementation's current state; dependence on current inputs or wires is not allowed.
This version verifies that reset establishes the initial condition and invariants, that each step preserves the invariants,
the relation of each chosen leaf operation, and that each unchosen leaf holds its mapped state.
When no operation occurs at all, the same per-leaf rules apply, and no uniform hold of public outputs is imposed.

Selectors may be true at the same time when they act only on different leaves or redundantly choose the same operation of the same leaf.
Exclusivity is proved only for combinations that choose different operations of the same leaf.
Exclusivity is a conclusion to verify, never an assumption used to make other obligations trivial.
In `scoped_dual_operator`, `left = i.go_left; right = i.go_right;` is
accepted even when both are true, and when both are false the private state of the two Counters is held.
This is verified with per-leaf choice conditions, without enumerating every selector subset.

The check is safety / stuttering refinement under the supplied deterministic state mapping, without restricting to reachable states.
An implementation that always stutters can also pass. Progress, fairness, termination, and implementability of every specified behavior are not required.

## version-4 JSON

For a complete example, see [scoped_budgeted_counter.json](../examples/scoped_budgeted_counter.json).
The array forms of expressions, types and finite examples are shared with version 3; only reference names are resolved in each local scope.

- Root: `version: 4`, `kind: "specification"`, `specs`, `compositions`
- Each spec: `inputs`, `outputs`, `state`, `init`, `invariant`, `operations`, `examples`
- Each composition: `inputs`, `outputs`, `instances`, `examples`. The optional `operations` maps a public operation name to an array of direct child `alias.operation` references
- Each instance: `target` and `connections`. The instance name is the key in `instances`; connections is an object from child port name to parent port name
- Optional implementation: `composition`, `inputs`, `reset_input`, `state`, `reset`, `next`, `operations`, `binding`
- Implementation binding: `states` and `outputs`. states maps a leaf instance path to a state mapping; outputs maps a port name to a state-only expression

The optional keys are the root's `name` / `implementation`, the composition's `operations`, and the implementation's `wires`.
All other keys are required, and an empty map is still written explicitly as `{}` in JSON.
A spec's `operations` maps operation names to Boolean relations; the implementation's map of the same name maps operation names to Boolean selectors.
Each example has `expect`, `initial` and `trace`. Each trace frame has `inputs`, `observe`, and exactly one of
`operation: "left"` or `actions: ["left", "right"]`.
`actions: []` is idle. Output port values use the existing example key `observe`.
The JSON printer keeps the original single-operation/set representation and never reorders arrays.

```
"operations": {"left": ["left.add"], "right": ["right.add"]}
```

Omitting this composition map gives the same-name synchronization shorthand. An empty map or empty group cannot substitute for it.
Duplicate JSON keys, duplicate instance names and undefined targets are rejected.

## Boundary with the old forms and migration

A document with signature-bearing `spec Name(...)` / `composition Name(...)` lowers to version 4.
The old `component Name { ... }`, global input / observation / operation,
and signatureless `composition Name { members compose(...); ... }` remain version 3.
The two forms cannot be mixed in one document. The meaning of old version 2/3 JSON, examples and verification is unchanged.

Migrating from version 3 to version 4 means deciding each specification's interface and connections explicitly.
No semantic transformation mechanically splits global ports or the identity of a shared component.
`scripts/json_to_lyd.py` is a printer that keeps the given schema version; it does not automatically convert v3 to v4.
New declaration words are interpreted contextually, and no global reserved words are added.

```sh
python scripts/json_to_lyd.py examples/scoped_dual_operator.json /tmp/scoped-dual.lyd --infix
cargo run --locked -- /tmp/scoped-dual.lyd --emit-json /tmp/scoped-dual.json --out /tmp/scoped-dual-check
```

The tool version is 0.8 and the JSON schema versions are 2 / 3 / 4; these are separate numbers.
The parser, lowering, instance expansion, type checking, obligation generation and Z3 remain trusted,
and this addition does not formally prove the soundness of the whole tool.

## Implementation response deadlines

`responses` inside `implementation` can state explicit completion deadlines for accepted requests.
See [the meaning and constraints shared by v3/v4](specifications.md#conditional-bounded-response) and
[the example](../examples/scoped_response.lyd). Existing selectors correspond to completion
events, distinct from acceptance. The initial version handles one contract and one outstanding request,
and separately checks satisfiability of input assumptions, request retention, reset cancellation and unsigned countdown.
It adds no guarantee of progress for operations without a contract, or of rescuing unbounded stalls through fairness.
