# Expected relations and finite traces with explicit quantifiers (0.9.1)

An `operation` describes the allowed current/next-state relation, not an execution procedure.
An `example` checks a finite trace of that relation under the quantifiers given for input values and executions.
The two have separate roles, and neither declares a clock event.

## Current and next values

```
spec Counter(input amount: bv<4>, output count: bv<4>) {
  state value: bv<4>;
  init value == 0u4;
  invariant count == value;
  operation add {
    expect value' == value + amount;
  }
}
```

In scoped `spec` relations, a bare state name `value` denotes the current value and `value'` the next state.
Likewise for outputs: `count` is the current value and `count'` the next output. Add exactly one ASCII `'`.
In `invariant` a bare name is still the current value; its meaning does not switch inside an operation.
For the next-state invariant, the checker separately applies the same relation to the next frame.

The input `amount` is the value supplied for this operation; `amount'` is an error.
`value''`, `s.value'`, `(value)'` and the curly quote `value’` are also rejected.
For memories, `memory'[address]` reads the next memory.

The compatibility forms `s.value` / `n.value` / `i.amount` / `o.count` / `no.count` remain available.
Older specs in which a state and a port share a name stay valid with explicit prefixes, but ambiguous bare references are rejected.
For example, if a state `count` and an output `count` coexist, use `s.count` and `o.count`.
`init` / `invariant` may reference only the current state and current outputs; inputs, next state and next outputs are rejected.
This restriction also applies to indirect references through named expectations.
These bare-name and prime rules do not carry over to version 2 machines or to implementation reset/next/binding.

## Named expectations and AND/OR

```
expectation grows { expect value' == value + amount; }
expectation holds { expect value' == value; }
expectation permitted {
  any {
    all { expect amount <= 2u4; expect grows; }
    all { expect amount > 2u4; expect holds; }
  }
}
operation add {
  expect permitted;
  expect count' == value';
}
```

- Conditions listed directly under an `operation` / `expectation` are ANDed
- `all { ... }` is the AND of its children; `any { ... }` is the OR of its children
- Nesting is preserved. The example above requires `(amount <= 2 AND grows) OR (amount > 2 AND holds)`
- A single `expect` can also contain an ordinary Boolean expression such as `grows || holds`
- Each expectation is a Boolean relation. It is not executed as a state update or an ordered procedure
- Empty operation/expectation/all/any blocks are rejected. Write `expect true;` for an intentionally unconstrained relation and `expect false;` for an impossible one
- Omitted states and outputs are not implicitly held. If holding is required, write `expect value' == value;`

Named expectations are usable only within one `spec`, and may be referenced before they are defined.
They have no parameters, mutable bindings, references from other specs, or dynamic macros.
Expectations sharing a name with a state, input or output, duplicate definitions, and cycles or type errors (including in unused ones) are rejected.
If the same spec is used twice, each instance interprets it in terms of its own local state.
The whole dependency graph is checked before expansion, with limits of 64 dependency levels, 64 levels of all/any nesting,
and 262144 nodes of relation expansion per spec. Exceeding a limit is an input error, never a success.

`operation add = relation;` remains as a compatibility form.
Named expectations are expanded into ordinary relations in the frontend and add no separate operations or solver assumptions.
Changes consisting only of primes and expectations do not alter the JSON version 4 schema or the meaning of the action set.
See [expectation_counter.lyd](../examples/expectation_counter.lyd) for an example.

## Writing a single operation readably

```
example every_input {
  forall a: bv<4>;
  execution forall;
  initial { count = 0u4; }
  trace {
    add(amount: a) => count == a;
    add(amount: 1u4) => count == a + 1u4;
  }
}
```

Read `add(amount: a) => count == a;` as "check the right-hand side against the outputs after add is
selected once with this input". The quantifier meaning is fixed by the enclosing `execution` and input quantifiers.
Here `count` is the output of the frame after the arrow; private state cannot be referenced.
The right-hand side is a Boolean expression over output names (`o.count` in version 3) and variables bound by `forall` / `exists`.
With no input arguments, write `add() => predicate;`.
Only when the operation is named `actions` is the block form
`actions { inputs { ... } ensure ...; }` used, to distinguish it from the action-set syntax.
Duplicate input names, undeclared inputs and type mismatches are rejected.

This form is equivalent to the existing single-operation trace block with an `ensure` added:

```
add {
  inputs { amount = a; }
  ensure count == a;
}
```

For simultaneous execution or no selection, use the existing `actions(left, right) { ensure ...; }` / `actions() { ensure ...; }`.
`observe { count = expression; }` is a compatibility form that checks equality with an output;
combined with `ensure`, both are required. `initial` is an assumption imposed on the starting state.
Trace frame order is preserved; two operation calls are not executed simultaneously.

## Quantifying inputs and executions separately

`forall a: bv<4>;` / `exists b: bool;` bind from outermost to innermost in the order written.
The final `execution` quantifies over the finite executions allowed under those values.
Quantified variables are referenced by their declared names, such as `a`, and passed explicitly to the input arguments that need them.
Declaring a quantified variable does not automatically connect it to an input port of the same name.

```
forall a: bv<2>;
exists b: bv<2>;
execution forall;
```

In this order `b` can be chosen depending on `a`.
In the reverse order `exists b; forall a;`, a single `b` that works for every `a` is required.
Variables with the same quantifier may be grouped as `forall { a: bv<2>; flag: bool; }`.
Duplicate names and shadowing are rejected, and input quantifiers must precede execution/expect.
Bound variables are local to their example and do not leak into other examples.
In the value expressions of `initial`, input arguments, `inputs` and `observe`, bare names refer only to that example's bound variables.
In `ensure` and on the right-hand side of an arrow, version 4 output names can also be referenced bare.
If a bound variable and an output share a name, writing that name bare is ambiguous and is an error.
Rename the bound variable, or, if the output is intended, qualify it explicitly as in `o.count`.
Version 3 outputs are still written as `o.count`, so they remain distinguishable even when a bound variable has the same name.
The old form `q.a` is still accepted for compatibility, but ordinary source needs no prefix.
Internal JSON keeps bound references as `q.a` as before, without changing types, quantifier order or SMT meaning.
The JSON printer also emits bare names. In version 4, a bound name that collides with an output is consistently renamed,
in its declaration and its references only, to an unused name (e.g. `count_value`); output references, quantifier order, groups and types are preserved.
Bound names that are reserved words or start with a digit, which are usable only in JSON, are likewise renamed to unused names valid in source.
The types `bool` / `bv<N>` / `mem<A,W>` are available, with up to 256 input quantifier groups.
The implementation does not enumerate the domain into finite values; it passes typed quantifiers to SMT.

Let `H` be the current/next states and outputs together with the omitted input values over all frames.
`R` is the condition on finite executions, made of init/invariant, the initial specification, the operation relations and the specified inputs.
`P` is the condition being checked: the AND of every step's observe/ensure.
**The expected result `P` is not mixed into the premise `R` that selects executions.**

- `execution exists`: `∃H (R ∧ P)`. At least one finite execution meets the expectation
- `execution not_exists`: `¬∃H (R ∧ P)`. No finite execution meets the expectation
- `execution forall`: `(∃H R) ∧ ∀H (R ⇒ P)`. An execution exists, and the expectation holds on every allowed execution

The input quantifier prefix, in written order, scopes over this entire formula.
`forall` does not succeed vacuously on inputs that admit no execution. Feasibility is also reported by a separate query with the expectation removed.
`not_exists` can be true even when no execution is possible at all, so it is not a claim that an execution exists.

The old `expect positive;` keeps the same existence condition as `exists`, and `expect negative;` as `not_exists`.
These can be combined with arrows/ensure without losing the checked conditions.
`execution positive;` / `execution negative;` are not accepted, as an ambiguous new form; use `expect` for the old forms.
An example cannot contain execution/expect twice.
When input quantifiers are specified, the new execution mode must be stated explicitly, even with an empty prefix.
Combining the old positive/negative with `quantifiers` is not allowed.

Unspecified inputs are part of `H`, so `execution exists` may choose a favorable completion.
`execution forall` checks every completion of the executions actually allowed, but does not guarantee
that the operation is possible for every combination of raw inputs. To also require feasibility for all inputs,
quantify the relevant inputs explicitly with `forall` and pass them as arguments.

This version is limited to **an input quantifier prefix followed by a single innermost execution quantifier**.
It does not implement a language with arbitrary alternation of execution and input quantifiers, synthesis of online strategies or causality,
or liveness over infinite executions.
`forall` is also a check of the given finite length, not an inductive proof for arbitrary lengths.
Initial conditions are still treated as assumptions. Whether the specification completely captures the user's intent is a separate question.

## Examples and checking results

- [quantified_counter.lyd](../examples/quantified_counter.lyd): two additions over all 4-bit inputs, wraparound, and a correction value chosen per input
- [quantified_counter_bad_order.lyd](../examples/quantified_counter_bad_order.lyd): fails because the correction value is chosen before the input
- [quantified_counter_impossible.lyd](../examples/quantified_counter_impossible.lyd): forall fails because some input admits no execution, even though the right-hand side is true

```
cargo run --locked -- examples/quantified_counter.lyd --out /tmp/quantified --z3 /path/to/z3
cargo run --locked -- examples/quantified_counter_bad_order.lyd --out /tmp/bad-order --z3 /path/to/z3 # exit 1
cargo run --locked -- examples/quantified_counter_impossible.lyd --out /tmp/impossible --z3 /path/to/z3 # exit 1
```

Quantified formulas are sent directly to Z3, and the original SMT formulas and query results are saved.
Each query has a 10-second timeout. The quantified path does not use the structural UNSAT kernel.
UNKNOWN, abnormal termination, and failure to recheck an obtained witness are never treated as success.
In the report, `valid` shows whether the proposition is true, false or undetermined; `feasibility.holds` shows execution existence with the expectation removed;
and `nonvacuous.holds` shows whether execution existence and the checked condition can be satisfied under the same outer choices.
Do not conflate these: separate existential queries may choose different inputs.
No claim is made that choice functions or strategies outside the quantifiers have been extracted as concrete witnesses.

## JSON and migration

Schema version 3/4 examples gain an optional `quantifiers` array.
Each element is `{"kind":"forall"|"exists","variables":{"name":type,...}}`.
`expect` accepts `exists` / `not_exists` / `forall` in addition to the old `positive` / `negative`.
Each trace frame gains an optional Boolean `ensure`; `inputs` / `observe` remain maps as before.
An explicit empty array `quantifiers: []` can be written in the DSL as `quantifiers {}`.

The JSON printer also handles the old forms and preserves input quantifier order and groups, expression trees and trace order.
It emits one expect per operation and does not flatten existing Boolean trees on its own.
Named expectation names do not survive in canonical JSON, so names are not reconstructed automatically.
Only bare port references in old JSON where a state and a port share a name are explicitly qualified with `i.` / `o.` to resolve the ambiguity.
In that case the conversion is to an equivalent reference spelling, not a round trip to byte-identical JSON.
